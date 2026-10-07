use super::{
    ClockPort, HttpAttempt, HttpPort, ProviderCall, RequestDeadlines, RequestEvent, RequestPolicy,
    SystemClock, respond,
};
use crate::provider::ModelResponse;
use crate::provider::auth::{Credential, InjectedCredential, RequestCredential};
use crate::provider::http::retry::RetryReplay;
use crate::provider::http::wire::{RequestContext, ResponseMode, WireConfig, WireRequest};
use crate::provider::session::ConversationBinding;
use crate::provider::{ModelUsage, ProviderErrorKind, ProviderFailure};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use url::Url;

#[derive(Default)]
struct FakeClock(AtomicU64);

impl ClockPort for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn sleep_ms(&self, delay_ms: u64) {
        self.0.fetch_add(delay_ms, Ordering::SeqCst);
    }
}

struct ScriptedHttp {
    outcomes: Mutex<VecDeque<HttpAttempt>>,
    requests: Mutex<Vec<WireRequest>>,
}

impl ScriptedHttp {
    fn new(outcomes: impl IntoIterator<Item = HttpAttempt>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<WireRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpPort for ScriptedHttp {
    fn execute(&self, request: &WireRequest, _deadlines: RequestDeadlines) -> HttpAttempt {
        self.requests.lock().unwrap().push(request.clone());
        self.outcomes.lock().unwrap().pop_front().unwrap()
    }
}

fn attempt(response: Result<ModelResponse, ProviderFailure>, items: Vec<Value>) -> HttpAttempt {
    HttpAttempt {
        response,
        received_items: items,
        elapsed_ms: 7,
        body_bytes_received: 32,
        sticky_routing_token: None,
    }
}

fn attempt_with_routing_token(
    response: Result<ModelResponse, ProviderFailure>,
    token: &str,
) -> HttpAttempt {
    let mut attempt = attempt(response, Vec::new());
    attempt.sticky_routing_token = Some(token.to_owned());
    attempt
}

fn success(text: &str) -> ModelResponse {
    ModelResponse {
        id: "resp-test".to_owned(),
        text: text.to_owned(),
        tool_calls: Vec::new(),
        usage: ModelUsage::default(),
        raw_items: Vec::new(),
    }
}

fn setup(model: &str, effort: &str) -> (PathBuf, RequestContext, RequestCredential, WireConfig) {
    let root = std::env::temp_dir().join(format!(
        "kogen-http-test-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let binding = ConversationBinding::new(&root, "develop");
    let request = RequestContext::for_conversation(
        &binding,
        model,
        effort,
        "System instructions",
        vec![
            json!({"type":"reasoning","encrypted_content":"ciphertext"}),
            user_item("first turn"),
        ],
    )
    .unwrap();
    let credential = RequestCredential::Injected(InjectedCredential {
        access_token: "fake-token".to_owned(),
        account_id: "fake-account".to_owned(),
        expires_at: i64::MAX,
    });
    let config = WireConfig {
        endpoint_override: Some(Url::parse("https://example.invalid/v1/responses").unwrap()),
        mode: ResponseMode::Injected,
        supports_generation_cap: false,
        user_agent_version: "test".to_owned(),
    };
    (root, request, credential, config)
}

fn options(fallback_on: bool) -> RequestPolicy {
    RequestPolicy {
        role: "builder".to_owned(),
        mode: "build".to_owned(),
        fallback_on,
        fallback_model: "gpt-6.1-sol".to_owned(),
        fallback_effort: "medium".to_owned(),
        wall_budget_ms: Some(100_000),
    }
}

fn user_item(text: &str) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":text}]})
}

fn go(
    request: &mut RequestContext,
    auth: &mut RequestCredential,
    config: &WireConfig,
    policy: &mut RetryReplay,
    options: &RequestPolicy,
    http: &dyn HttpPort,
    clock: &dyn ClockPort,
) -> Result<ProviderCall, super::ProviderCallFailure> {
    respond(
        request, auth, config, policy, options, http, clock, None, None,
    )
}

#[test]
fn retry_reuses_identical_request_and_preserves_null_usage() {
    let (root, mut request, mut auth, config) = setup("gpt-6-luna", "max");
    let http = ScriptedHttp::new([
        attempt(
            Err(ProviderFailure::new(
                ProviderErrorKind::Malformed,
                "bad first response",
            )),
            Vec::new(),
        ),
        attempt(Ok(success("finished")), Vec::new()),
    ]);
    let clock = FakeClock::default();
    let mut policy = RetryReplay::default();
    let outcome = go(
        &mut request,
        &mut auth,
        &config,
        &mut policy,
        &options(false),
        &http,
        &clock,
    )
    .unwrap();
    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(
        requests[0].header("session-id"),
        Some(request.cache_key.as_str())
    );
    assert_eq!(
        requests[0].header("thread-id"),
        Some(request.thread_id.as_str())
    );
    assert_ne!(
        requests[0].header("session-id"),
        requests[0].header("thread-id")
    );
    assert!(
        matches!(outcome.events.as_slice(), [RequestEvent::Retry { reason, resumed: false, delay_ms, .. }] if reason == "provider/malformed" && (1000..=2000).contains(delay_ms))
    );
    let usage = serde_json::to_value(outcome.response.usage).unwrap();
    assert_eq!(usage["input"], Value::Null);
    assert_eq!(usage["cached_input"], Value::Null);
    assert_eq!(
        crate::provider::cache_hit_rate(&[ModelUsage::default()]),
        None
    );
    assert!(clock.now_ms() >= 1000);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sticky_routing_token_is_replayed_only_within_its_conversation() {
    let (root_a, mut request_a, mut auth_a, config) = setup("gpt-6-luna", "medium");
    let (root_b, mut request_b, mut auth_b, config_b) = setup("gpt-6-luna", "medium");
    let http = ScriptedHttp::new([
        attempt_with_routing_token(Ok(success("first A")), "route-A"),
        attempt(Ok(success("second A")), Vec::new()),
        attempt(Ok(success("first B")), Vec::new()),
    ]);
    let clock = FakeClock::default();
    let mut policy_a = RetryReplay::default();
    go(
        &mut request_a,
        &mut auth_a,
        &config,
        &mut policy_a,
        &options(false),
        &http,
        &clock,
    )
    .unwrap();
    let mut policy_a_next = RetryReplay::default();
    go(
        &mut request_a,
        &mut auth_a,
        &config,
        &mut policy_a_next,
        &options(false),
        &http,
        &clock,
    )
    .unwrap();
    let mut policy_b = RetryReplay::default();
    go(
        &mut request_b,
        &mut auth_b,
        &config_b,
        &mut policy_b,
        &options(false),
        &http,
        &clock,
    )
    .unwrap();

    let requests = http.requests();
    assert_eq!(requests[0].header("x-codex-turn-state"), None);
    assert_eq!(requests[1].header("x-codex-turn-state"), Some("route-A"));
    assert_eq!(requests[2].header("x-codex-turn-state"), None);
    assert_eq!(request_a.sticky_routing_token.as_deref(), Some("route-A"));
    assert_eq!(request_b.sticky_routing_token, None);
    std::fs::remove_dir_all(root_a).unwrap();
    std::fs::remove_dir_all(root_b).unwrap();
}

#[test]
fn overload_switch_keeps_cache_and_thread_and_drops_encrypted_reasoning() {
    let (root, mut request, mut auth, config) = setup("gpt-6-luna", "max");
    let overload = || {
        attempt(
            Err(ProviderFailure::new(
                ProviderErrorKind::Overload,
                "overloaded",
            )),
            Vec::new(),
        )
    };
    let http = ScriptedHttp::new([
        overload(),
        overload(),
        attempt(Ok(success("ok")), Vec::new()),
    ]);
    let clock = FakeClock::default();
    let mut policy = RetryReplay::default();
    let outcome = go(
        &mut request,
        &mut auth,
        &config,
        &mut policy,
        &options(true),
        &http,
        &clock,
    )
    .unwrap();
    let requests = http.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(
        requests[0].header("session-id"),
        requests[2].header("session-id")
    );
    assert_eq!(
        requests[0].header("thread-id"),
        requests[2].header("thread-id")
    );
    let fallback_body = std::str::from_utf8(&requests[2].body).unwrap();
    assert!(fallback_body.contains("gpt-6.1-sol"));
    assert!(!fallback_body.contains("ciphertext"));
    assert_eq!(request.model, "gpt-6.1-sol");
    assert!(
        matches!(outcome.events.as_slice(), [RequestEvent::Retry { reason, .. }, RequestEvent::Switch { from_model, to_model }] if reason == "provider/overload" && from_model == "gpt-6-luna/max" && to_model == "gpt-6.1-sol/medium")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn partial_stream_continuation_appends_received_items_and_returns_combined_text() {
    let (root, mut request, mut auth, config) = setup("gpt-6-luna", "max");
    let partial = json!({
        "type":"message",
        "content":[{"type":"output_text","text":"partial"}]
    });
    let http = ScriptedHttp::new([
        attempt(
            Err(ProviderFailure::new(
                ProviderErrorKind::Timeout,
                "timed out",
            )),
            vec![partial.clone()],
        ),
        attempt(
            Err(ProviderFailure::new(
                ProviderErrorKind::Transport,
                "connection dropped",
            )),
            Vec::new(),
        ),
        attempt(Ok(success("done")), Vec::new()),
    ]);
    let clock = FakeClock::default();
    let mut policy = RetryReplay::default();
    let outcome = go(
        &mut request,
        &mut auth,
        &config,
        &mut policy,
        &options(false),
        &http,
        &clock,
    )
    .unwrap();
    let requests = http.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].body, requests[2].body);
    assert_eq!(outcome.response.text, "partialdone");
    assert_eq!(outcome.response.raw_items[0], partial);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let input = second["input"].as_array().unwrap();
    assert_eq!(input.len(), 4);
    assert_eq!(input[2], partial);
    assert_eq!(input[3]["role"], "user");
    assert!(
        input[3]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Continue the same turn")
    );
    assert!(matches!(
        outcome.events.as_slice(),
        [
            RequestEvent::Retry { resumed: true, .. },
            RequestEvent::Retry { resumed: true, .. }
        ]
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn usage_limit_uses_the_fixed_build_pause_outside_the_build_budget() {
    let (root, mut request, _injected_auth, mut config) = setup("gpt-6-luna", "max");
    config.mode = ResponseMode::Owned;
    let mut auth = RequestCredential::Owned(Credential {
        client_id: "client".to_owned(),
        access_token: "fake-token".to_owned(),
        refresh_token: "refresh-token".to_owned(),
        id_token: String::new(),
        expires_at: i64::MAX,
        scopes: Vec::new(),
        subject: "subject".to_owned(),
        email: None,
        host_id: "host".to_owned(),
    });
    let mut failure = ProviderFailure::new(
        ProviderErrorKind::UsageLimit,
        "ChatGPT subscription usage limit reached.",
    );
    failure.retry_after_ms = Some(30_000);
    let http = ScriptedHttp::new([attempt(Err(failure), Vec::new())]);
    let clock = FakeClock::default();
    let mut policy = RetryReplay::default();
    let result = go(
        &mut request,
        &mut auth,
        &config,
        &mut policy,
        &options(false),
        &http,
        &clock,
    );
    let Err(failure) = result else {
        panic!("usage limit must be returned to the Build after its pause");
    };
    assert_eq!(failure.events.len(), 1);
    assert_eq!(
        failure.events[0],
        RequestEvent::Wait {
            reason: "provider/usage_limit".to_owned(),
            wait_ms: 300_000,
            paused_ms: 300_000,
            budget_paused: true,
        }
    );
    assert_eq!(policy.waited, 300_000);
    assert_eq!(clock.now_ms(), 300_000);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn injected_login_failure_stops_without_refresh_retry_or_pause() {
    let (root, mut request, mut auth, config) = setup("gpt-6-luna", "max");
    let http = ScriptedHttp::new([attempt(
        Err(ProviderFailure::new(
            ProviderErrorKind::Login,
            "unauthorized",
        )),
        Vec::new(),
    )]);
    let clock = FakeClock::default();
    let mut policy = RetryReplay::default();
    let result = go(
        &mut request,
        &mut auth,
        &config,
        &mut policy,
        &options(false),
        &http,
        &clock,
    );
    let failure = match result {
        Err(failure) => failure,
        Ok(_) => panic!("injected credentials cannot be refreshed by the provider client"),
    };

    assert_eq!(failure.failure.kind, ProviderErrorKind::Login);
    assert_eq!(http.requests().len(), 1);
    assert!(failure.events.is_empty());
    assert_eq!(policy.decision, "stop");
    assert_eq!(clock.now_ms(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn response_deadlines_match_provider_contract() {
    let deadlines = RequestDeadlines::from_environment();
    assert_eq!(deadlines.first_byte.as_millis(), 120_000);
    assert_eq!(deadlines.idle.as_millis(), 90_000);
    assert_eq!(deadlines.total.as_millis(), 1_200_000);
}

#[test]
fn system_clock_starts_at_zero_and_accepts_zero_wait() {
    let clock = SystemClock::default();
    assert!(clock.now_ms() < 1000);
    clock.sleep_ms(0);
}

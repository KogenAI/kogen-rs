use super::{ClockPort, RequestEvent, RequestPolicy, ReqwestPort, respond};
use crate::provider::auth::{GrokCredential, RequestCredential};
use crate::provider::http::retry::RetryReplay;
use crate::provider::http::wire::{RequestContext, ResponseMode, WireConfig};
use crate::provider::session::ConversationBinding;
use serde_json::{Value, json};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use url::Url;

#[derive(Default)]
struct NoWaitClock(AtomicU64);

impl ClockPort for NoWaitClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn sleep_ms(&self, _delay_ms: u64) {}
}

#[test]
fn grok_retry_and_cache_prefix_use_the_production_http_and_wire_path() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/responses", server.local_addr().unwrap());
    let server_thread = thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..3 {
            let (mut stream, _) = server.accept().unwrap();
            requests.push(read_request(&mut stream));
            if index == 0 {
                write_response(
                    &mut stream,
                    503,
                    "Service Unavailable",
                    "application/json",
                    br#"{"error":"server_is_overloaded"}"#,
                );
            } else {
                let id = format!("grok-{index}");
                let body = format!(
                    "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"{id}\",\"status\":\"completed\",\"output\":[],\"usage\":{{\"input_tokens\":20,\"output_tokens\":4,\"input_tokens_details\":{{\"cached_tokens\":5}}}}}}}}\n\ndata: [DONE]\n\n"
                );
                write_response(&mut stream, 200, "OK", "text/event-stream", body.as_bytes());
            }
        }
        requests
    });

    let root = scratch_dir();
    let binding = ConversationBinding::new(&root, "develop");
    let mut request = RequestContext::for_conversation(
        &binding,
        "grok-4.6",
        "high",
        "System instructions",
        vec![user_item("first turn")],
    )
    .unwrap();
    request.tools = vec![json!({"type":"function","name":"shell","parameters":{}})];
    request.callable_tools = vec!["shell".to_owned()];
    let mut credential = RequestCredential::Grok(GrokCredential {
        access_token: "fake-grok-access".to_owned(),
        refresh_token: "fake-grok-refresh".to_owned(),
        expires_at: i64::MAX,
        scopes: Vec::new(),
        email: None,
        client_id: "fake-client".to_owned(),
        token_endpoint: "https://auth.x.ai/token".to_owned(),
    });
    let config = WireConfig {
        endpoint_override: Some(Url::parse(&endpoint).unwrap()),
        mode: ResponseMode::Grok,
        supports_generation_cap: false,
        user_agent_version: "0.1.0".to_owned(),
    };
    let options = RequestPolicy {
        role: "builder".to_owned(),
        mode: "build".to_owned(),
        fallback_on: true,
        fallback_model: "gpt-6.1-sol".to_owned(),
        fallback_effort: "medium".to_owned(),
        wall_budget_ms: Some(30_000),
    };
    let clock = NoWaitClock::default();
    let http = ReqwestPort::new().unwrap();
    let mut retry = RetryReplay::default();

    let first = respond(
        &mut request,
        &mut credential,
        &config,
        &mut retry,
        &options,
        &http,
        &clock,
        None,
        None,
    )
    .unwrap();
    assert_eq!(first.attempts.len(), 2);
    assert!(matches!(
        first.events.as_slice(),
        [RequestEvent::Retry { .. }]
    ));
    assert_eq!(first.attempts[0].body, first.attempts[1].body);
    assert_eq!(first.attempts[0].mode, ResponseMode::Grok);

    request.input.push(user_item("second turn"));
    let second = respond(
        &mut request,
        &mut credential,
        &config,
        &mut retry,
        &options,
        &http,
        &clock,
        None,
        None,
    )
    .unwrap();
    assert_eq!(second.attempts.len(), 1);
    assert!(
        second.attempts[0]
            .body
            .starts_with(&first.attempts[0].body[..first.attempts[0].body.len() - 2])
    );
    assert!(first.attempts[0].body.ends_with(b"]}"));

    let captured = server_thread.join().unwrap();
    assert_eq!(captured.len(), 3);
    for (headers, body) in &captured {
        assert!(headers.contains("x-xai-token-auth: xai-grok-cli"));
        assert!(headers.contains("x-authenticateresponse: authenticate-response"));
        assert!(headers.contains("x-grok-model-override: grok-4.6"));
        assert!(headers.contains("x-grok-client-identifier: kogen"));
        assert!(headers.contains("x-grok-client-mode: headless"));
        assert!(headers.contains("user-agent: kogen/0.1.0"));
        assert!(headers.contains(&format!("x-grok-conv-id: {}", request.cache_key)));
        assert!(headers.contains(&format!("x-grok-session-id: {}", request.cache_key)));
        assert!(headers.contains("x-grok-req-id: "));
        let value: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value["model"], "grok-4.6");
        assert_eq!(value["reasoning"], json!({"effort":"high"}));
        assert_eq!(value["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(value["store"], false);
        assert_eq!(value["stream"], true);
        assert!(value.get("reasoning").unwrap().get("summary").is_none());
        assert!(value.get("previous_response_id").is_none());
    }
    let request_ids = captured
        .iter()
        .map(|(headers, _)| header_value(headers, "x-grok-req-id"))
        .collect::<Vec<_>>();
    assert!(request_ids.iter().all(|id| is_uuid_v4(id)));
    assert_ne!(request_ids[0], request_ids[1]);
    assert_eq!(first.response.usage.input, Some(15));
    assert_eq!(first.response.usage.cached_input, Some(5));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn grok_mode_defaults_empty_model_and_effort_and_omits_empty_cache_headers() {
    let root = scratch_dir();
    let binding = ConversationBinding::new(&root, "develop");
    let mut request = RequestContext::for_conversation(&binding, "", "", "I", Vec::new()).unwrap();
    request.cache_key.clear();
    request.thread_id.clear();
    let credential = RequestCredential::Grok(GrokCredential {
        access_token: "token".to_owned(),
        refresh_token: "refresh".to_owned(),
        expires_at: i64::MAX,
        scopes: Vec::new(),
        email: None,
        client_id: "client".to_owned(),
        token_endpoint: "https://auth.x.ai/token".to_owned(),
    });
    let config = WireConfig {
        endpoint_override: None,
        mode: ResponseMode::Grok,
        supports_generation_cap: false,
        user_agent_version: "test".to_owned(),
    };
    let wire =
        crate::provider::http::wire::build_wire_request(&request, &credential, &config).unwrap();
    assert_eq!(
        wire.endpoint.as_str(),
        "https://cli-chat-proxy.grok.com/v1/responses"
    );
    assert_eq!(wire.header("x-grok-conv-id"), None);
    assert_eq!(wire.header("x-grok-session-id"), None);
    let body: Value = serde_json::from_slice(&wire.body).unwrap();
    assert_eq!(body["model"], "grok-4.6");
    assert_eq!(body["reasoning"]["effort"], "high");
    assert!(body.get("prompt_cache_key").is_none());
    std::fs::remove_dir_all(root).unwrap();
}

fn scratch_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kogen-grok-http-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn user_item(text: &str) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":text}]})
}

fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut header_bytes = Vec::new();
    while !header_bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).unwrap();
        header_bytes.push(byte[0]);
    }
    let headers = String::from_utf8(header_bytes)
        .unwrap()
        .to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (headers, body)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}

fn header_value(headers: &str, name: &str) -> String {
    headers
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}: ")))
        .unwrap()
        .trim()
        .to_owned()
}

fn is_uuid_v4(value: &str) -> bool {
    value.len() == 36
        && value.as_bytes()[14] == b'4'
        && matches!(value.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23) && byte == b'-' || byte.is_ascii_hexdigit()
        })
}

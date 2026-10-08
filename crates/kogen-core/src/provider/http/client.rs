//! Injectable HTTP attempts and production retry execution.

mod transport;

pub use transport::ReqwestPort;

use super::retry::RetryReplay;
use super::wire::{RequestContext, WireConfig, WireRequest, build_wire_request};
use crate::provider::auth::{self, RequestCredential};
use crate::provider::session::ConversationHistory;
use crate::provider::sse::{StreamLimitExceeded, StreamOutputLimits};
use crate::provider::{ModelResponse, ModelUsage, ProviderFailure};
use rand::Rng as _;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestDeadlines {
    pub first_byte: Duration,
    pub idle: Duration,
    pub total: Duration,
    pub unscaled_idle_ms: u64,
}

impl RequestDeadlines {
    #[must_use]
    pub fn from_environment() -> Self {
        let scale = time_scale();
        Self {
            first_byte: scaled_duration(120_000, scale),
            idle: scaled_duration(90_000, scale),
            total: scaled_duration(1_200_000, scale),
            unscaled_idle_ms: 90_000,
        }
    }
}

fn scaled_duration(ms: u64, scale: f64) -> Duration {
    Duration::from_millis((ms as f64 * scale).floor().max(1.0) as u64)
}

fn time_scale() -> f64 {
    std::env::var("KOGEN_TIME_SCALE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(1.0)
}

#[derive(Clone)]
pub struct HttpAttempt {
    pub response: Result<ModelResponse, ProviderFailure>,
    pub received_items: Vec<serde_json::Value>,
    /// Milliseconds from dispatch until the first nonempty response chunk.
    pub first_byte_ms: Option<u64>,
    pub elapsed_ms: u64,
    pub body_bytes_received: u64,
    /// HTTP status, even when the response stream later fails.
    pub status_code: Option<u16>,
    /// Provider usage before normalization, retained on partial/error streams.
    pub raw_usage: Option<serde_json::Value>,
    /// Provider-reported model, when the response stream supplied one.
    pub response_model: Option<String>,
    pub sticky_routing_token: Option<String>,
}

#[derive(Clone)]
pub struct LimitedHttpAttempt {
    pub attempt: HttpAttempt,
    pub limit_exceeded: Option<StreamLimitExceeded>,
}

pub trait HttpPort: Send + Sync {
    fn execute(&self, request: &WireRequest, deadlines: RequestDeadlines) -> HttpAttempt;

    /// Execute once and stop a streaming response when the supplied output
    /// thresholds are crossed. Non-streaming test ports receive a conservative
    /// post-response check; streaming transports should override this method
    /// and cancel their body stream immediately.
    fn execute_with_output_limits(
        &self,
        request: &WireRequest,
        deadlines: RequestDeadlines,
        limits: StreamOutputLimits,
    ) -> LimitedHttpAttempt {
        let attempt = self.execute(request, deadlines);
        let usage = attempt.raw_usage.as_ref();
        let limit_exceeded = usage.and_then(|usage| {
            let output = usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64);
            let reasoning = usage
                .get("output_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(serde_json::Value::as_u64);
            if output.is_some_and(|count| count >= limits.hard_budget_output_tokens) {
                Some(StreamLimitExceeded::GlobalBudget)
            } else if output.is_some_and(|count| count > limits.output_tokens) {
                Some(StreamLimitExceeded::OutputTokens)
            } else if reasoning.is_some_and(|count| count > limits.reasoning_tokens) {
                Some(StreamLimitExceeded::ReasoningTokens)
            } else {
                None
            }
        });
        LimitedHttpAttempt {
            attempt,
            limit_exceeded,
        }
    }
}

pub trait ClockPort: Send + Sync {
    fn now_ms(&self) -> u64;
    fn sleep_ms(&self, delay_ms: u64);
}

#[derive(Clone, Debug)]
pub struct SystemClock {
    started: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl ClockPort for SystemClock {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    fn sleep_ms(&self, delay_ms: u64) {
        std::thread::sleep(scaled_duration(delay_ms, time_scale()));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestEvent {
    Retry {
        reason: String,
        delay_ms: u64,
        resumed: bool,
    },
    Switch {
        from_model: String,
        to_model: String,
    },
    Wait {
        reason: String,
        wait_ms: u64,
        paused_ms: u64,
        budget_paused: bool,
    },
}

#[derive(Debug)]
pub struct ProviderCallFailure {
    pub failure: Box<ProviderFailure>,
    pub events: Vec<RequestEvent>,
    pub attempts: Vec<WireRequest>,
    pub usages: Vec<ModelUsage>,
}

pub struct ProviderCall {
    pub response: ModelResponse,
    pub events: Vec<RequestEvent>,
    pub attempts: Vec<WireRequest>,
    pub usages: Vec<ModelUsage>,
}

#[derive(Clone, Debug)]
pub struct RequestPolicy {
    pub role: String,
    pub mode: String,
    pub fallback_on: bool,
    pub fallback_model: String,
    pub fallback_effort: String,
    pub wall_budget_ms: Option<u64>,
}

/// Execute one model call. The caller owns retry state so wait caps survive
/// stage pauses; fake providers inject attempts and clock results at this port.
#[allow(clippy::too_many_arguments)]
pub fn respond(
    request: &mut RequestContext,
    auth: &mut RequestCredential,
    wire_config: &WireConfig,
    policy: &mut RetryReplay,
    options: &RequestPolicy,
    http: &dyn HttpPort,
    clock: &dyn ClockPort,
    home: Option<&Path>,
    account_label: Option<&str>,
) -> Result<ProviderCall, ProviderCallFailure> {
    if wire_config.mode == super::wire::ResponseMode::Grok {
        if request.model.is_empty() {
            request.model = crate::provider::grok::DEFAULT_MODEL.to_owned();
        }
        if request.effort.is_empty() {
            request.effort = crate::provider::grok::DEFAULT_EFFORT.to_owned();
        }
    }
    let mut events = Vec::new();
    let mut attempts = Vec::new();
    let mut usages = Vec::new();
    let mut spent = 0_u64;
    let open = serde_json::json!({
        "role": options.role,
        "model": model_class(&request.model),
        "fallbackOn": options.fallback_on && wire_config.mode != super::wire::ResponseMode::Grok,
        "refreshable": matches!(
            wire_config.mode,
            super::wire::ResponseMode::Owned | super::wire::ResponseMode::Grok
        ),
        "bounded": options.wall_budget_ms.is_some(),
        "wall": options.wall_budget_ms.unwrap_or(0),
        "mode": options.mode,
    });
    policy.apply("Open", Some(&open));
    let mut continued_text = String::new();
    let mut continued_items = Vec::new();
    loop {
        let wire = match build_wire_request(request, auth, wire_config) {
            Ok(wire) => wire,
            Err(failure) => return Err(call_error(failure, events, attempts, usages)),
        };
        attempts.push(wire.clone());
        let request_start = clock.now_ms();
        let attempt = http.execute(&wire, RequestDeadlines::from_environment());
        if wire.mode != super::wire::ResponseMode::Grok && request.sticky_routing_token.is_none() {
            request.sticky_routing_token = attempt
                .sticky_routing_token
                .as_ref()
                .filter(|token| !token.is_empty())
                .cloned();
        }
        let elapsed = attempt
            .elapsed_ms
            .max(clock.now_ms().saturating_sub(request_start));
        spent = spent.saturating_add(elapsed);
        usages.push(match &attempt.response {
            Ok(response) => response.usage.clone(),
            Err(failure) => failure.usage.as_deref().cloned().unwrap_or_default(),
        });
        match attempt.response {
            Ok(response) => {
                update_wall(policy, options.wall_budget_ms, spent);
                policy.apply(
                    "Result",
                    Some(&serde_json::json!({"kind":"ok", "items":!response.raw_items.is_empty()})),
                );
                let mut response = response;
                response.text = format!("{continued_text}{}", response.text);
                continued_items.append(&mut response.raw_items);
                response.raw_items = continued_items;
                return Ok(ProviderCall {
                    response,
                    events,
                    attempts,
                    usages,
                });
            }
            Err(failure) => {
                let mut refresh_failure = None;
                let has_items = !attempt.received_items.is_empty();
                let result = serde_json::json!({
                    "kind": failure.kind.as_str(),
                    "items": has_items,
                    "retryAfterMs": failure.retry_after_ms,
                });
                update_wall(policy, options.wall_budget_ms, spent);
                policy.apply("Result", Some(&result));
                if policy.decision == "refresh" {
                    if let (Some(home), Some(label)) = (home, account_label) {
                        match auth::refresh_request_after_401(home, label, auth) {
                            Ok(Some(credential)) => {
                                *auth = credential;
                                continue;
                            }
                            Ok(None) => policy.apply("Result", Some(&result)),
                            Err(refresh_error) => {
                                refresh_failure = Some(refresh_error);
                                policy.apply("Result", Some(&result));
                            }
                        }
                    } else {
                        policy.apply("Result", Some(&result));
                    }
                }
                if policy.decision == "pause" {
                    let wait_ms = policy.delay;
                    let wait_started = clock.now_ms();
                    clock.sleep_ms(wait_ms);
                    events.push(RequestEvent::Wait {
                        reason: policy.reason.clone(),
                        wait_ms,
                        paused_ms: clock.now_ms().saturating_sub(wait_started),
                        budget_paused: true,
                    });
                    return Err(call_error(
                        refresh_failure.unwrap_or(failure),
                        events,
                        attempts,
                        usages,
                    ));
                }
                if policy.phase == "stopped" || policy.decision == "incomplete" {
                    return Err(call_error(
                        refresh_failure.unwrap_or(failure),
                        events,
                        attempts,
                        usages,
                    ));
                }
                if policy.decision == "switch" {
                    let from_model = format!("{}/{}", request.model, request.effort);
                    request.model.clone_from(&options.fallback_model);
                    request.effort.clone_from(&options.fallback_effort);
                    let to_model = format!("{}/{}", request.model, request.effort);
                    let mut history = ConversationHistory::new(std::mem::take(&mut request.input));
                    history.discard_encrypted_reasoning();
                    request.input = history.items().to_vec();
                    events.push(RequestEvent::Switch {
                        from_model,
                        to_model,
                    });
                    continue;
                }
                if policy.decision == "retry" {
                    let ceiling = policy.delay;
                    let delay = if ceiling == 0 {
                        0
                    } else {
                        rand::thread_rng().gen_range((ceiling / 2)..=ceiling)
                    };
                    let resumed = !continued_items.is_empty() || policy.continued;
                    events.push(RequestEvent::Retry {
                        reason: policy.reason.clone(),
                        delay_ms: delay,
                        resumed,
                    });
                    if policy.continued {
                        continued_text.push_str(&text_from_items(&attempt.received_items));
                        continued_items.extend(attempt.received_items.iter().cloned());
                        let mut history =
                            ConversationHistory::new(std::mem::take(&mut request.input));
                        history.append_continuation(attempt.received_items);
                        request.input = history.items().to_vec();
                    }
                    clock.sleep_ms(delay);
                    continue;
                }
                return Err(call_error(failure, events, attempts, usages));
            }
        }
    }
}

fn update_wall(policy: &mut RetryReplay, budget: Option<u64>, spent: u64) {
    if let Some(budget) = budget {
        policy.wall = budget.saturating_sub(spent);
    }
}

fn model_class(model: &str) -> &'static str {
    if model.starts_with("grok-") {
        "grok"
    } else if model == "gpt-6.1-sol" {
        "sol"
    } else {
        "luna"
    }
}

fn text_from_items(items: &[serde_json::Value]) -> String {
    let mut text = String::new();
    for item in items {
        if item.get("type").and_then(serde_json::Value::as_str) == Some("message")
            && let Some(content) = item.get("content").and_then(serde_json::Value::as_array)
        {
            for part in content {
                if part.get("type").and_then(serde_json::Value::as_str) == Some("output_text")
                    && let Some(part) = part.get("text").and_then(serde_json::Value::as_str)
                {
                    text.push_str(part);
                }
            }
        }
    }
    text
}

fn call_error(
    failure: ProviderFailure,
    events: Vec<RequestEvent>,
    attempts: Vec<WireRequest>,
    usages: Vec<ModelUsage>,
) -> ProviderCallFailure {
    ProviderCallFailure {
        failure: Box::new(failure),
        events,
        attempts,
        usages,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "client/grok_tests.rs"]
mod grok_tests;

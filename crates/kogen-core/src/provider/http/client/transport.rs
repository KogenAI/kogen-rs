//! Tokio-backed streaming HTTP effect for the Responses client.

use super::{HttpAttempt, HttpPort, RequestDeadlines};
use crate::provider::http::wire::{ResponseMode, WireRequest};
use crate::provider::sse::{MAX_RESPONSE_BYTES, SseAssembler};
use crate::provider::{ProviderErrorKind, ProviderFailure};
use futures_util::StreamExt as _;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::sync::Arc;
use tokio::runtime::{Builder, Runtime};
use tokio::time::{Instant as TokioInstant, timeout_at};

pub struct ReqwestPort {
    client: reqwest::Client,
    runtime: Arc<Runtime>,
}

impl ReqwestPort {
    pub fn new() -> Result<Self, ProviderFailure> {
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|_| transport_failure(ResponseMode::Owned))?;
        let client = reqwest::Client::builder()
            .read_timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|_| transport_failure(ResponseMode::Owned))?;
        Ok(Self {
            client,
            runtime: Arc::new(runtime),
        })
    }
}

impl HttpPort for ReqwestPort {
    fn execute(&self, request: &WireRequest, deadlines: RequestDeadlines) -> HttpAttempt {
        self.runtime
            .block_on(execute_async(&self.client, request, deadlines))
    }
}

async fn execute_async(
    client: &reqwest::Client,
    request: &WireRequest,
    deadlines: RequestDeadlines,
) -> HttpAttempt {
    let start = TokioInstant::now();
    let total_deadline = start + deadlines.total;
    let first_deadline = std::cmp::min(start + deadlines.first_byte, total_deadline);
    let mut headers = HeaderMap::new();
    for (name, value) in &request.headers {
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) else {
            return failed_attempt(
                ProviderFailure::new(
                    ProviderErrorKind::Malformed,
                    if request.mode == ResponseMode::Grok {
                        "Could not encode Grok request."
                    } else {
                        "Could not encode ChatGPT request."
                    },
                ),
                start,
                Vec::new(),
                0,
            );
        };
        headers.insert(name, value);
    }
    let sent = timeout_at(
        first_deadline,
        client
            .post(request.endpoint.clone())
            .headers(headers)
            .body(request.body.clone())
            .send(),
    )
    .await;
    let response = match sent {
        Ok(Ok(response)) => response,
        Ok(Err(error)) if error.is_timeout() => {
            return failed_attempt(timeout_failure(request.mode), start, Vec::new(), 0);
        }
        Ok(Err(_)) => return failed_attempt(transport_failure(request.mode), start, Vec::new(), 0),
        Err(_) => return failed_attempt(timeout_failure(request.mode), start, Vec::new(), 0),
    };
    let sticky_routing_token = if request.mode == ResponseMode::Grok {
        None
    } else {
        response
            .headers()
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after);
    let mut stream = response.bytes_stream();
    let mut body_bytes = 0_u64;
    let mut seen_body = false;
    let mut last_byte = TokioInstant::now();
    let mut parser = SseAssembler::new();
    let mut error_body = Vec::new();
    let mut error_body_too_large = false;
    loop {
        let idle_deadline = if seen_body {
            last_byte + deadlines.idle
        } else {
            first_deadline
        };
        let deadline = std::cmp::min(total_deadline, idle_deadline);
        match timeout_at(deadline, stream.next()).await {
            Err(_) => {
                let failure = if TokioInstant::now() >= total_deadline || !seen_body {
                    timeout_failure(request.mode)
                } else {
                    ProviderFailure::new(
                        ProviderErrorKind::Stall,
                        format!(
                            "Provider stream sent nothing for {} s after it started.",
                            deadlines.unscaled_idle_ms / 1000
                        ),
                    )
                };
                return failed_attempt_with_routing_token(
                    failure,
                    start,
                    parser.collected_items().to_vec(),
                    body_bytes,
                    sticky_routing_token.clone(),
                );
            }
            Ok(Some(Err(error))) => {
                let failure = if error.is_timeout() && seen_body {
                    ProviderFailure::new(
                        ProviderErrorKind::Stall,
                        format!(
                            "Provider stream sent nothing for {} s after it started.",
                            deadlines.unscaled_idle_ms / 1000
                        ),
                    )
                } else if error.is_timeout() {
                    timeout_failure(request.mode)
                } else {
                    transport_failure(request.mode)
                };
                return failed_attempt_with_routing_token(
                    failure,
                    start,
                    parser.collected_items().to_vec(),
                    body_bytes,
                    sticky_routing_token.clone(),
                );
            }
            Ok(None) => break,
            Ok(Some(Ok(chunk))) => {
                if chunk.is_empty() {
                    continue;
                }
                seen_body = true;
                last_byte = TokioInstant::now();
                body_bytes = body_bytes.saturating_add(chunk.len() as u64);
                if (200..300).contains(&status) {
                    if parser.feed(&chunk).is_err() {
                        let failure = if request.mode == ResponseMode::Grok
                            && body_bytes as usize > MAX_RESPONSE_BYTES
                        {
                            size_limit_failure()
                        } else {
                            normalize_failure(
                                parser.finish().err().unwrap_or_else(malformed_failure),
                                request.mode,
                            )
                        };
                        return failed_attempt_with_routing_token(
                            failure,
                            start,
                            parser.collected_items().to_vec(),
                            body_bytes,
                            sticky_routing_token.clone(),
                        );
                    }
                } else {
                    if error_body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                        error_body_too_large = true;
                        break;
                    }
                    error_body.extend_from_slice(&chunk);
                }
            }
        }
    }
    if !(200..300).contains(&status) {
        let failure = if error_body_too_large && request.mode == ResponseMode::Grok {
            size_limit_failure()
        } else {
            classify_http_error(status, &error_body, retry_after, request.mode)
        };
        return failed_attempt_with_routing_token(
            failure,
            start,
            Vec::new(),
            body_bytes,
            sticky_routing_token,
        );
    }
    match parser.finish() {
        Ok(response) => HttpAttempt {
            received_items: response.raw_items.clone(),
            response: Ok(response),
            elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            body_bytes_received: body_bytes,
            sticky_routing_token,
        },
        Err(failure) => failed_attempt_with_routing_token(
            normalize_failure(failure, request.mode),
            start,
            parser.collected_items().to_vec(),
            body_bytes,
            sticky_routing_token,
        ),
    }
}

fn failed_attempt(
    failure: ProviderFailure,
    start: TokioInstant,
    received_items: Vec<serde_json::Value>,
    body_bytes: u64,
) -> HttpAttempt {
    HttpAttempt {
        response: Err(failure),
        received_items,
        elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        body_bytes_received: body_bytes,
        sticky_routing_token: None,
    }
}

fn failed_attempt_with_routing_token(
    failure: ProviderFailure,
    start: TokioInstant,
    received_items: Vec<serde_json::Value>,
    body_bytes: u64,
    sticky_routing_token: Option<String>,
) -> HttpAttempt {
    let mut attempt = failed_attempt(failure, start, received_items, body_bytes);
    attempt.sticky_routing_token = sticky_routing_token;
    attempt
}

fn classify_http_error(
    status: u16,
    body: &[u8],
    retry_after: Option<u64>,
    mode: ResponseMode,
) -> ProviderFailure {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    if mode == ResponseMode::Grok {
        let mut failure = classify_grok_http_error(status, &text);
        if matches!(status, 429 | 503) {
            failure.retry_after_ms = retry_after.or_else(|| retry_after_from_body(body));
        }
        return failure;
    }
    let mut failure = if status == 401 || status == 403 {
        ProviderFailure::new(
            ProviderErrorKind::Login,
            "ChatGPT rejected the login; sign in again.",
        )
    } else if status == 429 || text.contains("usage_limit") || text.contains("usage limit") {
        ProviderFailure::new(
            ProviderErrorKind::UsageLimit,
            "ChatGPT subscription usage limit reached. Manage usage: https://chatgpt.com/settings/usage",
        )
    } else if (500..600).contains(&status) || text.contains("overload") {
        ProviderFailure::new(
            ProviderErrorKind::Overload,
            "ChatGPT service is temporarily overloaded.",
        )
    } else {
        ProviderFailure::new(
            ProviderErrorKind::Malformed,
            format!("ChatGPT rejected the request (HTTP {status})."),
        )
    };
    if matches!(status, 429 | 503) {
        failure.retry_after_ms = retry_after.or_else(|| retry_after_from_body(body));
    }
    failure
}

fn classify_grok_http_error(status: u16, body: &str) -> ProviderFailure {
    let usage_limit = ["usage_limit", "usage limit", "quota exceeded", "rate limit"]
        .iter()
        .any(|needle| body.contains(needle));
    let overloaded = ["server_is_overloaded", "overloaded", "overload"]
        .iter()
        .any(|needle| body.contains(needle));
    if status == 401 {
        ProviderFailure::new(
            ProviderErrorKind::Login,
            "Grok rejected this session; run `kogen provider login grok`.",
        )
    } else if status == 403 {
        ProviderFailure::new(
            ProviderErrorKind::Login,
            "This Grok account cannot access the requested model.",
        )
    } else if status == 429 || usage_limit {
        ProviderFailure::new(
            ProviderErrorKind::UsageLimit,
            "Grok subscription usage limit reached.",
        )
    } else if (500..600).contains(&status) || overloaded {
        ProviderFailure::new(
            ProviderErrorKind::Overload,
            "Grok service is temporarily overloaded.",
        )
    } else {
        ProviderFailure::new(
            ProviderErrorKind::Malformed,
            format!("Grok rejected the request (HTTP {status})."),
        )
    }
}

fn parse_retry_after(value: &str) -> Option<u64> {
    let seconds = value.parse::<f64>().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1000.0).round() as u64)
}

fn retry_after_from_body(body: &[u8]) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let seconds = value.get("resets_in_seconds")?.as_f64()?;
    (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1000.0).round() as u64)
}

fn timeout_failure(mode: ResponseMode) -> ProviderFailure {
    ProviderFailure::new(
        ProviderErrorKind::Timeout,
        if mode == ResponseMode::Grok {
            "Grok request timed out."
        } else {
            "ChatGPT request timed out."
        },
    )
}

fn transport_failure(mode: ResponseMode) -> ProviderFailure {
    ProviderFailure::new(
        ProviderErrorKind::Transport,
        if mode == ResponseMode::Grok {
            "Grok request could not connect."
        } else {
            "ChatGPT request could not connect."
        },
    )
}

fn malformed_failure() -> ProviderFailure {
    ProviderFailure::new(
        ProviderErrorKind::Malformed,
        "ChatGPT returned a malformed response.",
    )
}

fn size_limit_failure() -> ProviderFailure {
    ProviderFailure::new(
        ProviderErrorKind::Malformed,
        "Grok response exceeded the size limit.",
    )
}

fn normalize_failure(mut failure: ProviderFailure, mode: ResponseMode) -> ProviderFailure {
    if mode != ResponseMode::Grok {
        return failure;
    }
    failure.message = match failure.kind {
        ProviderErrorKind::UsageLimit => "Grok subscription usage limit reached.".to_owned(),
        ProviderErrorKind::Overload => "Grok service is temporarily overloaded.".to_owned(),
        ProviderErrorKind::Timeout => "Grok request timed out.".to_owned(),
        ProviderErrorKind::Transport => "Grok request could not connect.".to_owned(),
        ProviderErrorKind::Malformed => "Grok returned a malformed response stream.".to_owned(),
        _ => failure.message,
    };
    failure
}

#[cfg(test)]
#[path = "transport/tests.rs"]
mod tests;

//! Tokio-backed streaming HTTP effect for the Responses client.

use super::{HttpAttempt, HttpPort, RequestDeadlines};
use crate::provider::http::wire::WireRequest;
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
            .map_err(|_| transport_failure())?;
        let client = reqwest::Client::builder()
            .build()
            .map_err(|_| transport_failure())?;
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
                    "Could not encode ChatGPT request.",
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
            return failed_attempt(timeout_failure(), start, Vec::new(), 0);
        }
        Ok(Err(_)) => return failed_attempt(transport_failure(), start, Vec::new(), 0),
        Err(_) => return failed_attempt(timeout_failure(), start, Vec::new(), 0),
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
                    timeout_failure()
                } else {
                    ProviderFailure::new(
                        ProviderErrorKind::Stall,
                        format!(
                            "Provider stream sent nothing for {} s after it started.",
                            deadlines.unscaled_idle_ms / 1000
                        ),
                    )
                };
                return failed_attempt(
                    failure,
                    start,
                    parser.collected_items().to_vec(),
                    body_bytes,
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
                    timeout_failure()
                } else {
                    transport_failure()
                };
                return failed_attempt(
                    failure,
                    start,
                    parser.collected_items().to_vec(),
                    body_bytes,
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
                        let failure = parser.finish().err().unwrap_or_else(malformed_failure);
                        return failed_attempt(
                            failure,
                            start,
                            parser.collected_items().to_vec(),
                            body_bytes,
                        );
                    }
                } else {
                    if error_body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                        break;
                    }
                    error_body.extend_from_slice(&chunk);
                }
            }
        }
    }
    if !(200..300).contains(&status) {
        let failure = classify_http_error(status, &error_body, retry_after);
        return failed_attempt(failure, start, Vec::new(), body_bytes);
    }
    match parser.finish() {
        Ok(response) => HttpAttempt {
            received_items: response.raw_items.clone(),
            response: Ok(response),
            elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            body_bytes_received: body_bytes,
        },
        Err(failure) => failed_attempt(
            failure,
            start,
            parser.collected_items().to_vec(),
            body_bytes,
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
    }
}

fn classify_http_error(status: u16, body: &[u8], retry_after: Option<u64>) -> ProviderFailure {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
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

fn parse_retry_after(value: &str) -> Option<u64> {
    let seconds = value.parse::<f64>().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1000.0).round() as u64)
}

fn retry_after_from_body(body: &[u8]) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let seconds = value.get("resets_in_seconds")?.as_f64()?;
    (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1000.0).round() as u64)
}

fn timeout_failure() -> ProviderFailure {
    ProviderFailure::new(ProviderErrorKind::Timeout, "ChatGPT request timed out.")
}

fn transport_failure() -> ProviderFailure {
    ProviderFailure::new(
        ProviderErrorKind::Transport,
        "ChatGPT request could not connect.",
    )
}

fn malformed_failure() -> ProviderFailure {
    ProviderFailure::new(
        ProviderErrorKind::Malformed,
        "ChatGPT returned a malformed response.",
    )
}

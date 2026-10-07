use super::controller_error;
use super::provider_prompt::{now_ms, usage_value};
use crate::error::CoreError;
use crate::provider::http::{ProviderCall, RequestEvent};
use crate::run::{RunEvent, RunSnapshot, RunStore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(super) fn record_provider_events(
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    stage: &str,
    events: &[RequestEvent],
) -> Result<(), CoreError> {
    for event in events {
        let entry = match event {
            RequestEvent::Retry {
                reason,
                delay_ms,
                resumed,
            } => RunEvent::new("provider_retry", now_ms())
                .with("stage", json!(stage))
                .with("rung", json!(if stage == "develop" { "R1" } else { "" }))
                .with("reason", json!(reason))
                .with("delay_ms", json!(delay_ms))
                .with("resumed", json!(resumed)),
            RequestEvent::Switch {
                from_model,
                to_model,
            } => RunEvent::new("provider_switch", now_ms())
                .with("stage", json!(stage))
                .with("from_model", json!(from_model))
                .with("to_model", json!(to_model)),
            RequestEvent::Wait {
                reason,
                wait_ms,
                paused_ms,
                budget_paused,
            } => RunEvent::new("provider_wait", now_ms())
                .with("reason", json!(reason))
                .with("wait_ms", json!(wait_ms))
                .with("paused_ms", json!(paused_ms))
                .with("budget_paused", json!(budget_paused)),
        };
        record(store, snapshot, &entry)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) struct RequestIdentity<'a> {
    pub(super) cache_key: &'a str,
    pub(super) thread_id: &'a str,
    pub(super) previous_input_items: usize,
}

pub(super) fn record_call(
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    stage: &str,
    rung: &str,
    call: &ProviderCall,
    wall_ms: u64,
    identity: RequestIdentity<'_>,
) -> Result<(), CoreError> {
    record_provider_events(store, snapshot, stage, &call.events)?;
    let wire = call.attempts.last();
    let body = wire.and_then(|wire| serde_json::from_slice::<Value>(&wire.body).ok());
    let model = body
        .as_ref()
        .and_then(|body| body.get("model"))
        .cloned()
        .unwrap_or(Value::Null);
    let effort = body
        .as_ref()
        .and_then(|body| body.pointer("/reasoning/effort"))
        .cloned()
        .unwrap_or(Value::Null);
    let prompt_cache_key = body
        .as_ref()
        .and_then(|body| body.get("prompt_cache_key"))
        .cloned()
        .unwrap_or(Value::Null);
    let input_item_count = body
        .as_ref()
        .and_then(|body| body.get("input"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let body_bytes = wire.map_or(0, |wire| wire.body.len());
    let body_prefix_sha256 = wire.and_then(|wire| {
        input_prefix_end(&wire.body, input_item_count).map(|end| sha256_hex(&wire.body[..end]))
    });
    let previous_input_prefix_sha256 = wire.and_then(|wire| {
        input_prefix_end(&wire.body, identity.previous_input_items)
            .map(|end| sha256_hex(&wire.body[..end]))
    });
    let endpoint_host = wire
        .and_then(|wire| wire.endpoint.host_str().map(str::to_owned))
        .map(|host| {
            wire.and_then(|wire| wire.endpoint.port())
                .map_or(host.clone(), |port| {
                    if host.contains(':') {
                        format!("[{host}]:{port}")
                    } else {
                        format!("{host}:{port}")
                    }
                })
        })
        .unwrap_or_default();
    let endpoint_path = wire.map_or_else(String::new, |wire| wire.endpoint.path().to_owned());
    let routing_headers: Vec<String> = wire
        .map(|wire| {
            wire.headers
                .iter()
                .filter(|(name, _)| is_routing_header(name))
                .map(|(name, _)| name.to_ascii_lowercase())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_default();
    let thread_id = json!(identity.thread_id);
    let event = RunEvent::new("model_stage", now_ms())
        .with("stage", json!(stage))
        .with("rung", json!(rung))
        .with("model", model)
        .with("effort", effort)
        .with("tokens", usage_value(&call.response.usage))
        .with("wall_ms", json!(wall_ms))
        .with("prompt_cache_key", prompt_cache_key.clone())
        .with("cache_key", json!(identity.cache_key))
        .with("thread_id", thread_id.clone())
        .with("conversation_id", thread_id)
        .with("endpoint_host", json!(endpoint_host))
        .with("endpoint_path", json!(endpoint_path))
        .with("routing_headers", json!(routing_headers))
        .with("body_bytes", json!(body_bytes))
        .with("body_prefix_sha256", json!(body_prefix_sha256))
        .with("previous_input_items", json!(identity.previous_input_items))
        .with(
            "previous_input_prefix_sha256",
            json!(previous_input_prefix_sha256),
        );
    record(store, snapshot, &event)
}

const ROUTING_HEADER_NAMES: &[&str] = &[
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-turn-state",
    "x-codex-window-id",
    "x-codex-turn-metadata",
];

fn is_routing_header(name: &str) -> bool {
    ROUTING_HEADER_NAMES
        .iter()
        .any(|routing_name| name.eq_ignore_ascii_case(routing_name))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Return the byte offset after `item_count` serialized input items. The resulting
/// prefix excludes the comma/closing brackets, so it is stable when later turns
/// append more items to the same input array.
fn input_prefix_end(body: &[u8], item_count: usize) -> Option<usize> {
    const INPUT_MARKER: &[u8] = b"\"input\":[";
    let marker_start = body
        .windows(INPUT_MARKER.len())
        .position(|window| window == INPUT_MARKER)?;
    let mut cursor = marker_start + INPUT_MARKER.len();
    if item_count == 0 {
        return Some(cursor);
    }
    for index in 0..item_count {
        cursor = scan_json_value_end(body, skip_json_whitespace(body, cursor)?)?;
        if index + 1 < item_count {
            cursor = skip_json_whitespace(body, cursor)?;
            if body.get(cursor) != Some(&b',') {
                return None;
            }
            cursor += 1;
        }
    }
    Some(cursor)
}

fn skip_json_whitespace(body: &[u8], mut cursor: usize) -> Option<usize> {
    while body.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    (cursor < body.len()).then_some(cursor)
}

fn scan_json_value_end(body: &[u8], start: usize) -> Option<usize> {
    let first = *body.get(start)?;
    if first == b'"' {
        return scan_json_string_end(body, start);
    }
    if first == b'{' || first == b'[' {
        let mut depth = 0_usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, byte) in body.iter().enumerate().skip(start) {
            if in_string {
                if escaped {
                    escaped = false;
                } else if *byte == b'\\' {
                    escaped = true;
                } else if *byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(offset + 1);
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    let mut cursor = start;
    while body
        .get(cursor)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b',' | b']' | b'}'))
    {
        cursor += 1;
    }
    (cursor > start).then_some(cursor)
}

fn scan_json_string_end(body: &[u8], start: usize) -> Option<usize> {
    let mut escaped = false;
    for (offset, byte) in body.iter().enumerate().skip(start + 1) {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if *byte == b'"' {
            return Some(offset + 1);
        }
    }
    None
}

pub(super) fn record_event(
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    event: &RunEvent,
) -> Result<(), CoreError> {
    record(store, snapshot, event)
}

fn record(store: &RunStore, snapshot: &mut RunSnapshot, event: &RunEvent) -> Result<(), CoreError> {
    store
        .record(event, snapshot)
        .map_err(|error| controller_error("run_journal_failed", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::record_call;
    use crate::provider::http::{ProviderCall, ResponseMode, WireRequest};
    use crate::provider::{ModelResponse, ModelUsage};
    use crate::run::{RunSnapshot, RunStore};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use url::Url;

    #[test]
    fn model_stage_journal_records_cache_and_conversation_identity() {
        let directory = std::env::temp_dir().join(format!(
            "kogen-provider-journal-{}-{}",
            std::process::id(),
            super::now_ms()
        ));
        let store = RunStore::new(&directory);
        let mut snapshot = RunSnapshot {
            schema: 1,
            run_id: "run".to_owned(),
            slug: "greet".to_owned(),
            approval_sha256: String::new(),
            approval_commit: String::new(),
            target_branch: "main".to_owned(),
            status: "running".to_owned(),
            landing: None,
            owner_pid: std::process::id(),
            owner_started_ms: 0,
            started_ms: 0,
            fields: BTreeMap::new(),
        };
        let call = ProviderCall {
            response: ModelResponse {
                id: "response".to_owned(),
                text: String::new(),
                tool_calls: Vec::new(),
                usage: ModelUsage::default(),
                raw_items: Vec::new(),
            },
            events: Vec::new(),
            attempts: vec![WireRequest {
                endpoint: Url::parse(
                    "https://example.invalid:8443/v1/responses?api_key=do-not-journal",
                )
                .unwrap(),
                mode: ResponseMode::Injected,
                headers: vec![
                    (
                        "authorization".to_owned(),
                        "Bearer do-not-journal".to_owned(),
                    ),
                    ("session-id".to_owned(), "cache-456".to_owned()),
                    ("thread-id".to_owned(), "thread-123".to_owned()),
                    ("x-client-request-id".to_owned(), "thread-123".to_owned()),
                    (
                        "x-codex-turn-state".to_owned(),
                        "route-token-do-not-journal".to_owned(),
                    ),
                ],
                body: br#"{"model":"gpt-6.1-sol","reasoning":{"effort":"high"},"prompt_cache_key":"cache-456","input":[{"role":"user","content":[{"type":"input_text","text":"prompt-do-not-journal"}]},{"type":"function_call","call_id":"call-1","name":"shell","arguments":"{}"}]}"#.to_vec(),
            }],
        };

        record_call(
            &store,
            &mut snapshot,
            "plan",
            "",
            &call,
            12,
            super::RequestIdentity {
                cache_key: "cache-456",
                thread_id: "thread-123",
                previous_input_items: 1,
            },
        )
        .unwrap();
        let events = std::fs::read_to_string(directory.join("events.jsonl")).unwrap();
        let event: Value = serde_json::from_str(events.trim()).unwrap();
        assert_eq!(event["prompt_cache_key"], "cache-456");
        assert_eq!(event["cache_key"], "cache-456");
        assert_eq!(event["thread_id"], "thread-123");
        assert_eq!(event["conversation_id"], "thread-123");
        assert_eq!(event["endpoint_host"], "example.invalid:8443");
        assert_eq!(event["endpoint_path"], "/v1/responses");
        assert_eq!(event["body_bytes"], call.attempts[0].body.len());
        assert_eq!(
            event["routing_headers"],
            json!([
                "session-id",
                "thread-id",
                "x-client-request-id",
                "x-codex-turn-state"
            ])
        );
        assert_eq!(event["body_prefix_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(
            event["previous_input_prefix_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert_ne!(
            event["body_prefix_sha256"],
            event["previous_input_prefix_sha256"]
        );
        assert!(!events.contains("prompt-do-not-journal"));
        assert!(!events.contains("route-token-do-not-journal"));
        assert!(!events.contains("Bearer do-not-journal"));
        assert!(!events.contains("api_key=do-not-journal"));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn input_prefix_through_prior_items_is_byte_identical_after_append() {
        let previous = br#"{"model":"gpt-6-luna","input":[{"role":"user","content":"first"}]}"#;
        let current = br#"{"model":"gpt-6-luna","input":[{"role":"user","content":"first"},{"role":"assistant","content":"second"}]}"#;
        let previous_end = super::input_prefix_end(previous, 1).unwrap();
        let current_end = super::input_prefix_end(current, 1).unwrap();
        assert_eq!(&previous[..previous_end], &current[..current_end]);
        assert_eq!(
            super::sha256_hex(&previous[..previous_end]),
            super::sha256_hex(&current[..current_end])
        );
    }
}

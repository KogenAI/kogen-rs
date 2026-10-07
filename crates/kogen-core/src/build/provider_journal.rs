use super::controller_error;
use super::provider_prompt::{now_ms, usage_value};
use crate::error::CoreError;
use crate::provider::http::{ProviderCall, RequestEvent};
use crate::run::{RunEvent, RunSnapshot, RunStore};
use serde_json::{Value, json};

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
        .with("conversation_id", thread_id);
    record(store, snapshot, &event)
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
                endpoint: Url::parse("https://example.invalid/responses").unwrap(),
                mode: ResponseMode::Injected,
                headers: vec![("thread-id".to_owned(), "thread-123".to_owned())],
                body: serde_json::to_vec(&json!({
                    "model": "gpt-6.1-sol",
                    "reasoning": {"effort": "high"},
                    "prompt_cache_key": "cache-456"
                }))
                .unwrap(),
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
            },
        )
        .unwrap();
        let events = std::fs::read_to_string(directory.join("events.jsonl")).unwrap();
        let event: Value = serde_json::from_str(events.trim()).unwrap();
        assert_eq!(event["prompt_cache_key"], "cache-456");
        assert_eq!(event["cache_key"], "cache-456");
        assert_eq!(event["thread_id"], "thread-123");
        assert_eq!(event["conversation_id"], "thread-123");
        let _ = std::fs::remove_dir_all(directory);
    }
}

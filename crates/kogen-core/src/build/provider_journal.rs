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

pub(super) fn record_call(
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    stage: &str,
    rung: &str,
    call: &ProviderCall,
    wall_ms: u64,
) -> Result<(), CoreError> {
    record_provider_events(store, snapshot, stage, &call.events)?;
    let body = call
        .attempts
        .last()
        .and_then(|wire| serde_json::from_slice::<Value>(&wire.body).ok());
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
    let event = RunEvent::new("model_stage", now_ms())
        .with("stage", json!(stage))
        .with("rung", json!(rung))
        .with("model", model)
        .with("effort", effort)
        .with("tokens", usage_value(&call.response.usage))
        .with("wall_ms", json!(wall_ms))
        .with(
            "prompt_cache_key",
            body.and_then(|body| body.get("prompt_cache_key").cloned())
                .unwrap_or(Value::Null),
        );
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

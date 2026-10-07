use kogen_core::run::orchestration::replay::OrchestrationReplay;
use serde_json::Value;

pub(super) fn apply(state: &mut OrchestrationReplay, event: &Value) -> Result<Value, String> {
    state.apply(event)?;
    Ok(state.observe())
}

pub(super) fn observe(state: &OrchestrationReplay) -> Value {
    state.observe()
}

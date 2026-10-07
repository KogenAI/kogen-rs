use kogen_core::run::orchestration::GateReplay;
use serde_json::Value;

pub(super) fn apply(state: &mut GateReplay, event: &Value) -> Result<Value, String> {
    state.apply(event)?;
    Ok(state.observe())
}

pub(super) fn observe(state: &GateReplay) -> Value {
    state.observe()
}

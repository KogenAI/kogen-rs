//! Strict JSON decoding for queue events; queue decisions stay in kogen-core.
use kogen_core::queue::{
    DrainOutcome, QueueApproval, QueueEvent, QueueObservation, QueueScheduler,
};
use serde_json::Value;

use super::{object, string};

pub(super) fn apply(scheduler: &mut QueueScheduler, event: &Value) -> Result<Value, String> {
    let tag = string(event, "tag")?;
    let transition = match tag {
        "Enqueue" => {
            let value = event
                .get("value")
                .filter(|value| value.is_object())
                .ok_or_else(|| "Enqueue requires an object value".to_owned())?;
            let slug = value
                .get("slug")
                .and_then(Value::as_str)
                .ok_or_else(|| "Enqueue value requires string field `slug`".to_owned())?;
            let time = integer(value, "time")?;
            let priority = integer(value, "priority")?;
            let hash = optional_string(value, "approval_hash")?.unwrap_or_default();
            let commit = optional_string(value, "approval_commit")?.unwrap_or_default();
            QueueEvent::Enqueue(
                QueueApproval::new(slug, time, priority, hash).with_approval_commit(commit),
            )
        }
        "Start" => QueueEvent::Start,
        "Die" => QueueEvent::Die,
        "Halt" => QueueEvent::Halt,
        "Release" => QueueEvent::Release,
        "Outcome" => {
            let value = object(event, "value")?;
            let kind = value
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| "Outcome value requires string field `kind`".to_owned())?;
            QueueEvent::Outcome(outcome(kind))
        }
        _ => return Err(format!("unknown queue event tag {tag:?}")),
    };
    Ok(observation(scheduler.apply(transition)))
}

fn integer(value: &Value, key: &str) -> Result<i64, String> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("Enqueue value requires integer field `{key}`"))
}

fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!("Enqueue value field `{key}` must be a string")),
    }
}

fn outcome(kind: &str) -> DrainOutcome {
    match kind {
        "landed" => DrainOutcome::Landed,
        "failed" => DrainOutcome::Failed,
        "failed_provider" => DrainOutcome::FailedProvider,
        "parked" => DrainOutcome::Parked,
        "stopped_environment" => DrainOutcome::StoppedEnvironment,
        "stopped_provider" => DrainOutcome::StoppedProvider,
        "stopped_controller" => DrainOutcome::StoppedController,
        "skipped" => DrainOutcome::Skipped,
        other => DrainOutcome::Other(other.to_owned()),
    }
}

fn observation(value: QueueObservation) -> Value {
    serde_json::to_value(value).expect("queue observations are serializable")
}

#[cfg(test)]
mod tests {
    use super::apply;
    use kogen_core::queue::QueueScheduler;
    use serde_json::{Value, json};

    fn event(queue: &mut QueueScheduler, event: Value) -> Value {
        apply(queue, &event).expect("well-formed queue trace event")
    }

    #[test]
    fn live_second_start_trace_keeps_the_current_approval_and_exact_exit() {
        let mut queue = QueueScheduler::new();
        event(
            &mut queue,
            json!({"tag":"Enqueue","value":{"slug":"alpha","time":1,"priority":0}}),
        );
        event(&mut queue, json!({"tag":"Start"}));

        let observed = event(&mut queue, json!({"tag":"Start"}));

        assert_eq!(
            observed,
            json!({
                "last":"ok", "line":"already_running", "exit":0,
                "held":true, "alive":true, "stop":false, "phase":"building",
                "current":"alpha", "queue":[], "built":0, "landed":0
            })
        );
    }

    #[test]
    fn same_hash_reapproval_trace_runs_the_slug_once_then_keeps_it_queued() {
        let mut queue = QueueScheduler::new();
        event(
            &mut queue,
            json!({"tag":"Enqueue","value":{"slug":"alpha","time":1,"priority":0,"approval_hash":"same","approval_commit":"c1"}}),
        );
        event(
            &mut queue,
            json!({"tag":"Enqueue","value":{"slug":"bravo","time":2,"priority":0,"approval_hash":"other","approval_commit":"c2"}}),
        );
        event(&mut queue, json!({"tag":"Start"}));
        event(
            &mut queue,
            json!({"tag":"Enqueue","value":{"slug":"alpha","time":3,"priority":9,"approval_hash":"same","approval_commit":"c3"}}),
        );
        event(
            &mut queue,
            json!({"tag":"Outcome","value":{"kind":"failed"}}),
        );
        let done = event(
            &mut queue,
            json!({"tag":"Outcome","value":{"kind":"landed"}}),
        );

        assert_eq!(
            done,
            json!({
                "last":"ok", "line":"done", "exit":1,
                "held":false, "alive":false, "stop":false, "phase":"idle",
                "current":"", "queue":["alpha"], "built":2, "landed":1
            })
        );
    }
}

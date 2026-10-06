//! Strict recovery event decoding; production transitions live in `kogen-core`.

use kogen_core::recovery::{RecoveryEvent, RecoveryFact, RecoveryModel};
use serde_json::{Map, Value, json};

use super::{object, string};

pub(super) fn apply(replay: &mut RecoveryModel, event: &Value) -> Result<Value, String> {
    let tag = string(event, "tag")?;
    let transition = match tag {
        "Put" => {
            let value = object(event, "value")?;
            RecoveryEvent::Put(RecoveryFact {
                id: string_field(value, "id")?.to_owned(),
                status: string_field(value, "status")?.to_owned(),
                alive: boolean_field(value, "alive")?,
                on_base: boolean_field(value, "onBase")?,
                last_event: string_field(value, "lastEvent")?.to_owned(),
                incoming: boolean_field(value, "incoming")?,
                queued: boolean_field(value, "queued")?,
                claim: boolean_field(value, "claim")?,
                reason: string_field(value, "reason")?.to_owned(),
            })
        }
        "Recover" => RecoveryEvent::Recover,
        "Reapprove" => {
            RecoveryEvent::Reapprove(string_field(object(event, "value")?, "id")?.to_owned())
        }
        other => return Err(format!("unknown recovery event tag {other:?}")),
    };
    Ok(json!(replay.apply(transition)))
}

fn string_field<'a>(value: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("recovery event requires string field `{key}`"))
}

fn boolean_field(value: &Map<String, Value>, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("recovery event requires boolean field `{key}`"))
}

//! Strict status event decoding; status policy remains in `kogen-core`.

use kogen_core::status::{StatusEvent, StatusRaw, StatusReplay, StatusRow};
use serde_json::{Map, Value, json};

use super::{object, string};

pub(super) fn apply(replay: &mut StatusReplay, event: &Value) -> Result<Value, String> {
    let tag = string(event, "tag")?;
    let transition = match tag {
        "Row" => {
            let value = object(event, "value")?;
            StatusEvent::Row(StatusRow {
                slug: string_field(value, "slug")?.to_owned(),
                status: string_field(value, "status")?.to_owned(),
                priority: integer(value, "priority")?,
                at: integer(value, "at")?,
                blocks: string_field(value, "blocks")?.to_owned(),
                sched: string_field(value, "sched")?.to_owned(),
                started: integer(value, "started")?,
                index: integer(value, "index")?,
            })
        }
        "Raw" => {
            let value = object(event, "value")?;
            StatusEvent::Raw(StatusRaw {
                slug: string_field(value, "slug")?.to_owned(),
                trailer: boolean_field(value, "trailer")?,
                claimed: boolean_field(value, "claimed")?,
                run_status: string_field(value, "runStatus")?.to_owned(),
                event: string_field(value, "event")?.to_owned(),
                alive: boolean_field(value, "alive")?,
                approved: boolean_field(value, "approved")?,
                reason: string_field(value, "reason")?.to_owned(),
                same: boolean_field(value, "same")?,
                blocks: string_field(value, "blocks")?.to_owned(),
                priority: integer(value, "priority")?,
                at: integer(value, "at")?,
            })
        }
        "Derive" => StatusEvent::Derive,
        "Now" => StatusEvent::Now(integer(object(event, "value")?, "t")?),
        "Older" => StatusEvent::Older(integer(object(event, "value")?, "n")?),
        "Queue" => StatusEvent::Queue(boolean_field(object(event, "value")?, "running")?),
        "Agents" => StatusEvent::Agents(integer(object(event, "value")?, "busy")?),
        "Watch" => StatusEvent::Watch(string_field(object(event, "value")?, "slug")?.to_owned()),
        "Json" => StatusEvent::Json,
        other => return Err(format!("unknown status event tag {other:?}")),
    };
    Ok(json!(replay.apply(transition)))
}

fn integer(value: &Map<String, Value>, key: &str) -> Result<i64, String> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("status event requires integer field `{key}`"))
}

fn string_field<'a>(value: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("status event requires string field `{key}`"))
}

fn boolean_field(value: &Map<String, Value>, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("status event requires boolean field `{key}`"))
}

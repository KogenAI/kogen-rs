//! Event decoder for the production landing transition model.

use kogen_core::git::landing::model::RepairKind;
use kogen_core::git::landing::{LandingEvent, LandingModel, RebaseKind};
use serde_json::{Map, Value, json};

pub(super) fn apply(model: &mut LandingModel, event: &Value) -> Result<Value, String> {
    let tag = string(event, "tag")?;
    let transition = match tag {
        "Record" => LandingEvent::Record,
        "Lock" => LandingEvent::Lock,
        "Head" => LandingEvent::Head(head_kind(string_field(object(event, "value")?, "kind")?)?),
        "Push" => LandingEvent::Push,
        "Cas" => LandingEvent::Cas(boolean_field(object(event, "value")?, "won")?),
        "Worktree" => LandingEvent::Worktree(boolean_field(object(event, "value")?, "dirty")?),
        "Drop" => LandingEvent::Drop(boolean_field(object(event, "value")?, "ok")?),
        "Again" => LandingEvent::Again,
        "Rebase" => LandingEvent::Rebase(
            RebaseKind::parse(string_field(object(event, "value")?, "kind")?)
                .ok_or_else(|| "rebase event has unknown kind".to_owned())?,
        ),
        "Repair" => {
            LandingEvent::Repair(repair_kind(string_field(object(event, "value")?, "kind")?)?)
        }
        other => return Err(format!("unknown rebase event tag {other:?}")),
    };
    Ok(json!(model.apply(transition)))
}

fn object<'a>(event: &'a Value, key: &str) -> Result<&'a Map<String, Value>, String> {
    event
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("rebase event requires object field `{key}`"))
}

fn string<'a>(event: &'a Value, key: &str) -> Result<&'a str, String> {
    event
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("rebase event requires string field `{key}`"))
}

fn string_field<'a>(event: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    event
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("rebase event requires string field `{key}`"))
}

fn boolean_field(event: &Map<String, Value>, key: &str) -> Result<bool, String> {
    event
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("rebase event requires boolean field `{key}`"))
}

fn head_kind(value: &str) -> Result<&'static str, String> {
    match value {
        "ok" | "not_fast_forward" | "tree_mismatch" => Ok(match value {
            "ok" => "ok",
            "not_fast_forward" => "not_fast_forward",
            _ => "tree_mismatch",
        }),
        _ => Err("head event has unknown kind".to_owned()),
    }
}

fn repair_kind(value: &str) -> Result<RepairKind, String> {
    match value {
        "green" => Ok(RepairKind::Green),
        "red" => Ok(RepairKind::Red),
        "spent" => Ok(RepairKind::Spent),
        _ => Err("repair event has unknown kind".to_owned()),
    }
}

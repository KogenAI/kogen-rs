use serde_json::{Map, Value, json};

pub fn intent_initial() -> Value {
    json!({
        "life": {}, "refs": {}, "shown": "", "casTries": 0,
        "did": "", "last": "ok", "exit": 0
    })
}

pub fn intent_apply(state: &Value, event: &Value) -> Result<Value, String> {
    let tag = text(event, "tag")?;
    let value = event.get("value").unwrap_or(&Value::Null);
    match tag {
        "Init" => Ok(intent_initial()),
        "Shape" => {
            let slug = text(value, "slug")?;
            let result = text(value, "result")?;
            if !known(slug) {
                return Ok(stop(state, "unknown_slug"));
            }
            let life = lives(state);
            let current = life.get(slug).and_then(Value::as_str).unwrap_or_default();
            if current == "building" {
                return Ok(stop(state, "intent_building"));
            }
            if result == "empty" {
                return Ok(stop(state, "intent/request_unavailable"));
            }
            if result == "provider" {
                return Ok(stop(state, "provider/overload"));
            }
            if result == "failed" {
                let mut next = stop(state, "candidate/repair_limit");
                set(&mut next, "did", json!("shape_failed"));
                return Ok(next);
            }
            if result != "valid" {
                return Ok(stop(state, "unknown_result"));
            }
            if is_bound(current) {
                let mut next = state.clone();
                set(&mut next, "last", json!("ok"));
                set(&mut next, "exit", json!(0));
                set(&mut next, "did", json!("shaped"));
                set(&mut next, "shown", json!(""));
                return Ok(next);
            }
            let mut next = state.clone();
            let mut next_life = life;
            next_life.insert(slug.to_owned(), json!("shaped"));
            set(&mut next, "life", Value::Object(next_life));
            set(&mut next, "last", json!("ok"));
            set(&mut next, "exit", json!(0));
            set(&mut next, "did", json!("shaped"));
            set(&mut next, "shown", json!(""));
            Ok(next)
        }
        "Approve" => {
            let slug = text(value, "slug")?;
            let mode = text(value, "mode")?;
            let hash = text(value, "hash")?;
            let prefix_ok = boolean(value, "prefixOk")?;
            let race = text(value, "race")?;
            if !known(slug) {
                return Ok(stop(state, "unknown_slug"));
            }
            let life = lives(state);
            let refs = refs(state);
            let current = life.get(slug).and_then(Value::as_str).unwrap_or_default();
            if !life.contains_key(slug) {
                return Ok(stop(state, "intent/not_found"));
            }
            if current == "building" {
                return Ok(stop(state, "intent_building"));
            }
            if mode == "card" {
                let mut next = state.clone();
                set(&mut next, "last", json!("needs_decision"));
                set(&mut next, "exit", json!(5));
                set(&mut next, "did", json!("card"));
                set(&mut next, "shown", json!(hash));
                set(&mut next, "casTries", json!(0));
                return Ok(next);
            }
            if mode != "commit" {
                return Ok(stop(state, "unknown_mode"));
            }
            if !prefix_ok {
                let mut next = stop(state, "intent/hash_mismatch");
                set(&mut next, "shown", json!(hash));
                set(&mut next, "casTries", json!(0));
                return Ok(next);
            }
            if race == "twice" {
                let mut next = stop(state, "controller/approval_cas_lost");
                set(&mut next, "casTries", json!(2));
                return Ok(next);
            }
            if race != "none" && race != "once" {
                return Ok(stop(state, "unknown_race"));
            }
            let previous = refs
                .get(slug)
                .and_then(|value| value.get("n"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let mut next_life = life;
            next_life.insert(slug.to_owned(), json!("approved"));
            let mut next_refs = refs;
            next_refs.insert(slug.to_owned(), json!({"n": previous + 1, "hash": hash}));
            let mut next = state.clone();
            set(&mut next, "life", Value::Object(next_life));
            set(&mut next, "refs", Value::Object(next_refs));
            set(&mut next, "last", json!("ok"));
            set(&mut next, "exit", json!(0));
            set(&mut next, "did", json!("approved"));
            set(&mut next, "shown", json!(hash));
            set(
                &mut next,
                "casTries",
                json!(if race == "once" { 2 } else { 1 }),
            );
            Ok(next)
        }
        "Remove" => {
            let slug = text(value, "slug")?;
            let force = boolean(value, "force")?;
            if !known(slug) {
                return Ok(stop(state, "unknown_slug"));
            }
            let life = lives(state);
            let current = life.get(slug).and_then(Value::as_str).unwrap_or_default();
            if !life.contains_key(slug) {
                return Ok(stop(state, "intent/not_found"));
            }
            if current == "building" {
                return Ok(stop(state, "intent/remove_blocked"));
            }
            if needs_force(current) && !force {
                return Ok(stop(state, "intent/remove_requires_force"));
            }
            let mut next = state.clone();
            set(&mut next, "life", object_without(&life, slug));
            set(&mut next, "refs", object_without(&refs(state), slug));
            set(&mut next, "last", json!("ok"));
            set(&mut next, "exit", json!(0));
            set(&mut next, "did", json!("removed"));
            set(&mut next, "shown", json!(""));
            set(&mut next, "casTries", json!(0));
            Ok(next)
        }
        "Adopt" => {
            let slug = text(value, "slug")?;
            let status = text(value, "status")?;
            if !["building", "failed", "parked", "interrupted", "landed"].contains(&status) {
                return Ok(stop(state, "bad_adopt"));
            }
            let life = lives(state);
            let current = life.get(slug).and_then(Value::as_str).unwrap_or_default();
            if current != "approved" && !(current == "building" && status != "building") {
                return Ok(stop(state, "bad_adopt"));
            }
            let mut next = state.clone();
            let mut next_life = life;
            next_life.insert(slug.to_owned(), json!(status));
            set(&mut next, "life", Value::Object(next_life));
            set(&mut next, "last", json!("ok"));
            set(&mut next, "exit", json!(0));
            set(&mut next, "did", json!("adopted"));
            set(&mut next, "shown", json!(""));
            Ok(next)
        }
        _ => Err(format!("unknown intent event {tag}")),
    }
}

fn stop(state: &Value, code: &str) -> Value {
    let mut next = state.clone();
    set(&mut next, "last", json!(code));
    set(&mut next, "exit", json!(exit_code(code)));
    set(&mut next, "did", json!(""));
    set(&mut next, "shown", json!(""));
    next
}

fn exit_code(code: &str) -> i32 {
    match code {
        "ok" => 0,
        "needs_decision" => 5,
        "candidate/repair_limit" | "intent/hash_mismatch" => 1,
        "intent/not_found"
        | "intent/request_unavailable"
        | "intent/remove_blocked"
        | "intent/remove_requires_force"
        | "intent_building"
        | "unknown_slug" => 2,
        "provider/overload" => 4,
        _ => 70,
    }
}

fn known(slug: &str) -> bool {
    matches!(slug, "alpha" | "bravo")
}

fn is_bound(life: &str) -> bool {
    matches!(
        life,
        "approved" | "building" | "failed" | "parked" | "interrupted" | "landed"
    )
}

fn needs_force(life: &str) -> bool {
    matches!(life, "approved" | "failed" | "parked" | "interrupted")
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {key}"))
}

fn boolean(value: &Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing boolean field {key}"))
}

fn lives(state: &Value) -> Map<String, Value> {
    state
        .get("life")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn refs(state: &Value) -> Map<String, Value> {
    state
        .get("refs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn object_without(map: &Map<String, Value>, key: &str) -> Value {
    let mut next = map.clone();
    next.remove(key);
    Value::Object(next)
}

fn set(state: &mut Value, key: &str, value: Value) {
    if let Some(map) = state.as_object_mut() {
        map.insert(key.to_owned(), value);
    }
}

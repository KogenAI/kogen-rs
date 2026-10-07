use serde_json::{Map, Value, json};

pub fn approve_initial() -> Value {
    json!({
        "approvals": {}, "cache": ["", ""], "checkRuns": 0, "last": "ok", "exit": 0,
        "sha8": "", "approver": "", "feas": "", "bwarn": false,
        "lwarn": false, "ran": false, "_cacheStatus": ""
    })
}

pub fn approve_observe(state: &Value) -> Value {
    let Some(map) = state.as_object() else {
        return json!({});
    };
    let mut observed = map.clone();
    observed.remove("_cacheStatus");
    Value::Object(observed)
}

pub fn approve_apply(state: &Value, event: &Value) -> Result<Value, String> {
    let tag = text(event, "tag")?;
    if tag == "Init" {
        return Ok(approve_initial());
    }
    if tag != "Approve" {
        return Err(format!("unknown approval event {tag}"));
    }
    let value = event.get("value").unwrap_or(&Value::Null);
    let given = text(value, "given")?;
    let prefix_ok = boolean(value, "prefixOk")?;
    let stable = boolean(value, "stableBeforeCas")?;
    let new_sha8 = text(value, "newSha8")?;
    let sha = text(value, "sha")?;
    let sha8 = text(value, "sha8")?;
    let slug = text(value, "slug")?;
    let by = text(value, "by")?;
    let by_bad = boolean(value, "byBad")?;
    let ident = text(value, "ident")?;
    let parse_error = boolean(value, "parseErr")?;
    let lint_error = boolean(value, "lintErr")?;
    let lint_warning = boolean(value, "lintWarn")?;
    let missing = boolean(value, "missing")?;
    let setup = text(value, "setup")?;
    let cache_key = text(value, "cacheKey")?;
    let baseline = text(value, "baseline")?;
    let acceptance = text(value, "acceptance")?;
    let witness_mode = boolean(value, "witnessMode")?;
    let feasibility = text(value, "feas")?;
    let commit = text(value, "commit")?;
    let base_sha = text(value, "baseSha")?;

    let hashed = !given.is_empty();
    let approver = if !by.is_empty() { by } else { ident };
    if by_bad {
        return Ok(stop(state, "usage", false));
    }
    if hashed && missing {
        return Ok(stop(state, "intent/acceptance_missing", false));
    }
    if hashed && !prefix_ok {
        let mut next = stop(state, "intent/hash_mismatch", false);
        set(&mut next, "sha8", json!(sha8));
        return Ok(next);
    }
    if parse_error {
        return Ok(stop(state, "intent/parse", false));
    }
    if lint_error {
        return Ok(stop(state, "intent/lint", false));
    }
    if missing {
        return Ok(stop(state, "intent/acceptance_missing", false));
    }
    if approver.is_empty() {
        return Ok(stop(state, "intent/approval_identity_unavailable", false));
    }
    if setup != "ok" {
        return Ok(stop(state, "environment/setup_failed", false));
    }

    let identity = json!([text(value, "baseTree")?, cache_key]);
    let hit = state.get("cache") == Some(&identity);
    let baseline_status = if hit {
        text_or_empty(state, "_cacheStatus")
    } else {
        baseline
    };
    let ran = !hit;
    let mut checked = state.clone();
    set(&mut checked, "cache", identity);
    if !hit {
        let count = state.get("checkRuns").and_then(Value::as_u64).unwrap_or(0);
        set(&mut checked, "checkRuns", json!(count + 1));
    }
    set(&mut checked, "_cacheStatus", json!(baseline_status));
    if acceptance == "tool_missing" {
        return Ok(stop(&checked, "environment/tool_missing", ran));
    }
    if acceptance != "green" {
        return Ok(stop(&checked, "check/acceptance_check_failed", ran));
    }
    if !hashed {
        let mut next = checked;
        set(&mut next, "last", json!("needs_decision"));
        set(&mut next, "exit", json!(5));
        set(&mut next, "sha8", json!(sha8));
        set(&mut next, "approver", json!(approver));
        set(&mut next, "feas", json!(feasibility));
        set(&mut next, "bwarn", json!(baseline_status == "red"));
        set(&mut next, "lwarn", json!(lint_warning));
        set(&mut next, "ran", json!(ran));
        return Ok(next);
    }
    if !stable {
        let mut next = stop(&checked, "intent/hash_mismatch", ran);
        set(&mut next, "sha8", json!(new_sha8));
        return Ok(next);
    }
    if witness_mode && !proven(feasibility) {
        return Ok(stop(&checked, "intent/unproven", ran));
    }

    let mut approvals = state
        .get("approvals")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let previous = approvals
        .get(slug)
        .and_then(|approval| approval.get("n"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    approvals.insert(
        slug.to_owned(),
        json!({
            "n": previous + 1, "sha": sha, "by": approver, "commit": commit,
            "base": base_sha, "feas": feasibility
        }),
    );
    let mut next = checked;
    set(&mut next, "last", json!("ok"));
    set(&mut next, "exit", json!(0));
    set(&mut next, "sha8", json!(sha8));
    set(&mut next, "approver", json!(approver));
    set(&mut next, "feas", json!(feasibility));
    set(&mut next, "bwarn", json!(baseline_status == "red"));
    set(&mut next, "lwarn", json!(lint_warning));
    set(&mut next, "ran", json!(ran));
    set(&mut next, "approvals", Value::Object(approvals));
    Ok(next)
}

fn stop(state: &Value, code: &str, ran: bool) -> Value {
    let mut next = state.clone();
    set(&mut next, "last", json!(code));
    set(&mut next, "exit", json!(exit_code(code)));
    set(&mut next, "ran", json!(ran));
    set(&mut next, "sha8", json!(""));
    set(&mut next, "approver", json!(""));
    set(&mut next, "feas", json!(""));
    set(&mut next, "bwarn", json!(false));
    set(&mut next, "lwarn", json!(false));
    next
}

fn exit_code(code: &str) -> i32 {
    match code {
        "ok" => 0,
        "needs_decision" => 5,
        "intent/parse"
        | "intent/lint"
        | "intent/hash_mismatch"
        | "intent/unproven"
        | "check/acceptance_check_failed" => 1,
        "usage" | "intent/acceptance_missing" | "intent/approval_identity_unavailable" => 2,
        _ => 3,
    }
}

fn proven(value: &str) -> bool {
    matches!(value, "PROVEN" | "PROVEN with concerns")
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {key}"))
}

fn text_or_empty<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn boolean(value: &Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing boolean field {key}"))
}

fn set(state: &mut Value, key: &str, value: Value) {
    if let Some(map) = state.as_object_mut() {
        map.insert(key.to_owned(), value);
    }
}

#[allow(dead_code)]
fn _object(_: Map<String, Value>) {}

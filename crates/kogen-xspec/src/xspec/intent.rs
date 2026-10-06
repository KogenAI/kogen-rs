use super::{Adapter, ApprovalSummary, SourceBytes, empty_observation, object, string, value_with};
use kogen_core::approval::replay::{self, intent_apply};
use kogen_core::intent::{approval_sha256, intent_sha256};
use serde_json::{Map, Value, json};

pub(super) fn apply(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    let tag = string(event, "tag")?;
    match tag {
        "Shape" => shape(adapter, event),
        "Approve" => approve(adapter, event),
        "Remove" => remove(adapter, event),
        "Adopt" => adopt(adapter, event),
        _ => Err(format!("unknown intent event `{tag}`")),
    }
}

pub(super) fn observe(adapter: &Adapter) -> Value {
    let mut observation = adapter.state.clone();
    let mut refs = Map::new();
    if let Ok(summaries) = adapter.project.approval_summaries(&["alpha", "bravo"]) {
        for (slug, summary) in summaries {
            refs.insert(slug.to_owned(), summary_ref(&summary));
        }
    }
    if let Some(map) = observation.as_object_mut() {
        map.insert("refs".to_owned(), Value::Object(refs));
        return observation;
    }
    empty_observation()
}

fn shape(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    let value = event
        .get("value")
        .filter(|value| value.is_object())
        .ok_or_else(|| "Shape requires a value object".to_owned())?;
    let slug = string(value, "slug")?;
    let result = string(value, "result")?;
    let next = intent_apply(&adapter.state, event)?;
    if result == "valid"
        && next.get("did").and_then(Value::as_str) == Some("shaped")
        && matches!(slug, "alpha" | "bravo")
        && !adapter
            .project
            .intent_source_exists(slug)
            .map_err(|error| error.to_string())?
    {
        let bytes = adapter.project.reset_sources(slug)?;
        adapter.project.write_sources(slug, &bytes)?;
        adapter.project.persist_sources(slug)?;
    }
    adapter.state = next;
    Ok(observe(adapter))
}

fn approve(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    let value = event
        .get("value")
        .and_then(Value::as_object)
        .ok_or_else(|| "Approve requires a value object".to_owned())?;
    let value = Value::Object(value.clone());
    let slug = string(&value, "slug")?;
    let mode = string(&value, "mode")?;
    let claim = string(&value, "hash")?;
    let race = string(&value, "race")?;
    let _asserted_prefix = super::boolean(&value, "prefixOk")?;

    let source = if matches!(slug, "alpha" | "bravo")
        && adapter
            .project
            .intent_source_exists(slug)
            .map_err(|error| error.to_string())?
        && adapter
            .project
            .acceptance_source_exists(slug)
            .map_err(|error| error.to_string())?
    {
        Some(
            adapter
                .project
                .read_sources(slug)
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let actual_hash = source
        .as_ref()
        .map(|bytes| approval_sha256(&bytes.intent, &bytes.acceptance));
    let prefix_ok = actual_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with(claim));
    let output_hash = if mode == "card" || prefix_ok {
        actual_hash.clone().unwrap_or_else(|| claim.to_owned())
    } else {
        claim.to_owned()
    };
    let mut effective_value = value.as_object().cloned().unwrap_or_default();
    effective_value.insert("prefixOk".to_owned(), json!(prefix_ok));
    effective_value.insert("hash".to_owned(), json!(output_hash));
    effective_value.insert("race".to_owned(), json!("none"));
    let effective = value_with(event, Value::Object(effective_value));

    let mut actual_race = if matches!(race, "none" | "once" | "twice") {
        "none"
    } else {
        race
    };
    let candidate = intent_apply(&adapter.state, &effective)?;
    if candidate.get("did").and_then(Value::as_str) == Some("approved")
        && matches!(race, "none" | "once" | "twice")
    {
        let source = source.ok_or_else(|| "approval source disappeared".to_owned())?;
        let (tries, won) = create_and_cas_approval(adapter, slug, &source, &output_hash, race)?;
        actual_race = match (tries, won) {
            (1, true) => "none",
            (2, true) => "once",
            (2, false) => "twice",
            _ => return Err("approval CAS returned an impossible attempt count".to_owned()),
        };
    }
    if let Some(map) = effective.get("value").and_then(Value::as_object) {
        let mut next_value = map.clone();
        next_value.insert("race".to_owned(), json!(actual_race));
        adapter.state = intent_apply(
            &adapter.state,
            &value_with(event, Value::Object(next_value)),
        )?;
    } else {
        return Err("Approve event lost its value object".to_owned());
    }
    Ok(observe(adapter))
}

fn create_and_cas_approval(
    adapter: &Adapter,
    slug: &str,
    source: &SourceBytes,
    hash: &str,
    race_injection: &str,
) -> Result<(u8, bool), String> {
    let by = adapter
        .project
        .checkout_repo
        .author_identity()
        .map_err(|error| error.to_string())?;
    let base = adapter
        .project
        .base_commit()
        .map_err(|error| error.to_string())?;
    let ref_name = format!("refs/kogen/intents/{slug}");
    let mut parent = adapter
        .project
        .origin
        .ref_target(&ref_name)
        .map_err(|error| error.to_string())?;
    let mut tries = 0_u8;
    let mut won = false;
    for attempt in 0..2 {
        let approval_bytes = approval_document(slug, hash, &source.intent, &base, &by);
        let commit = replay::create_approval_commit(
            &adapter.project.origin,
            replay::ApprovalPackage {
                slug,
                intent: &source.intent,
                approval: &approval_bytes,
                ledger: None,
                test_path: &format!(".kogen/acceptance/{slug}.t.sh"),
                acceptance: &source.acceptance,
                by: &by,
                hash,
                at: "2000-01-01T00:00:00Z",
                parent: parent.as_deref(),
            },
        )
        .map_err(|error| error.to_string())?;
        if race_injection == "twice" || (race_injection == "once" && attempt == 0) {
            adapter.project.advance_approval_ref(slug)?;
        }
        tries += 1;
        if adapter
            .project
            .origin
            .cas_ref(&ref_name, &commit, parent.as_deref())
            .map_err(|error| error.to_string())?
        {
            won = true;
            break;
        }
        if attempt == 0 {
            parent = adapter
                .project
                .origin
                .ref_target(&ref_name)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok((tries, won))
}

fn remove(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    let next = intent_apply(&adapter.state, event)?;
    if next.get("did").and_then(Value::as_str) == Some("removed") {
        let value = Value::Object(object(event, "value")?.clone());
        let slug = string(&value, "slug")?;
        adapter.project.remove_tracked_sources(slug)?;
        adapter.project.remove_approval_ref(slug)?;
    }
    adapter.state = next;
    Ok(observe(adapter))
}

fn adopt(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    adapter.state = intent_apply(&adapter.state, event)?;
    Ok(observe(adapter))
}

fn summary_ref(summary: &ApprovalSummary) -> Value {
    json!({"n": summary.n, "hash": summary.sha})
}

fn approval_document(slug: &str, hash: &str, intent: &[u8], base: &str, by: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 2,
        "slug": slug,
        "approval_sha256": hash,
        "intent_sha256": intent_sha256(intent),
        "target_branch": "main",
        "base_sha": base,
        "domains": ["platform"],
        "acceptance_paths": [format!(".kogen/acceptance/{slug}.t.sh")],
        "protected_manifest": {},
        "check_baseline": [],
        "witness": null,
        "by": by,
        "at": "2000-01-01T00:00:00Z",
        "feasibility": "not checked"
    }))
    .unwrap_or_default()
}

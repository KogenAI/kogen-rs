use super::{Adapter, ApprovalSummary, SourceBytes, object, string, value_with};
use kogen_core::approval::replay::{self, approve_apply, approve_observe};
use kogen_core::intent::{Intent, LintSeverity, approval_sha256, intent_sha256};
use serde_json::{Map, Value, json};

pub(super) fn apply(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    if string(event, "tag")? != "Approve" {
        return Err(format!(
            "unknown approval event `{}`",
            string(event, "tag")?
        ));
    }
    let input = Value::Object(object(event, "value")?.clone());
    let derived = derive_event(adapter, &input)?;
    let mut final_value = derived.value.clone();
    let hashed = !string(&input, "given")?.is_empty();
    let preview = approve_apply(
        &adapter.state,
        &value_with(event, Value::Object(final_value.clone())),
    )?;
    let would_approve = preview.get("last").and_then(Value::as_str) == Some("ok")
        && preview
            .get("approvals")
            .and_then(Value::as_object)
            .and_then(|approvals| approvals.get(string(&input, "slug").ok()?))
            .and_then(|approval| approval.get("n"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > adapter
                .state
                .get("approvals")
                .and_then(Value::as_object)
                .and_then(|approvals| approvals.get(string(&input, "slug").ok()?))
                .and_then(|approval| approval.get("n"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
    let mut late_read_probe = final_value.clone();
    late_read_probe.insert("stableBeforeCas".to_owned(), json!(false));
    let late_read_preview = approve_apply(
        &adapter.state,
        &value_with(event, Value::Object(late_read_probe)),
    )?;
    let reaches_late_read = hashed
        && super::boolean(&Value::Object(final_value.clone()), "prefixOk")?
        && late_read_preview.get("last").and_then(Value::as_str) == Some("intent/hash_mismatch")
        && late_read_preview.get("sha8").and_then(Value::as_str)
            == Some(string(&input, "newSha8")?);

    if reaches_late_read {
        let slug = string(&input, "slug")?;
        let source = derived
            .source
            .as_ref()
            .ok_or_else(|| "approval source disappeared before commit".to_owned())?;
        let hash = approval_sha256(&source.intent, &source.acceptance);
        let commit = if would_approve {
            Some(create_approval(
                adapter,
                slug,
                source,
                &hash,
                &derived.actual_base,
                &final_value,
            )?)
        } else {
            None
        };

        // A false hand-event stability flag injects a late source mutation; the
        // transition receives only the result of rereading those actual bytes.
        let asserted_stable = super::boolean(&input, "stableBeforeCas")?;
        if !asserted_stable
            || input.get("lateIntentBytes").is_some()
            || input.get("lateAcceptanceBytes").is_some()
        {
            mutate_late_source(adapter, slug, source, &input)?;
        }
        let late = read_late_source(adapter, slug)?;
        let late_hash = late
            .as_ref()
            .map(|bytes| approval_sha256(&bytes.intent, &bytes.acceptance));
        let stable = late_hash.as_deref() == Some(hash.as_str())
            && late.as_ref().is_some_and(|bytes| {
                replay::prefix_matches(&bytes.intent, &bytes.acceptance, &derived.actual_given)
            });
        final_value.insert("stableBeforeCas".to_owned(), json!(stable));
        if stable && let Some(commit) = commit {
            cas_approval(adapter, slug, &commit)?;
        }
    }

    adapter.state = approve_apply(
        &adapter.state,
        &value_with(event, Value::Object(final_value)),
    )?;
    Ok(observe(adapter))
}

pub(super) fn observe(adapter: &Adapter) -> Value {
    let mut observation = approve_observe(&adapter.state);
    let mut approvals = observation
        .get("approvals")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Ok(summaries) = adapter.project.approval_summaries(&["alpha", "bravo"]) {
        for (slug, summary) in summaries {
            if let Some(model_approval) = approvals.get(&slug) {
                let projected = summary_approval(&summary, model_approval);
                approvals.insert(slug, projected);
            }
        }
    }
    if let Some(map) = observation.as_object_mut() {
        map.insert("approvals".to_owned(), Value::Object(approvals));
        return observation;
    }
    json!({})
}

struct DerivedEvent {
    value: Map<String, Value>,
    source: Option<SourceBytes>,
    actual_given: String,
    actual_base: String,
}

fn derive_event(adapter: &Adapter, input: &Value) -> Result<DerivedEvent, String> {
    let slug = string(input, "slug")?;
    let given = string(input, "given")?;
    let model_sha = string(input, "sha")?;
    let parse_error_input = super::boolean(input, "parseErr")?;
    let lint_error_input = super::boolean(input, "lintErr")?;
    let lint_warning_input = super::boolean(input, "lintWarn")?;
    let missing_input = super::boolean(input, "missing")?;
    let _asserted_prefix = super::boolean(input, "prefixOk")?;
    let by_bad_input = super::boolean(input, "byBad")?;
    let by = string(input, "by")?;
    let _asserted_identity = string(input, "ident")?;
    let _asserted_stability = super::boolean(input, "stableBeforeCas")?;
    let mut bytes = super::temp::default_sources(slug);
    if parse_error_input && input.get("intentBytes").is_none() {
        bytes.intent = super::temp::malformed_intent();
    } else if lint_error_input && input.get("intentBytes").is_none() {
        bytes.intent = super::temp::lint_invalid_intent(slug);
    } else if lint_warning_input && input.get("intentBytes").is_none() {
        bytes.intent = super::temp::lint_warning_intent(slug);
    }
    if let Some(intent) = input.get("intentBytes") {
        bytes.intent = bytes_value(intent, "intentBytes")?;
    }
    if let Some(acceptance) = input.get("acceptanceBytes") {
        bytes.acceptance = bytes_value(acceptance, "acceptanceBytes")?;
    }

    let source_exists = adapter.project.intent_source_exists(slug).unwrap_or(false);
    if super::temp::is_valid_slug(slug) && !source_exists {
        adapter.project.write_sources(slug, &bytes)?;
        adapter.project.persist_sources(slug)?;
    } else if super::temp::is_valid_slug(slug) {
        adapter.project.write_sources(slug, &bytes)?;
    }
    if missing_input {
        adapter.project.remove_acceptance_source(slug)?;
    }

    let actual_intent = if super::temp::is_valid_slug(slug) {
        adapter
            .project
            .read_intent(slug)
            .unwrap_or(bytes.intent.clone())
    } else {
        bytes.intent.clone()
    };
    let missing = !adapter
        .project
        .acceptance_source_exists(slug)
        .unwrap_or(false);
    let actual_acceptance = if missing {
        Vec::new()
    } else {
        adapter
            .project
            .read_acceptance(slug)
            .unwrap_or(bytes.acceptance.clone())
    };
    let actual_hash = approval_sha256(&actual_intent, &actual_acceptance);
    let actual_given = super::digest::model_prefix(&actual_hash, model_sha, given);
    let prefix_ok = !missing && actual_hash.starts_with(&actual_given);
    let parsed = Intent::parse(slug, &actual_intent);
    let parse_error = parsed.is_err();
    let lint = parsed.map(|intent| intent.lint()).unwrap_or_default();
    let lint_error = lint
        .iter()
        .any(|issue| issue.severity == LintSeverity::Error);
    let lint_warning = lint
        .iter()
        .any(|issue| issue.severity == LintSeverity::Style);
    let actual_base = adapter
        .project
        .base_commit()
        .map_err(|error| error.to_string())?;
    let mut value = input.as_object().cloned().unwrap_or_default();
    value.insert("prefixOk".to_owned(), json!(prefix_ok));
    value.insert("parseErr".to_owned(), json!(parse_error));
    value.insert("lintErr".to_owned(), json!(lint_error));
    value.insert("lintWarn".to_owned(), json!(lint_warning));
    value.insert("missing".to_owned(), json!(missing));
    value.insert(
        "byBad".to_owned(),
        json!(by_bad_input || by.contains(['\n', '\r'])),
    );
    // The initial snapshot has not yet been compared with the second source
    // read. The adapter always performs that read before CAS and records its
    // result from the actual bytes below.
    value.insert("stableBeforeCas".to_owned(), json!(true));
    let source = if missing {
        None
    } else {
        Some(SourceBytes {
            intent: actual_intent,
            acceptance: actual_acceptance,
        })
    };
    Ok(DerivedEvent {
        value,
        source,
        actual_given,
        actual_base,
    })
}

fn create_approval(
    adapter: &Adapter,
    slug: &str,
    source: &SourceBytes,
    hash: &str,
    base: &str,
    value: &Map<String, Value>,
) -> Result<String, String> {
    let value = Value::Object(value.clone());
    let given_by = string(&value, "by")?;
    let by = if given_by.trim().is_empty() {
        string(&value, "ident")?
    } else {
        given_by
    };
    let feasibility = string(&value, "feas")?;
    let bytes = approval_document(slug, hash, &source.intent, base, by, feasibility);
    let parent = adapter
        .project
        .origin
        .ref_target(&format!("refs/kogen/intents/{slug}"))
        .map_err(|error| error.to_string())?;
    replay::create_approval_commit(
        &adapter.project.origin,
        replay::ApprovalPackage {
            slug,
            intent: &source.intent,
            approval: &bytes,
            ledger: None,
            test_path: &format!(".kogen/acceptance/{slug}.t.sh"),
            acceptance: &source.acceptance,
            by,
            hash,
            at: "2000-01-01T00:00:00Z",
            parent: parent.as_deref(),
        },
    )
    .map_err(|error| error.to_string())
}

fn mutate_late_source(
    adapter: &Adapter,
    slug: &str,
    source: &SourceBytes,
    input: &Value,
) -> Result<(), String> {
    let intent = match input.get("lateIntentBytes") {
        Some(value) => bytes_value(value, "lateIntentBytes")?,
        None => {
            let mut changed = source.intent.clone();
            changed.push(b'\n');
            changed
        }
    };
    let acceptance = match input.get("lateAcceptanceBytes") {
        Some(value) => bytes_value(value, "lateAcceptanceBytes")?,
        None => source.acceptance.clone(),
    };
    adapter
        .project
        .write_sources(slug, &SourceBytes { intent, acceptance })
}

fn read_late_source(adapter: &Adapter, slug: &str) -> Result<Option<SourceBytes>, String> {
    let intent = match adapter.project.read_intent(slug) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    let acceptance = match adapter.project.read_acceptance(slug) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    Ok(Some(SourceBytes { intent, acceptance }))
}

fn cas_approval(adapter: &Adapter, slug: &str, commit: &str) -> Result<(), String> {
    let ref_name = format!("refs/kogen/intents/{slug}");
    let expected = adapter
        .project
        .origin
        .ref_target(&ref_name)
        .map_err(|error| error.to_string())?;
    if adapter
        .project
        .origin
        .cas_ref(&ref_name, commit, expected.as_deref())
        .map_err(|error| error.to_string())?
    {
        Ok(())
    } else {
        Err("approval CAS lost; approve replay has no retry transition".to_owned())
    }
}

fn bytes_value(value: &Value, field: &str) -> Result<Vec<u8>, String> {
    value
        .as_str()
        .map(|bytes| bytes.as_bytes().to_vec())
        .ok_or_else(|| format!("`{field}` must be a UTF-8 string"))
}

fn approval_document(
    slug: &str,
    hash: &str,
    intent: &[u8],
    base: &str,
    by: &str,
    feasibility: &str,
) -> Vec<u8> {
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
        "feasibility": feasibility
    }))
    .unwrap_or_default()
}

fn summary_approval(summary: &ApprovalSummary, model_approval: &Value) -> Value {
    let mut approval = model_approval.as_object().cloned().unwrap_or_default();
    approval.insert("n".to_owned(), json!(summary.n));
    approval.insert("by".to_owned(), json!(summary.by));
    approval.insert("feas".to_owned(), json!(summary.feasibility));
    Value::Object(approval)
}

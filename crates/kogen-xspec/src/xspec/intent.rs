use super::{Adapter, ApprovalAlias, boolean, object, string};
use kogen_core::error::CliOutput;
use kogen_core::intent::{Intent, approval_sha256};
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

pub(super) fn observe(adapter: &Adapter) -> Result<Value, String> {
    let summaries = adapter.project.approval_summaries(&["alpha", "bravo"])?;
    let mut refs = Map::new();
    let mut life = Map::new();
    for (slug, alias) in &adapter.approval_aliases {
        let summary = summaries
            .get(slug)
            .ok_or_else(|| format!("expected approval ref refs/kogen/intents/{slug} is missing"))?;
        if summary.sha != alias.actual_sha {
            return Err(format!(
                "approval ref {slug} now binds {}, expected {}",
                summary.sha, alias.actual_sha
            ));
        }
        if summary.commit != alias.actual_commit {
            return Err(format!(
                "approval ref {slug} moved to {}, expected {}",
                summary.commit, alias.actual_commit
            ));
        }
    }
    for slug in ["alpha", "bravo"] {
        if !adapter.project.intent_source_exists(slug)? {
            continue;
        }
        let status = if let Some(summary) = summaries.get(slug) {
            let status = adapter
                .project
                .adopted_status(slug, &summary.commit)?
                .unwrap_or_else(|| "approved".to_owned());
            let hash = adapter
                .intent_hash_aliases
                .get(slug)
                .cloned()
                .unwrap_or_else(|| summary.sha.clone());
            refs.insert(slug.to_owned(), json!({ "n": summary.n, "hash": hash }));
            status
        } else {
            "shaped".to_owned()
        };
        life.insert(slug.to_owned(), json!(status));
    }
    let mut observation = adapter.state.clone();
    let map = observation
        .as_object_mut()
        .ok_or_else(|| "intent observation state is not an object".to_owned())?;
    map.insert("refs".to_owned(), Value::Object(refs));
    map.insert("life".to_owned(), Value::Object(life));
    Ok(observation)
}

fn shape(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    let value = Value::Object(object(event, "value")?.clone());
    let slug = string(&value, "slug")?;
    let result = string(&value, "result")?;
    let current_summary = if matches!(slug, "alpha" | "bravo") {
        adapter.project.approval_summaries(&[slug])?.remove(slug)
    } else {
        None
    };
    let (last, exit, did) = if !matches!(slug, "alpha" | "bravo") {
        ("unknown_slug", 2, "")
    } else {
        match result {
            "empty" => ("intent/request_unavailable", 2, ""),
            "provider" => ("provider/overload", 4, ""),
            "failed" => ("candidate/repair_limit", 1, "shape_failed"),
            "valid" => {
                if current_summary.is_none() && !adapter.project.intent_source_exists(slug)? {
                    let source = adapter.project.reset_sources(slug)?;
                    let parsed = Intent::parse(slug, &source.intent)
                        .map_err(|error| format!("valid shape fixture did not parse: {error}"))?;
                    if parsed
                        .lint()
                        .iter()
                        .any(|issue| issue.severity == kogen_core::intent::LintSeverity::Error)
                    {
                        return Err("valid shape fixture failed production Intent lint".to_owned());
                    }
                    adapter.project.persist_sources(slug)?;
                }
                ("ok", 0, "shaped")
            }
            _ => ("unknown_result", 70, ""),
        }
    };
    set_intent_state(adapter, last, exit, did, "", None);
    observe(adapter)
}

fn approve(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    let value = Value::Object(object(event, "value")?.clone());
    let slug = string(&value, "slug")?;
    let mode = string(&value, "mode")?;
    let symbolic_hash = string(&value, "hash")?;
    if !matches!(slug, "alpha" | "bravo") {
        set_intent_state(adapter, "unknown_slug", 2, "", "", Some(0));
        return observe(adapter);
    }
    if !matches!(mode, "card" | "commit") {
        set_intent_state(adapter, "unknown_mode", 70, "", "", None);
        return observe(adapter);
    }

    let source_exists = adapter.project.intent_source_exists(slug)?;
    let source = source_exists
        .then(|| adapter.project.read_sources(slug))
        .transpose()?;
    let actual_hash = source
        .as_ref()
        .map(|source| approval_sha256(&source.intent, &source.acceptance));
    let prefix_ok = boolean(&value, "prefixOk")?;
    let actual_claim = if mode == "commit" {
        actual_hash
            .as_deref()
            .map(|hash| super::digest::modeled_claim(hash, symbolic_hash, prefix_ok))
    } else {
        None
    };
    let project = adapter.project.resolve()?;
    let race = string(&value, "race")?;
    let mut effects = IntentEffects {
        project: &adapter.project,
        race,
        attempts: 0,
    };
    let output = kogen_core::approval::approve_with_effects(
        &project,
        slug,
        actual_claim.as_deref(),
        None,
        &mut effects,
    );
    let last = last_code(&output);
    let did = match last.as_str() {
        "needs_decision" => "card",
        "ok" if mode == "commit" => "approved",
        _ => "",
    };
    if did == "approved" {
        let summary = adapter
            .project
            .approval_summaries(&[slug])?
            .remove(slug)
            .ok_or_else(|| {
                format!(
                    "production approve reported success but refs/kogen/intents/{slug} is missing"
                )
            })?;
        if actual_hash.as_deref() != Some(summary.sha.as_str()) {
            return Err(format!(
                "production approval ref {slug} binds {}, expected actual source digest {}",
                summary.sha,
                actual_hash.as_deref().unwrap_or("<missing>")
            ));
        }
        adapter
            .intent_hash_aliases
            .insert(slug.to_owned(), symbolic_hash.to_owned());
        adapter.approval_aliases.insert(
            slug.to_owned(),
            ApprovalAlias {
                sha: symbolic_hash.to_owned(),
                actual_sha: summary.sha,
                commit: String::new(),
                base: String::new(),
                actual_commit: summary.commit,
            },
        );
    } else if last == "controller/approval_cas_lost"
        && let Some(alias) = adapter.approval_aliases.get_mut(slug)
        && let Some(summary) = adapter.project.approval_summaries(&[slug])?.get(slug)
        && summary.sha == alias.actual_sha
    {
        alias.actual_commit = summary.commit.clone();
    }
    let cas_tries = match last.as_str() {
        "needs_decision" | "intent/hash_mismatch" => Some(0),
        "ok" if mode == "commit" => Some(effects.attempts),
        "controller/approval_cas_lost" => Some(effects.attempts),
        _ => None,
    };
    let shown = if matches!(
        last.as_str(),
        "needs_decision" | "ok" | "intent/hash_mismatch"
    ) {
        symbolic_hash.to_owned()
    } else {
        String::new()
    };
    set_intent_state(
        adapter,
        &last,
        output.exit_code.as_i32(),
        did,
        &shown,
        cas_tries,
    );
    observe(adapter)
}

fn remove(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    let value = Value::Object(object(event, "value")?.clone());
    let slug = string(&value, "slug")?;
    let force = boolean(&value, "force")?;
    if !matches!(slug, "alpha" | "bravo") {
        set_intent_state(adapter, "unknown_slug", 2, "", "", Some(0));
        return observe(adapter);
    }
    let project = adapter.project.resolve()?;
    let output = kogen_core::approval::remove(&project, slug, force);
    let last = last_code(&output);
    let did = if last == "ok" { "removed" } else { "" };
    if did == "removed" {
        adapter.approval_aliases.remove(slug);
        adapter.intent_hash_aliases.remove(slug);
    }
    set_intent_state(
        adapter,
        &last,
        output.exit_code.as_i32(),
        did,
        "",
        (did == "removed").then_some(0),
    );
    observe(adapter)
}

fn adopt(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    let value = Value::Object(object(event, "value")?.clone());
    let slug = string(&value, "slug")?;
    let status = string(&value, "status")?;
    if !matches!(slug, "alpha" | "bravo") {
        set_intent_state(adapter, "bad_adopt", 70, "", "", None);
        return observe(adapter);
    }
    let summary = adapter.project.approval_summaries(&[slug])?.remove(slug);
    let Some(summary) = summary else {
        set_intent_state(adapter, "bad_adopt", 70, "", "", None);
        return observe(adapter);
    };
    let previous = adapter
        .project
        .adopted_status(slug, &summary.commit)?
        .unwrap_or_else(|| "approved".to_owned());
    if !matches!(
        status,
        "building" | "failed" | "parked" | "interrupted" | "landed"
    ) || (previous != "approved" && !(previous == "building" && status != "building"))
    {
        set_intent_state(adapter, "bad_adopt", 70, "", "", None);
        return observe(adapter);
    }
    adapter.project.adopt_run_status(slug, status)?;
    set_intent_state(adapter, "ok", 0, "adopted", "", None);
    observe(adapter)
}

struct IntentEffects<'a> {
    project: &'a super::temp::TempProject,
    race: &'a str,
    attempts: u8,
}

impl kogen_core::approval::ApprovalEffects for IntentEffects<'_> {
    fn before_ref_cas(
        &mut self,
        _project: &kogen_core::project::ProjectResolution,
        slug: &str,
        attempt: u8,
        _expected: Option<&str>,
    ) -> Result<(), String> {
        self.attempts = self.attempts.max(attempt);
        if self.race == "twice" || (self.race == "once" && attempt == 1) {
            self.project.advance_approval_ref(slug)?;
        }
        Ok(())
    }
}

fn set_intent_state(
    adapter: &mut Adapter,
    last: &str,
    exit: i32,
    did: &str,
    shown: &str,
    cas_tries: Option<u8>,
) {
    let previous_cas = adapter
        .state
        .get("casTries")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let cas_tries = cas_tries.map_or(previous_cas, u64::from);
    adapter.state = json!({
        "last": last,
        "exit": exit,
        "did": did,
        "shown": shown,
        "casTries": cas_tries,
        "life": {},
        "refs": {},
    });
}

fn last_code(output: &CliOutput) -> String {
    match output.exit_code {
        kogen_core::ExitCode::Done => "ok".to_owned(),
        kogen_core::ExitCode::Decision => "needs_decision".to_owned(),
        _ => output
            .stdout
            .lines()
            .next()
            .and_then(|line| line.split_once(": ").map(|(head, _)| head.to_owned()))
            .unwrap_or_else(|| "unknown_error".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::observe;
    use crate::xspec::Adapter;
    use serde_json::json;

    #[test]
    fn observation_rejects_a_missing_approval_ref() {
        let mut adapter = Adapter::new("intent").expect("temporary intent project");
        adapter
            .handle(json!({
                "op": "apply",
                "event": {"tag": "Shape", "value": {"slug": "alpha", "result": "valid"}}
            }))
            .expect("valid production shape");
        adapter
            .handle(json!({
                "op": "apply",
                "event": {"tag": "Approve", "value": {
                    "slug": "alpha", "mode": "commit", "hash": "aaaa1111",
                    "prefixOk": true, "race": "none"
                }}
            }))
            .expect("approval through production decision and effects");

        let approval_ref = "refs/kogen/intents/alpha";
        let target = adapter
            .project
            .origin
            .ref_target(approval_ref)
            .expect("read approval ref")
            .expect("production approval created ref");
        assert!(
            adapter
                .project
                .origin
                .delete_ref_cas(approval_ref, &target)
                .expect("delete approval ref")
        );

        let error = observe(&adapter).expect_err("missing production ref must fail closed");
        assert!(error.contains("expected approval ref refs/kogen/intents/alpha is missing"));
        let error = super::super::approve::observe(&adapter)
            .expect_err("approval observation must detect its missing production ref");
        assert!(error.contains("expected approval ref refs/kogen/intents/alpha is missing"));
    }
}

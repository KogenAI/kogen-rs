use super::{Adapter, ApprovalAlias, boolean, object, string};
use crate::xspec::temp::SourceBytes;
use kogen_core::error::CliOutput;
use kogen_core::intent::approval_sha256;
use serde_json::{Map, Value, json};

pub(super) fn apply(adapter: &mut Adapter, event: &Value) -> Result<Value, String> {
    if string(event, "tag")? != "Approve" {
        return Err(format!(
            "unknown approval event `{}`",
            string(event, "tag")?
        ));
    }
    let input = Value::Object(object(event, "value")?.clone());
    let slug = string(&input, "slug")?;
    let mut source = source_for(&input, slug)?;
    if crate::xspec::temp::is_valid_slug(slug) {
        let had_intent = adapter.project.intent_source_exists(slug)?;
        if !had_intent {
            adapter.project.write_sources(slug, &source)?;
            adapter.project.persist_sources(slug)?;
        } else {
            adapter.project.write_sources(slug, &source)?;
        }
    }
    let missing_input = boolean(&input, "missing")?;
    if missing_input && crate::xspec::temp::is_valid_slug(slug) {
        adapter.project.remove_acceptance_source(slug)?;
        source.acceptance.clear();
    }

    let valid_slug = crate::xspec::temp::is_valid_slug(slug);
    let (actual_intent, actual_acceptance) = if valid_slug {
        let intent = adapter.project.read_intent(slug)?;
        let acceptance = if adapter.project.acceptance_source_exists(slug)? {
            adapter.project.read_acceptance(slug)?
        } else {
            Vec::new()
        };
        (intent, acceptance)
    } else {
        (source.intent.clone(), source.acceptance.clone())
    };
    let actual_hash = approval_sha256(&actual_intent, &actual_acceptance);
    let model_sha = string(&input, "sha")?;
    let symbolic_given = string(&input, "given")?;
    let has_given = !symbolic_given.is_empty();
    let prefix_ok = boolean(&input, "prefixOk")?;
    let actual_given = if has_given {
        Some(super::digest::modeled_claim(
            &actual_hash,
            symbolic_given,
            prefix_ok,
        ))
    } else {
        None
    };

    adapter.project.write_control_files(
        string(&input, "baseTree")?,
        string(&input, "cacheKey")?,
        string(&input, "setup")?,
        string(&input, "baseline")?,
        string(&input, "acceptance")?,
    )?;
    adapter.project.set_identity(string(&input, "ident")?)?;
    if valid_slug {
        adapter.project.set_witness_fixture(
            boolean(&input, "witnessMode")?,
            slug,
            &actual_hash,
            string(&input, "feas")?,
        )?;
    }

    let before_cache_count = adapter.project.approval_cache_count()?;
    let project = adapter.project.resolve()?;
    let by = string(&input, "by")?;
    let by = if boolean(&input, "byBad")? {
        Some("invalid\nidentity")
    } else if by.is_empty() {
        None
    } else {
        Some(by)
    };
    let mutate = !boolean(&input, "stableBeforeCas")?
        || input.get("lateIntentBytes").is_some()
        || input.get("lateAcceptanceBytes").is_some();
    let mut effects = ReplayEffects {
        project: &adapter.project,
        input: &input,
        source: &source,
        race: string(&input, "race").unwrap_or("none"),
        mutate_late_source: mutate,
        late_mutated: false,
        attempts: 0,
    };
    let output = kogen_core::approval::approve_with_effects(
        &project,
        slug,
        actual_given.as_deref(),
        by,
        &mut effects,
    );
    let last = last_code(&output);
    let exit = output.exit_code.as_i32();
    let did = if last == "needs_decision" {
        "card"
    } else if last == "ok" && has_given {
        "approved"
    } else {
        ""
    };
    let after_cache_count = adapter.project.approval_cache_count()?;
    let ran = after_cache_count > before_cache_count;
    let used_baseline = matches!(
        last.as_str(),
        "needs_decision"
            | "ok"
            | "environment/tool_missing"
            | "check/acceptance_check_failed"
            | "intent/unproven"
    ) || (last == "intent/hash_mismatch" && effects.late_mutated);
    let cache = if used_baseline {
        json!([string(&input, "baseTree")?, string(&input, "cacheKey")?])
    } else {
        adapter
            .state
            .get("cache")
            .cloned()
            .unwrap_or_else(|| json!(["", ""]))
    };

    if did == "approved" {
        let summaries = adapter.project.approval_summaries(&[slug])?;
        let summary = summaries.get(slug).ok_or_else(|| {
            format!("production approve reported success but refs/kogen/intents/{slug} is missing")
        })?;
        if summary.sha != actual_hash {
            return Err(format!(
                "production approval ref {slug} binds {}, expected actual source digest {actual_hash}",
                summary.sha
            ));
        }
        let actual_base = adapter
            .project
            .base_commit()
            .map_err(|error| error.to_string())?;
        if summary.base != actual_base {
            return Err(format!(
                "approval document records base {}, expected production base {actual_base}",
                summary.base
            ));
        }
        let base_alias = string(&input, "baseSha")?.to_owned();
        adapter.approval_aliases.insert(
            slug.to_owned(),
            ApprovalAlias {
                sha: model_sha.to_owned(),
                actual_sha: summary.sha.clone(),
                commit: string(&input, "commit")?.to_owned(),
                base: base_alias,
                actual_commit: summary.commit.clone(),
            },
        );
    } else if last == "controller/approval_cas_lost"
        && let Some(alias) = adapter.approval_aliases.get_mut(slug)
        && let Some(summary) = adapter.project.approval_summaries(&[slug])?.get(slug)
        && summary.sha == alias.actual_sha
    {
        // The losing production CAS leaves the competing ref tip in place.
        // Keep its actual tip so later observations can still verify it.
        alias.actual_commit = summary.commit.clone();
    }

    let sha8 = match last.as_str() {
        "intent/hash_mismatch" if effects.late_mutated => string(&input, "newSha8")?.to_owned(),
        "intent/hash_mismatch" if has_given => string(&input, "sha8")?.to_owned(),
        "needs_decision" | "ok" => string(&input, "sha8")?.to_owned(),
        _ => String::new(),
    };
    let approver = if last == "needs_decision" {
        output_line_value(&output.stdout, "Approver: ").unwrap_or_default()
    } else if last == "ok"
        && let Some(summary) = adapter
            .project
            .approval_summaries(&["alpha", "bravo"])?
            .get(slug)
    {
        summary.by.clone()
    } else {
        String::new()
    };
    let feasibility = if last == "needs_decision" {
        output_line_value(&output.stdout, "Feasibility: ").unwrap_or_default()
    } else if last == "ok"
        && let Some(summary) = adapter
            .project
            .approval_summaries(&["alpha", "bravo"])?
            .get(slug)
    {
        summary.feasibility.clone()
    } else {
        String::new()
    };
    let warning = output
        .stdout
        .contains("Warning: configured checks are already red on the base");
    let lint_warning = output.stdout.contains("lint_");
    adapter.state = json!({
        "last": last,
        "exit": exit,
        "sha8": sha8,
        "approver": approver,
        "feas": feasibility,
        "bwarn": warning,
        "lwarn": lint_warning,
        "ran": ran,
        "checkRuns": after_cache_count,
        "cache": cache,
        "approvals": {},
    });
    observe(adapter)
}

pub(super) fn observe(adapter: &Adapter) -> Result<Value, String> {
    let summaries = adapter.project.approval_summaries(&["alpha", "bravo"])?;
    let mut approvals = Map::new();
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
        approvals.insert(
            slug.clone(),
            json!({
                "n": summary.n,
                "sha": alias.sha,
                "by": summary.by,
                "commit": alias.commit,
                "base": alias.base,
                "feas": summary.feasibility,
            }),
        );
    }
    let mut observation = adapter.state.clone();
    observation
        .as_object_mut()
        .ok_or_else(|| "approval observation state is not an object".to_owned())?
        .insert("approvals".to_owned(), Value::Object(approvals));
    Ok(observation)
}

fn source_for(input: &Value, slug: &str) -> Result<SourceBytes, String> {
    let mut source = super::temp::default_sources(slug);
    if boolean(input, "parseErr")? && input.get("intentBytes").is_none() {
        source.intent = super::temp::malformed_intent();
    } else if boolean(input, "lintErr")? && input.get("intentBytes").is_none() {
        source.intent = super::temp::lint_invalid_intent(slug);
    } else if boolean(input, "lintWarn")? && input.get("intentBytes").is_none() {
        source.intent = super::temp::lint_warning_intent(slug);
    }
    if let Some(value) = input.get("intentBytes") {
        source.intent = bytes_value(value, "intentBytes")?;
    }
    if let Some(value) = input.get("acceptanceBytes") {
        source.acceptance = bytes_value(value, "acceptanceBytes")?;
    }
    Ok(source)
}

struct ReplayEffects<'a> {
    project: &'a super::temp::TempProject,
    input: &'a Value,
    source: &'a SourceBytes,
    race: &'a str,
    mutate_late_source: bool,
    late_mutated: bool,
    attempts: u8,
}

impl kogen_core::approval::ApprovalEffects for ReplayEffects<'_> {
    fn before_late_read(
        &mut self,
        _project: &kogen_core::project::ProjectResolution,
        slug: &str,
        attempt: u8,
    ) -> Result<(), String> {
        if !self.mutate_late_source || attempt != 1 {
            return Ok(());
        }
        let intent = match self.input.get("lateIntentBytes") {
            Some(value) => bytes_value(value, "lateIntentBytes")?,
            None => {
                let mut changed = self.source.intent.clone();
                changed.push(b'\n');
                changed
            }
        };
        let acceptance = match self.input.get("lateAcceptanceBytes") {
            Some(value) => bytes_value(value, "lateAcceptanceBytes")?,
            None => self.source.acceptance.clone(),
        };
        self.project
            .write_sources(slug, &SourceBytes { intent, acceptance })?;
        self.late_mutated = true;
        Ok(())
    }

    fn before_ref_cas(
        &mut self,
        _project: &kogen_core::project::ProjectResolution,
        slug: &str,
        attempt: u8,
        _expected: Option<&str>,
    ) -> Result<(), String> {
        self.attempts = self.attempts.max(attempt);
        let inject = self.race == "twice" || (self.race == "once" && attempt == 1);
        if inject {
            self.project.advance_approval_ref(slug)?;
        }
        Ok(())
    }
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

fn output_line_value(stdout: &str, prefix: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::to_owned))
}

fn bytes_value(value: &Value, field: &str) -> Result<Vec<u8>, String> {
    value
        .as_str()
        .map(|bytes| bytes.as_bytes().to_vec())
        .ok_or_else(|| format!("`{field}` must be a UTF-8 string"))
}

//! Hash-bound Intent approvals and immutable approval refs.

mod checks;
mod manifest;
mod model;
mod remove;
mod render;
pub mod replay;
mod support;

#[cfg(test)]
mod tests;

use crate::ExitCode;
use crate::error::{CliOutput, CoreError};
use crate::git::GitRepo;
use crate::intent::{Intent, approval_sha256, intent_sha256};
use crate::project::{ProjectResolution, valid_slug};
use checks::{check_error, run_setup_and_baseline, stage_and_check};
use manifest::{baseline_warning, protected_manifest, witness};
use model::ApprovalDocument;
use render::{ApprovalCard, render_card, render_warning_prefix};
use support::*;

const NEXT_LINE: &str = "Next: kogen queue start (does nothing if the queue is already running)";

pub(crate) fn witness_build_manifest(
    project: &ProjectResolution,
    base_sha: &str,
    intent: &Intent,
    intent_bytes: &[u8],
    acceptance_path: &str,
    acceptance_bytes: &[u8],
) -> Result<std::collections::BTreeMap<String, String>, String> {
    protected_manifest(
        project,
        base_sha,
        intent,
        intent_bytes,
        acceptance_path,
        acceptance_bytes,
    )
    .map(|manifest| manifest.hashes)
}

/// Run the public `intent approve` command against the resolved project.
pub fn approve(
    project: &ProjectResolution,
    slug: &str,
    given_hash: Option<&str>,
    by: Option<&str>,
) -> CliOutput {
    approve_with_effects(project, slug, given_hash, by, &mut NoApprovalEffects)
}

/// Effect port used to inject source changes and ref races in deterministic
/// adapter replays. Production callers use [`approve`], which supplies a
/// no-op implementation.
pub trait ApprovalEffects {
    fn before_late_read(
        &mut self,
        _project: &ProjectResolution,
        _slug: &str,
        _attempt: u8,
    ) -> Result<(), String> {
        Ok(())
    }

    fn before_ref_cas(
        &mut self,
        _project: &ProjectResolution,
        _slug: &str,
        _attempt: u8,
        _expected: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct NoApprovalEffects;

impl ApprovalEffects for NoApprovalEffects {}

/// Run the production approval decision with injected effects immediately
/// before the late source read and at the ref-CAS boundary. The policy, source
/// reread, retry count, and CAS remain in the same command path used by
/// [`approve`].
pub fn approve_with_effects(
    project: &ProjectResolution,
    slug: &str,
    given_hash: Option<&str>,
    by: Option<&str>,
    effects: &mut dyn ApprovalEffects,
) -> CliOutput {
    match approve_inner(project, slug, given_hash, by, effects) {
        Ok(output) => output,
        Err(error) => error.into_cli_output(),
    }
}

/// Run the public `intent remove` command against the resolved project.
pub fn remove(project: &ProjectResolution, slug: &str, force: bool) -> CliOutput {
    remove::remove_command(project, slug, force)
}

fn approve_inner(
    project: &ProjectResolution,
    slug: &str,
    given_hash: Option<&str>,
    by: Option<&str>,
    effects: &mut dyn ApprovalEffects,
) -> Result<CliOutput, CoreError> {
    if !valid_slug(slug) {
        return Err(intent_error(
            "invalid_slug",
            "Slug must use lowercase letters, digits, and dashes.",
            ExitCode::Usage,
        ));
    }
    if by.is_some_and(|value| value.contains(['\n', '\r'])) {
        return Err(intent_error(
            "approval_by_invalid",
            "--by must be a single line",
            ExitCode::Usage,
        ));
    }

    let intent_path = project
        .checkout
        .join(format!(".kogen/intents/{slug}/intent.md"));
    let intent_bytes = read_source(&intent_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            intent_error("not_found", "Intent does not exist", ExitCode::Usage)
        } else {
            intent_error("parse", "the Intent cannot be read", ExitCode::Negative)
        }
    })?;
    let source_path = acceptance_source_path(project, slug);

    // A supplied hash binds raw bytes before parsing, linting, identity or checks.
    let early_acceptance = if given_hash.is_some() {
        Some(read_acceptance(&source_path)?)
    } else {
        None
    };
    let initial_hash = early_acceptance
        .as_deref()
        .map(|test| approval_sha256(&intent_bytes, test));
    if let (Some(given), Some(actual)) = (given_hash, initial_hash.as_deref())
        && !replay::prefix_matches(
            &intent_bytes,
            early_acceptance.as_deref().unwrap_or_default(),
            given,
        )
    {
        return Err(hash_mismatch(slug, &actual[..8], given));
    }

    let intent = Intent::parse(slug, &intent_bytes).map_err(|error| {
        intent_error(
            "parse",
            format!("the Intent cannot be read\n{error}"),
            ExitCode::Negative,
        )
    })?;
    let lint = intent.lint();
    let lint_errors = lint
        .iter()
        .filter(|issue| issue.severity == crate::intent::LintSeverity::Error)
        .collect::<Vec<_>>();
    if !lint_errors.is_empty() {
        let mut detail = "the Intent needs changes".to_owned();
        for issue in lint_errors {
            detail.push('\n');
            detail.push_str(issue.rule);
            if let Some(line) = issue.line {
                detail.push_str(&format!(" at line {line}"));
            }
            detail.push_str(": ");
            detail.push_str(&issue.message);
        }
        return Err(intent_error("lint", detail, ExitCode::Negative));
    }

    let acceptance_bytes = match early_acceptance {
        Some(bytes) => bytes,
        None => read_acceptance(&source_path)?,
    };
    let actual_hash = approval_sha256(&intent_bytes, &acceptance_bytes);
    let identity = resolve_approver(project, by)?;
    let origin = GitRepo::new(&project.origin);
    let base_sha = origin
        .resolve_commit(&project.base)
        .map_err(|error| environment_git("base_unavailable", error))?;
    let run_dir = approval_run_dir(project, slug);
    let manifest = protected_manifest(
        project,
        &base_sha,
        &intent,
        &intent_bytes,
        &acceptance_candidate_path(project, slug),
        &acceptance_bytes,
    )
    .map_err(|detail| {
        environment_error("approval_manifest_failed", detail, ExitCode::Environment)
    })?;
    if !manifest.behind.is_empty() {
        let paths = manifest.behind.iter().take(2).cloned().collect::<Vec<_>>();
        return Err(environment_error(
            "checkout_behind_base",
            format!(
                "checkout is behind {}: {} differ; update your checkout first",
                project.base,
                paths.join(", ")
            ),
            ExitCode::Environment,
        ));
    }

    let candidate_relative = acceptance_candidate_path(project, slug);
    let candidate = project.checkout.join(&candidate_relative);
    let check_outcome =
        run_setup_and_baseline(project, &base_sha, run_dir.clone()).map_err(check_error)?;
    stage_and_check(
        project,
        &candidate,
        &candidate_relative,
        &acceptance_bytes,
        &check_outcome,
    )?;

    let mut warnings = matching_shape_warnings(project, slug, &actual_hash);
    for warning in approval_style_warnings(&intent) {
        if !warnings.iter().any(|existing| {
            existing.code == warning.code
                && existing.item_ids == warning.item_ids
                && existing.message == warning.message
        }) {
            warnings.push(warning);
        }
    }
    let has_concern = warnings
        .iter()
        .any(|warning| warning.code == "feasibility_concern");
    let (feasibility, witness_doc) =
        witness(project, slug, &base_sha, has_concern).map_err(|detail| {
            environment_error("witness_read_failed", detail, ExitCode::Environment)
        })?;
    let is_card = given_hash.is_none();
    let bwarn = baseline_warning(&check_outcome.rows);

    if is_card {
        let stdout = render_card(ApprovalCard {
            intent: &intent,
            hash: &actual_hash,
            approver: &identity,
            base: &project.base,
            base_sha: &base_sha,
            feasibility: &feasibility,
            warnings: &warnings,
            baseline_warning: bwarn,
            baseline: &check_outcome.rows,
        });
        return Ok(CliOutput {
            stdout,
            stderr: String::new(),
            exit_code: ExitCode::Decision,
        });
    }

    let ledger = matching_ledger(project, slug, &actual_hash);
    let approval = ApprovalDocument {
        schema: 2,
        slug: slug.to_owned(),
        approval_sha256: actual_hash.clone(),
        intent_sha256: intent_sha256(&intent_bytes),
        target_branch: project.base.clone(),
        base_sha: base_sha.clone(),
        domains: intent.frontmatter.domains.clone(),
        acceptance_paths: vec![source_relative_path(project, slug)],
        protected_manifest: manifest.hashes,
        check_baseline: check_outcome.rows.clone(),
        witness: witness_doc,
        by: by
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&identity)
            .to_owned(),
        at: now_rfc3339(),
    };

    let approval_bytes = serde_json::to_vec(&approval)
        .map_err(|error| controller_error("approval_serialize_failed", error.to_string()))?;
    let ref_name = format!("refs/kogen/intents/{slug}");
    let previous = origin
        .ref_target(&ref_name)
        .map_err(|error| environment_git("approval_ref_read_failed", error))?;
    let mut parent = previous.clone();
    let mut tries = 0;
    let mut committed = None;
    while tries < 2 {
        tries += 1;
        let commit = replay::create_approval_commit(
            &origin,
            replay::ApprovalPackage {
                slug,
                intent: &intent_bytes,
                approval: &approval_bytes,
                ledger: ledger.as_deref(),
                test_path: &source_relative_path(project, slug),
                acceptance: &acceptance_bytes,
                by: &approval.by,
                hash: &approval.approval_sha256,
                at: &approval.at,
                parent: parent.as_deref(),
            },
        )
        .map_err(|error| environment_git("approval_commit_failed", error))?;
        effects
            .before_late_read(project, slug, tries)
            .map_err(|detail| controller_error("approval_effect_failed", detail))?;
        let late_intent = read_source(&intent_path)
            .map_err(|_| hash_mismatch(slug, "unavailable", given_hash.unwrap_or_default()))?;
        let late_acceptance = read_source(&source_path)
            .map_err(|_| hash_mismatch(slug, "unavailable", given_hash.unwrap_or_default()))?;
        let late_hash = approval_sha256(&late_intent, &late_acceptance);
        if late_hash != actual_hash
            || !replay::prefix_matches(
                &late_intent,
                &late_acceptance,
                given_hash.unwrap_or_default(),
            )
        {
            return Err(hash_mismatch(
                slug,
                &late_hash[..8],
                given_hash.unwrap_or_default(),
            ));
        }
        if project_uses_witness(project) && approval.witness.is_none() {
            return Err(intent_error(
                "unproven",
                "the witness is not proven",
                ExitCode::Negative,
            ));
        }
        effects
            .before_ref_cas(project, slug, tries, parent.as_deref())
            .map_err(|detail| controller_error("approval_effect_failed", detail))?;
        if origin
            .cas_ref(&ref_name, &commit, parent.as_deref())
            .map_err(|error| environment_git("approval_ref_update_failed", error))?
        {
            committed = Some(commit);
            break;
        }
        if tries == 2 {
            break;
        }
        parent = origin
            .ref_target(&ref_name)
            .map_err(|error| environment_git("approval_ref_read_failed", error))?;
    }
    let Some(commit) = committed else {
        return Err(controller_error(
            "approval_cas_lost",
            "approval ref changed twice while approving; review and retry",
        ));
    };
    let mut stdout = render_warning_prefix(bwarn, &check_outcome.rows, &warnings);
    if !stdout.is_empty() {
        stdout.push('\n');
    }
    stdout.push_str(&format!(
        "approved {slug} {} (approval {}); it is queued\n{NEXT_LINE}\n",
        &actual_hash[..8],
        &commit[..8]
    ));
    Ok(CliOutput::success(stdout))
}

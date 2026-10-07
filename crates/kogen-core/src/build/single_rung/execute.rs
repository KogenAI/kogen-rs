use super::super::approval::ApprovedBuild;
use super::super::config::BuildOptions;
use super::super::provider::BuildProvider;
use super::super::support;
use super::super::{controller_error, environment_error};
use super::{BuildOutcome, BuildStatus};
mod helpers;
mod start;
use crate::error::CoreError;
use crate::git::landing::LandingRepository;
use crate::project::ProjectResolution;
use crate::run::{RunEvent, RunSnapshot, RunStore};
use helpers::*;
use serde_json::{Value, json};
pub(super) use start::run;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
fn run_rung(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
    run_dir: &Path,
    candidate_path: &Path,
    base_path: &Path,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
) -> Result<BuildOutcome, CoreError> {
    let candidate = LandingRepository::clone_fresh(&project.origin, candidate_path, base_sha)
        .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
    let base = LandingRepository::clone_fresh(&project.origin, base_path, base_sha)
        .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
    let integrity = support::IntegritySnapshot::new(project);
    let supervisor = crate::run::ProcessSupervisor;
    let builder_runner = support::sandboxed(
        project,
        options,
        candidate.workspace(),
        run_dir,
        &supervisor,
        &integrity,
    );
    let base_runner = support::sandboxed(
        project,
        options,
        base.workspace(),
        run_dir,
        &supervisor,
        &integrity,
    );
    let child_env = support::child_environment(project, run_dir, candidate.workspace(), options)?;
    let base_env = support::child_environment(project, run_dir, base.workspace(), options)?;
    let sandbox = probe_sandbox(&builder_runner, candidate.workspace(), run_dir, &child_env)?;
    let sandbox_warning = sandbox_warning(&sandbox);
    snapshot
        .fields
        .insert("sandbox".to_owned(), json!(sandbox.status.as_str()));
    snapshot
        .fields
        .insert("sandbox_warning".to_owned(), json!(sandbox_warning));
    let mut provider = BuildProvider::new(project, approved, options, store, snapshot)?;
    let account = provider.account().clone();
    record_started(
        &mut provider,
        approved,
        options,
        base_sha,
        &account,
        sandbox.status.as_str(),
    )?;
    if let Some(reason) = sandbox
        .warning_reason
        .as_deref()
        .filter(|_| sandbox.status == crate::run::SandboxStatus::Unconfined)
    {
        provider.record_event(
            &RunEvent::new("sandbox_unavailable", now_ms()).with("reason", json!(reason)),
        )?;
    }
    let _interrupt = super::super::interrupt::InterruptMonitor::install(store.clone())?;
    if base_sha != approved.base_sha {
        provider.record_event(
            &RunEvent::new("base_moved_at_start", now_ms())
                .with("approved", json!(approved.base_sha))
                .with("tip", json!(base_sha))
                .with(
                    "ancestor",
                    json!(is_ancestor(project, &approved.base_sha, base_sha)),
                ),
        )?;
    }

    let no_plan = matches!(
        options.recipe.as_str(),
        "direct" | "direct-escalate" | "direct-shell" | "escalate-shell"
    );
    let (plan, _plan_wall_ms) = if no_plan {
        ("easy\0".to_owned(), 0)
    } else {
        let plan_files = tracked_paths(project, base_sha)?;
        provider.plan(run_dir, &plan_files)?
    };
    let (_, plan_text) = plan.split_once('\0').unwrap_or(("easy", plan.as_str()));
    provider.record_event(
        &RunEvent::new("rung_started", now_ms())
            .with("rung", json!("R1"))
            .with("model", json!(options.builder_model))
            .with("effort", json!(options.builder_effort))
            .with("entered_because", json!("single_rung"))
            .with("wall_ms", json!(options.wall_ms)),
    )?;
    for (workspace, runner, environment) in [
        (
            candidate.workspace(),
            &builder_runner as &dyn crate::run::ProcessPort,
            &child_env,
        ),
        (
            base.workspace(),
            &base_runner as &dyn crate::run::ProcessPort,
            &base_env,
        ),
    ] {
        let (key, outcome) =
            support::run_setup_cached(project, options, runner, workspace, run_dir, environment)?;
        if outcome.reused {
            provider.record_event(
                &RunEvent::new("setup_reused", now_ms())
                    .with("setup_key", json!(key))
                    .with("saved_wall_ms", json!(outcome.saved_wall_ms)),
            )?;
        }
    }

    install_approved(candidate.workspace(), approved, options)?;
    let base_acceptance = support::base_acceptance(
        &base_runner,
        options,
        approved,
        base.workspace(),
        run_dir,
        &base_env,
    )?;
    if base_acceptance
        .failures
        .contains(&crate::gate::AcceptanceFailure::ToolMissing)
    {
        return Err(environment_error(
            "tool_missing",
            "acceptance runner is unavailable on the build base",
        ));
    }
    let acceptance_text = base_acceptance_text(approved, &base_acceptance);
    provider.record_event(
        &RunEvent::new("base_acceptance", now_ms()).with(
            "items",
            json!(approved
                .intent
                .verify
                .iter()
                .map(|item| json!({
                    "id":item.id,
                    "kind":item.kind().unwrap_or("test"),
                    "base_status":if base_acceptance.item_pass.get(&item.id) == Some(&true) {"passed"} else {"failed"}
                }))
                .collect::<Vec<_>>()),
        ),
    )?;

    let protection = support::protected_workspace(project, approved, options, base_sha)?;
    let develop = provider.develop(
        run_dir,
        candidate.workspace(),
        plan_text,
        &acceptance_text,
        &builder_runner,
        child_env.clone(),
        &protection,
        matches!(options.recipe.as_str(), "direct" | "direct-escalate"),
    )?;
    drop(provider);
    let report = run_gate(
        project,
        approved,
        options,
        run_dir,
        base.workspace(),
        candidate.workspace(),
        &supervisor,
        &integrity,
        &child_env,
        protection,
    )?;
    let tree = report.verified_tree.clone().unwrap_or(
        crate::gate::snapshot_tree(candidate.workspace())
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?,
    );
    let verdict = if report.is_landable() {
        "green"
    } else {
        "unverified"
    };
    let diff = candidate_diff(candidate.workspace(), base_sha)?;
    write_private(&run_dir.join("candidate-R1.diff"), &diff)?;
    write_private(&run_dir.join("candidate.diff"), &diff)?;
    let setup_outputs = options
        .setup_outputs
        .iter()
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    let candidate_commit = candidate
        .candidate_commit_excluding(
            base_sha,
            &tree,
            &approved.intent.frontmatter.title,
            &approved.slug,
            &setup_outputs,
        )
        .map_err(|error| controller_error("candidate_commit_failed", error.to_string()))?;
    publish_candidate(
        candidate.workspace(),
        &candidate_commit.commit,
        &snapshot.run_id,
    )?;
    snapshot.fields.insert("verdict".to_owned(), json!(verdict));
    snapshot.fields.insert(
        "candidate_commit".to_owned(),
        json!(candidate_commit.commit),
    );
    record(
        store,
        snapshot,
        RunEvent::new("verification", now_ms())
            .with("rung", json!("R1"))
            .with("tree", json!(tree))
            .with("result", json!(verdict))
            .with("checks", checks_json(&report))
            .with("acceptance", acceptance_json(&report))
            .with("count", json!(usize::from(!report.is_landable()))),
    )?;
    record(
        store,
        snapshot,
        RunEvent::new("rung_finished", now_ms())
            .with("rung", json!("R1"))
            .with(
                "reason",
                json!(if report.is_landable() {
                    &develop.reason
                } else {
                    "verification_red"
                }),
            )
            .with("verdict", json!(verdict))
            .with("tree", json!(tree))
            .with("turns", json!(develop.turns))
            .with("changed", json!(develop.changed))
            .with("tool_calls", json!(develop.tool_outputs.len()))
            .with("model_stages", json!(develop.model_stages)),
    )?;
    record(
        store,
        snapshot,
        RunEvent::new("commit_result", now_ms())
            .with("rung", json!("R1"))
            .with("candidate_commit", json!(candidate_commit.commit))
            .with("tree", json!(tree))
            .with("verdict", json!(verdict)),
    )?;

    if !report.is_landable() {
        candidate
            .park(&snapshot.run_id, &candidate_commit.commit)
            .map_err(|error| controller_error("candidate_park_failed", error.to_string()))?;
        snapshot.status = "failed".to_owned();
        record(
            store,
            snapshot,
            RunEvent::new("selection", now_ms())
                .with("rung", json!("R1"))
                .with("verdict", json!(verdict)),
        )?;
        record(
            store,
            snapshot,
            RunEvent::new("finished", now_ms())
                .with("status", json!("failed"))
                .with(
                    "reason",
                    json!(if develop.reason == "finish" {
                        "verification_red"
                    } else {
                        &develop.reason
                    }),
                ),
        )?;
        cleanup_path(base.workspace());
        cleanup_path(candidate.workspace());
        return Ok(BuildOutcome {
            status: BuildStatus::Failed,
            run_id: snapshot.run_id.clone(),
            commit: candidate_commit.commit,
            verdict: verdict.to_owned(),
            reason: if develop.reason == "finish" {
                "verification_red".to_owned()
            } else {
                develop.reason
            },
            stderr: sandbox_warning.clone(),
            has_run: true,
        });
    }

    let result = super::landing::land_candidate(
        project,
        approved,
        options,
        store,
        snapshot,
        &candidate,
        base_sha,
        &tree,
        &integrity,
        &supervisor,
    )?;
    cleanup_path(base.workspace());
    match result.outcome {
        crate::git::landing::LandingOutcome::Landed {
            commit, warnings, ..
        } => Ok(BuildOutcome {
            status: BuildStatus::Landed,
            run_id: snapshot.run_id.clone(),
            commit,
            verdict: "green".to_owned(),
            reason: String::new(),
            stderr: [sandbox_warning, warnings.join("\n")]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            has_run: true,
        }),
        crate::git::landing::LandingOutcome::Parked { commit, reason, .. } => {
            snapshot.status = "parked".to_owned();
            cleanup_path(candidate.workspace());
            Ok(BuildOutcome {
                status: BuildStatus::Parked,
                run_id: snapshot.run_id.clone(),
                commit,
                verdict: verdict.to_owned(),
                reason,
                stderr: sandbox_warning.clone(),
                has_run: true,
            })
        }
        crate::git::landing::LandingOutcome::Stopped { reason, .. } => {
            snapshot.status = "stopped".to_owned();
            cleanup_path(candidate.workspace());
            Ok(BuildOutcome {
                status: BuildStatus::Stopped {
                    class: "controller".to_owned(),
                    reason: reason.clone(),
                },
                run_id: snapshot.run_id.clone(),
                commit: candidate_commit.commit,
                verdict: verdict.to_owned(),
                reason,
                stderr: sandbox_warning,
                has_run: true,
            })
        }
    }
}

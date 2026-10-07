use super::super::approval::ApprovedBuild;
use super::super::config::BuildOptions;
use super::super::provider::BuildProvider;
use super::super::support;
use super::super::{controller_error, environment_error};
use super::{BuildOutcome, BuildStatus};
mod helpers;
mod parallel;
mod start;
use crate::error::CoreError;
use crate::git::landing::LandingRepository;
use crate::project::ProjectResolution;
use crate::run::orchestration::BuildAuditor;
use crate::run::{RunEvent, RunSnapshot, RunStore};
use helpers::*;
use serde_json::{Value, json};
pub(super) use start::run;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

#[derive(Clone)]
struct CandidateSnapshot {
    rung: String,
    rung_number: u8,
    commit: String,
    tree: String,
    verdict: String,
    reason: String,
    pass_count: usize,
    blocking_count: usize,
    diff_lines: usize,
    diff: Vec<u8>,
}

impl CandidateSnapshot {
    #[allow(clippy::too_many_arguments)]
    fn from_report(
        rung: &str,
        rung_number: u8,
        commit: String,
        tree: String,
        verdict: &str,
        reason: &str,
        report: &crate::gate::GateReport,
        demoted: &BTreeSet<String>,
        diff: Vec<u8>,
    ) -> Self {
        Self {
            rung: rung.to_owned(),
            rung_number,
            commit,
            tree,
            verdict: verdict.to_owned(),
            reason: reason.to_owned(),
            pass_count: report
                .acceptance
                .item_pass
                .iter()
                .filter(|(id, passed)| **passed && !demoted.contains(*id))
                .count(),
            blocking_count: red_count(report, demoted),
            diff_lines: changed_line_count(&diff),
            diff,
        }
    }
}

fn changed_line_count(diff: &[u8]) -> usize {
    String::from_utf8_lossy(diff)
        .lines()
        .filter(|line| {
            (line.starts_with('+') && !line.starts_with("+++"))
                || (line.starts_with('-') && !line.starts_with("---"))
        })
        .count()
}

fn stage_wall_ms(started: Instant, configured_ms: u64) -> u64 {
    configured_ms
        .saturating_sub(started.elapsed().as_millis() as u64)
        .min(1_800_000)
}

fn landing_stderr(sandbox_warning: &str, warnings: &[String]) -> String {
    let mut stderr = sandbox_warning.to_owned();
    for warning in warnings {
        stderr.push_str(warning.trim_end_matches('\n'));
        stderr.push('\n');
    }
    stderr
}

fn gate_end_reason(report: &crate::gate::GateReport, develop_reason: &str) -> String {
    if matches!(
        develop_reason,
        "turn_cap" | "budget" | "protected_restore_limit"
    ) {
        develop_reason.to_owned()
    } else if report.is_landable() {
        "green".to_owned()
    } else {
        "verification_red".to_owned()
    }
}

fn advisory_failures(
    report: &crate::gate::GateReport,
    demoted: &BTreeSet<String>,
) -> BTreeSet<String> {
    report
        .acceptance
        .item_pass
        .iter()
        .filter_map(|(id, passed)| (!passed && demoted.contains(id)).then_some(id.clone()))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn run_candidate_gate(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    run_dir: &Path,
    base: &Path,
    candidate: &LandingRepository,
    supervisor: &crate::run::ProcessSupervisor,
    integrity: &support::IntegritySnapshot,
    environment: &crate::run::ChildEnvironment,
    protection: crate::gate::ProtectedWorkspace,
) -> Result<crate::gate::GateReport, CoreError> {
    candidate
        .reset_workspace_git_settings()
        .map_err(|error| controller_error("workspace_git_config_failed", error.to_string()))?;
    let report = run_gate(
        project,
        approved,
        options,
        run_dir,
        base,
        candidate.workspace(),
        supervisor,
        integrity,
        environment,
        protection,
    )?;
    candidate
        .reset_workspace_git_settings()
        .map_err(|error| controller_error("workspace_git_config_failed", error.to_string()))?;
    Ok(report)
}

fn record_verification(
    provider: &mut BuildProvider<'_>,
    snapshot: &mut RunSnapshot,
    report: &crate::gate::GateReport,
    rung: &str,
    demoted: &BTreeSet<String>,
) -> Result<(), CoreError> {
    provider.record_event(
        snapshot,
        &RunEvent::new("verification", now_ms())
            .with("rung", json!(rung))
            .with("tree", json!(report.verified_tree.clone()))
            .with(
                "result",
                json!(if report.is_landable() { "green" } else { "red" }),
            )
            .with("checks", checks_json(report))
            .with("acceptance", acceptance_json(report, demoted))
            .with("count", json!(support::gate_failure_count(report)))
            .with("blocking_count", json!(red_count(report, demoted))),
    )
}

fn candidate_ranking(candidates: &[CandidateSnapshot]) -> Vec<Value> {
    let mut ranked = candidates.iter().collect::<Vec<_>>();
    ranked.sort_by_key(|candidate| {
        (
            std::cmp::Reverse(candidate.pass_count),
            candidate.blocking_count,
            candidate.diff_lines,
            candidate.rung_number,
        )
    });
    ranked
        .into_iter()
        .map(|candidate| {
            json!({
                "rung": candidate.rung,
                "verdict": candidate.verdict,
                "passing_items": candidate.pass_count,
                "blocking_findings": candidate.blocking_count,
                "diff_lines": candidate.diff_lines,
            })
        })
        .collect()
}

fn best_candidate(candidates: &[CandidateSnapshot]) -> Option<&CandidateSnapshot> {
    candidates.iter().min_by_key(|candidate| {
        (
            std::cmp::Reverse(candidate.pass_count),
            candidate.blocking_count,
            candidate.diff_lines,
            candidate.rung_number,
        )
    })
}

fn first_message_with_history(
    approved: &ApprovedBuild,
    acceptance: &str,
    plan: &str,
    prior: &[String],
) -> String {
    let message =
        super::super::provider_prompt::builder_message(&approved.intent_bytes, acceptance, plan);
    if prior.is_empty() {
        return message;
    }
    let summary = prior
        .iter()
        .flat_map(|value| value.lines())
        .take(5)
        .map(|line| line.chars().take(180).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    message.replace(
        "\n\nRepairs available: 6.",
        &format!("\n\nEarlier attempts:\n{summary}\n\nRepairs available: 6."),
    )
}

#[allow(clippy::too_many_arguments)]
fn audit_acceptance(
    provider: &mut BuildProvider<'_>,
    snapshot: &mut RunSnapshot,
    auditor: &mut BuildAuditor,
    approved: &ApprovedBuild,
    base_sha: &str,
    run_dir: &Path,
    workspace: &Path,
    report: &mut crate::gate::GateReport,
    rung: &str,
    demoted: &mut BTreeSet<String>,
) -> Result<(), CoreError> {
    let failed_ids = report
        .acceptance
        .item_pass
        .iter()
        .filter_map(|(id, passed)| (!passed).then_some(id.clone()))
        .collect::<Vec<_>>();
    let only_acceptance_failures = report.verdict == crate::gate::GateVerdict::Unverified
        && report.verified_tree.is_some()
        && acceptance_only_red(report)
        && report.acceptance.failures.is_empty()
        && !failed_ids.is_empty();
    let witness_mode = approved
        .approval
        .get("witness")
        .is_some_and(|value| !value.is_null());
    let pending = auditor.begin_rung_audit(&failed_ids, only_acceptance_failures, witness_mode);
    if pending.is_empty() {
        return Ok(());
    }

    let tree = report
        .verified_tree
        .as_deref()
        .expect("audit requires a verified tree");
    let diff = candidate_diff(workspace, base_sha, tree)?;
    let request = crate::run::orchestration::BuildAuditRequest {
        ids: pending.clone(),
        request: approved.intent.request.clone().unwrap_or_default(),
        test_source: String::from_utf8_lossy(&approved.acceptance_bytes).into_owned(),
        failure_output: String::from_utf8_lossy(&report.acceptance.process.output_tail)
            .into_owned(),
        candidate_diff: String::from_utf8_lossy(&diff).into_owned(),
    };
    let reply = provider.audit(snapshot, run_dir, rung, &request)?;
    let dispositions = auditor.complete_rung_audit(&pending, &reply);
    provider.record_event(
        snapshot,
        &RunEvent::new("audit", now_ms())
            .with("rung", json!(rung))
            .with(
                "items",
                json!(
                    dispositions
                        .iter()
                        .map(|item| json!({
                            "id": item.id,
                            "verdict": item.verdict.as_str(),
                            "reason": item.reason,
                        }))
                        .collect::<Vec<_>>()
                ),
            ),
    )?;
    let demoted_ids = dispositions
        .iter()
        .filter(|item| item.demote)
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let had_demotion = !demoted_ids.is_empty();
    demoted.extend(demoted_ids.iter().cloned());
    report.apply_audit_demotions(&demoted_ids);
    for item in dispositions {
        let event = if item.demote {
            "acceptance_demoted"
        } else {
            "acceptance_upheld"
        };
        provider.record_event(
            snapshot,
            &RunEvent::new(event, now_ms())
                .with("rung", json!(rung))
                .with("id", json!(item.id))
                .with("verdict", json!(item.verdict.as_str()))
                .with("reason", json!(item.reason)),
        )?;
    }
    if had_demotion {
        record_verification(provider, snapshot, report, rung, demoted)?;
    }
    Ok(())
}

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
    let build_started = Instant::now();
    let last_rung = if options.experimental_r4 {
        options.max_rungs
    } else {
        options.max_rungs.min(3)
    };
    let mut candidates = Vec::<CandidateSnapshot>::new();
    let mut auditor = BuildAuditor::default();
    let excluded_paths = progress_exclusions(approved, options);
    let mut candidate =
        LandingRepository::clone_fresh(&project.origin, candidate_path, base_sha)
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
    let run_id = snapshot.run_id.clone();
    let mut provider = BuildProvider::new(project, approved, options, store)?;
    let account = provider.account().clone();
    record_started(
        &mut provider,
        snapshot,
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
            snapshot,
            &RunEvent::new("sandbox_unavailable", now_ms()).with("reason", json!(reason)),
        )?;
    }
    let _interrupt = super::super::interrupt::InterruptMonitor::install(store.clone())?;
    if base_sha != approved.base_sha {
        provider.record_event(
            snapshot,
            &RunEvent::new("base_moved_at_start", now_ms())
                .with("approved", json!(approved.base_sha))
                .with("tip", json!(base_sha))
                .with(
                    "ancestor",
                    json!(is_ancestor(project, &approved.base_sha, base_sha)),
                ),
        )?;
    }

    if let Some(witness) = try_witness(
        project,
        approved,
        options,
        base_sha,
        run_dir,
        &integrity,
        &supervisor,
        &mut provider,
        snapshot,
        &run_id,
    )? {
        cleanup_path(base.workspace());
        cleanup_path(candidate.workspace());
        let result = super::landing::land_candidate(
            project,
            approved,
            options,
            store,
            snapshot,
            &witness.candidate,
            base_sha,
            &witness.tree,
            &integrity,
            &supervisor,
            &mut provider,
            &witness.tree,
            &excluded_paths,
        )?;
        cleanup_path(witness.candidate.workspace());
        return Ok(match result.outcome {
            crate::git::landing::LandingOutcome::Landed {
                commit, warnings, ..
            } => BuildOutcome {
                status: BuildStatus::Landed,
                run_id: run_id.clone(),
                commit,
                verdict: "green".to_owned(),
                advisory_items: Vec::new(),
                reason: String::new(),
                stderr: landing_stderr(&sandbox_warning, &warnings),
                has_run: true,
            },
            crate::git::landing::LandingOutcome::Parked { commit, reason, .. } => BuildOutcome {
                status: BuildStatus::Parked,
                run_id: run_id.clone(),
                commit,
                verdict: "green".to_owned(),
                advisory_items: Vec::new(),
                reason,
                stderr: sandbox_warning,
                has_run: true,
            },
            crate::git::landing::LandingOutcome::Stopped { reason, .. } => BuildOutcome {
                status: BuildStatus::Stopped {
                    class: "controller".to_owned(),
                    reason: reason.clone(),
                },
                run_id: run_id.clone(),
                commit: witness.commit,
                verdict: "green".to_owned(),
                advisory_items: Vec::new(),
                reason,
                stderr: sandbox_warning,
                has_run: true,
            },
        });
    }

    let no_plan = matches!(
        options.recipe.as_str(),
        "direct" | "direct-escalate" | "direct-shell" | "escalate-shell"
    );
    let (plan, _plan_wall_ms) = if no_plan {
        ("easy\0".to_owned(), 0)
    } else {
        let plan_files = tracked_paths(project, base_sha)?;
        provider.plan(snapshot, run_dir, &plan_files)?
    };
    let (difficulty, plan_text) = plan.split_once('\0').unwrap_or(("easy", plan.as_str()));
    let hard_parallel = difficulty == "hard"
        && crate::run::orchestration::BuildRecipe::parse(&options.recipe)
            .ok()
            .is_some_and(|recipe| {
                recipe.kind == crate::run::orchestration::RecipeKind::Ladder
                    && matches!(
                        recipe.entry_schedule(
                            crate::run::orchestration::Difficulty::Hard,
                            false,
                            options.max_rungs,
                        ),
                        crate::run::orchestration::RungSchedule::Parallel(_)
                    )
            });
    if hard_parallel {
        return parallel::run(
            project,
            approved,
            options,
            base_sha,
            run_dir,
            candidate,
            base,
            store,
            snapshot,
            provider,
            plan_text,
            build_started,
            &integrity,
            &supervisor,
            &sandbox_warning,
        );
    }
    provider.record_event(
        snapshot,
        &RunEvent::new("rung_started", now_ms())
            .with("rung", json!("R1"))
            .with("model", json!(options.builder_model))
            .with("effort", json!(options.builder_effort))
            .with("entered_because", json!("single_rung"))
            .with(
                "wall_ms",
                json!(stage_wall_ms(build_started, options.wall_ms)),
            ),
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
                snapshot,
                &RunEvent::new("setup_reused", now_ms())
                    .with("setup_key", json!(key))
                    .with("saved_wall_ms", json!(outcome.saved_wall_ms)),
            )?;
        }
    }

    install_approved(candidate.workspace(), approved, options)?;
    let baseline_tree =
        crate::gate::snapshot_tree_excluding(candidate.workspace(), &excluded_paths)
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?;
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
    provider.record_event(snapshot,
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
    let mut develop = provider.develop(
        snapshot,
        run_dir,
        candidate.workspace(),
        &baseline_tree,
        &excluded_paths,
        plan_text,
        &acceptance_text,
        &builder_runner,
        child_env.clone(),
        &protection,
        matches!(options.recipe.as_str(), "direct" | "direct-escalate"),
    )?;
    let mut report = run_candidate_gate(
        project,
        approved,
        options,
        run_dir,
        base.workspace(),
        &candidate,
        &supervisor,
        &integrity,
        &child_env,
        protection.clone(),
    )?;
    let mut demoted = BTreeSet::new();
    record_verification(&mut provider, snapshot, &report, "R1", &demoted)?;
    audit_acceptance(
        &mut provider,
        snapshot,
        &mut auditor,
        approved,
        base_sha,
        run_dir,
        candidate.workspace(),
        &mut report,
        "R1",
        &mut demoted,
    )?;
    let mut repairs = 0_u8;
    let mut current_rung = "R1".to_owned();
    let mut rung_reason = gate_end_reason(&report, &develop.reason);
    let mut previous_red_count = None;
    while !report.is_landable()
        && repairs < 6
        && rung_reason != "budget"
        && rung_reason != "protected_restore_limit"
    {
        let count = red_count(&report, &demoted);
        if previous_red_count.is_some_and(|previous| count >= previous) {
            rung_reason = "no_progress".to_owned();
            break;
        }
        previous_red_count = Some(count);
        let before = candidate_working_diff(candidate.workspace(), base_sha)?;
        let feedback = support::gate_feedback(approved, &report, run_dir);
        provider.record_event(
            snapshot,
            &RunEvent::new("repair", now_ms())
                .with("rung", json!("R1"))
                .with("number", json!(repairs + 1))
                .with("count", json!(count)),
        )?;
        let repaired = provider.develop_on_rung(
            snapshot,
            run_dir,
            candidate.workspace(),
            "R1",
            &options.builder_model,
            &options.builder_effort,
            "",
            Some(&feedback),
            &baseline_tree,
            &excluded_paths,
            &builder_runner,
            child_env.clone(),
            &protection,
            matches!(options.recipe.as_str(), "direct" | "direct-escalate"),
            None,
        )?;
        repairs = repairs.saturating_add(1);
        develop.turns = repaired.turns;
        develop.changed |= repaired.changed;
        develop.tool_outputs.extend(repaired.tool_outputs);
        develop.model_stages = develop.model_stages.saturating_add(repaired.model_stages);
        report = run_candidate_gate(
            project,
            approved,
            options,
            run_dir,
            base.workspace(),
            &candidate,
            &supervisor,
            &integrity,
            &child_env,
            protection.clone(),
        )?;
        record_verification(&mut provider, snapshot, &report, "R1", &demoted)?;
        audit_acceptance(
            &mut provider,
            snapshot,
            &mut auditor,
            approved,
            base_sha,
            run_dir,
            candidate.workspace(),
            &mut report,
            "R1",
            &mut demoted,
        )?;
        if repaired.reason == "turn_cap" {
            rung_reason = "turn_cap".to_owned();
            break;
        }
        if repaired.reason == "protected_restore_limit" {
            rung_reason = "protected_restore_limit".to_owned();
            break;
        }
        if repaired.reason == "budget" {
            rung_reason = "budget".to_owned();
            break;
        }
        if !repaired.progressed {
            rung_reason = "unchanged".to_owned();
            break;
        }
        let after = candidate_working_diff(candidate.workspace(), base_sha)?;
        if before == after {
            rung_reason = "unchanged".to_owned();
            break;
        }
        rung_reason = gate_end_reason(&report, &repaired.reason);
    }
    if repairs == 6 && !report.is_landable() {
        rung_reason = "repair_cap".to_owned();
    }

    let mut advisory = advisory_failures(&report, &demoted);
    let mut landable =
        report.is_landable() && (advisory.is_empty() || options.land_policy != "green");
    if !landable && last_rung >= 2 && rung_reason != "budget" {
        let r1_tree = report.verified_tree.clone().unwrap_or(
            crate::gate::snapshot_tree(candidate.workspace()).map_err(|error| {
                environment_error("candidate_snapshot_failed", error.to_string())
            })?,
        );
        let r1_diff = candidate_diff(candidate.workspace(), base_sha, &r1_tree)?;
        write_private(&run_dir.join("candidate-R1.diff"), &r1_diff)?;
        let setup_outputs = options
            .setup_outputs
            .iter()
            .map(std::path::PathBuf::from)
            .collect::<Vec<_>>();
        let r1_commit = candidate
            .candidate_commit_excluding(
                base_sha,
                &r1_tree,
                &approved.intent.frontmatter.title,
                &approved.slug,
                &setup_outputs,
            )
            .map_err(|error| controller_error("candidate_commit_failed", error.to_string()))?;
        publish_candidate(
            candidate.workspace(),
            candidate.origin(),
            &r1_commit.commit,
            &run_id,
            "R1",
        )?;
        let r1_advisory = advisory_failures(&report, &demoted);
        let r1_verdict = if report.is_landable() && !r1_advisory.is_empty() {
            "green-with-advisory-tests"
        } else if report.is_landable() {
            "green"
        } else {
            "unverified"
        };
        candidates.push(CandidateSnapshot::from_report(
            "R1",
            1,
            r1_commit.commit.clone(),
            r1_tree.clone(),
            r1_verdict,
            &rung_reason,
            &report,
            &demoted,
            r1_diff.clone(),
        ));
        for event in [
            RunEvent::new("verification", now_ms())
                .with("rung", json!("R1"))
                .with("tree", json!(r1_tree.clone()))
                .with("result", json!(r1_verdict))
                .with("checks", checks_json(&report))
                .with("acceptance", acceptance_json(&report, &demoted))
                .with("count", json!(support::gate_failure_count(&report)))
                .with("blocking_count", json!(red_count(&report, &demoted))),
            RunEvent::new("rung_finished", now_ms())
                .with("rung", json!("R1"))
                .with("reason", json!(rung_reason.clone()))
                .with("verdict", json!(r1_verdict))
                .with("tree", json!(r1_tree.clone()))
                .with(
                    "candidate_ref",
                    json!(format!("refs/kogen/candidates/{run_id}/R1")),
                )
                .with("diff_lines", json!(changed_line_count(&r1_diff)))
                .with("turns", json!(develop.turns))
                .with("changed", json!(develop.changed))
                .with("repairs", json!(repairs))
                .with("tool_calls", json!(develop.tool_outputs.len()))
                .with("model_stages", json!(develop.model_stages)),
        ] {
            provider.record_event(snapshot, &event)?;
        }

        let r2_path = project.state_root.join(format!("{run_id}-R2"));
        cleanup_path(candidate.workspace());
        candidate = LandingRepository::clone_fresh(&project.origin, &r2_path, base_sha)
            .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
        let r2_runner = support::sandboxed(
            project,
            options,
            candidate.workspace(),
            run_dir,
            &supervisor,
            &integrity,
        );
        let r2_env = support::child_environment(project, run_dir, candidate.workspace(), options)?;
        let (setup_key, setup_outcome) = support::run_setup_cached(
            project,
            options,
            &r2_runner,
            candidate.workspace(),
            run_dir,
            &r2_env,
        )?;
        if setup_outcome.reused {
            provider.record_event(
                snapshot,
                &RunEvent::new("setup_reused", now_ms())
                    .with("setup_key", json!(setup_key))
                    .with("saved_wall_ms", json!(setup_outcome.saved_wall_ms)),
            )?;
        }
        install_approved(candidate.workspace(), approved, options)?;
        let baseline_tree =
            crate::gate::snapshot_tree_excluding(candidate.workspace(), &excluded_paths).map_err(
                |error| environment_error("candidate_snapshot_failed", error.to_string()),
            )?;
        let model = options.rung2_model.as_str();
        let effort = options.rung2_effort.as_str();
        provider.record_event(
            snapshot,
            &RunEvent::new("rung_started", now_ms())
                .with("rung", json!("R2"))
                .with("model", json!(model))
                .with("effort", json!(effort))
                .with("entered_because", json!("escalation"))
                .with(
                    "wall_ms",
                    json!(stage_wall_ms(build_started, options.wall_ms)),
                ),
        )?;
        let r1_summary = format!(
            "R1 {}: {}; {}",
            options.builder_model,
            rung_reason,
            support::gate_feedback(approved, &report, run_dir)
                .lines()
                .take(4)
                .map(|line| line.chars().take(180).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        );
        let r2_first =
            first_message_with_history(approved, &acceptance_text, plan_text, &[r1_summary]);
        develop = provider.develop_on_rung(
            snapshot,
            run_dir,
            candidate.workspace(),
            "R2",
            model,
            effort,
            &r2_first,
            None,
            &baseline_tree,
            &excluded_paths,
            &r2_runner,
            r2_env.clone(),
            &protection,
            matches!(options.recipe.as_str(), "direct" | "direct-escalate"),
            None,
        )?;
        report = run_candidate_gate(
            project,
            approved,
            options,
            run_dir,
            base.workspace(),
            &candidate,
            &supervisor,
            &integrity,
            &r2_env,
            protection.clone(),
        )?;
        current_rung = "R2".to_owned();
        rung_reason = gate_end_reason(&report, &develop.reason);
        record_verification(&mut provider, snapshot, &report, "R2", &demoted)?;
        audit_acceptance(
            &mut provider,
            snapshot,
            &mut auditor,
            approved,
            base_sha,
            run_dir,
            candidate.workspace(),
            &mut report,
            "R2",
            &mut demoted,
        )?;
        if report.is_landable() {
            rung_reason = "green".to_owned();
        }
        repairs = 0;
        previous_red_count = None;
        while !report.is_landable()
            && repairs < 6
            && rung_reason != "budget"
            && rung_reason != "protected_restore_limit"
        {
            let count = red_count(&report, &demoted);
            if previous_red_count.is_some_and(|previous| count >= previous) {
                rung_reason = "no_progress".to_owned();
                break;
            }
            previous_red_count = Some(count);
            let before = candidate_working_diff(candidate.workspace(), base_sha)?;
            provider.record_event(
                snapshot,
                &RunEvent::new("repair", now_ms())
                    .with("rung", json!("R2"))
                    .with("number", json!(repairs + 1))
                    .with("count", json!(count)),
            )?;
            let repaired = provider.develop_on_rung(
                snapshot,
                run_dir,
                candidate.workspace(),
                "R2",
                model,
                effort,
                "",
                Some(&support::gate_feedback(approved, &report, run_dir)),
                &baseline_tree,
                &excluded_paths,
                &r2_runner,
                r2_env.clone(),
                &protection,
                matches!(options.recipe.as_str(), "direct" | "direct-escalate"),
                None,
            )?;
            repairs = repairs.saturating_add(1);
            develop.turns = repaired.turns;
            develop.changed |= repaired.changed;
            develop.tool_outputs.extend(repaired.tool_outputs);
            develop.model_stages = develop.model_stages.saturating_add(repaired.model_stages);
            report = run_candidate_gate(
                project,
                approved,
                options,
                run_dir,
                base.workspace(),
                &candidate,
                &supervisor,
                &integrity,
                &r2_env,
                protection.clone(),
            )?;
            record_verification(&mut provider, snapshot, &report, "R2", &demoted)?;
            audit_acceptance(
                &mut provider,
                snapshot,
                &mut auditor,
                approved,
                base_sha,
                run_dir,
                candidate.workspace(),
                &mut report,
                "R2",
                &mut demoted,
            )?;
            if repaired.reason == "turn_cap" {
                rung_reason = "turn_cap".to_owned();
                break;
            }
            if repaired.reason == "protected_restore_limit" {
                rung_reason = "protected_restore_limit".to_owned();
                break;
            }
            if repaired.reason == "budget" {
                rung_reason = "budget".to_owned();
                break;
            }
            if !repaired.progressed {
                rung_reason = "unchanged".to_owned();
                break;
            }
            let after = candidate_working_diff(candidate.workspace(), base_sha)?;
            if before == after {
                rung_reason = "unchanged".to_owned();
                break;
            }
            rung_reason = gate_end_reason(&report, &repaired.reason);
        }
        if repairs == 6 && !report.is_landable() {
            rung_reason = "repair_cap".to_owned();
        }
        advisory = advisory_failures(&report, &demoted);
        landable = report.is_landable() && (advisory.is_empty() || options.land_policy != "green");

        if !landable && last_rung >= 3 && rung_reason != "budget" {
            let r2_tree = report.verified_tree.clone().unwrap_or(
                crate::gate::snapshot_tree(candidate.workspace()).map_err(|error| {
                    environment_error("candidate_snapshot_failed", error.to_string())
                })?,
            );
            let r2_diff = candidate_diff(candidate.workspace(), base_sha, &r2_tree)?;
            write_private(&run_dir.join("candidate-R2.diff"), &r2_diff)?;
            let setup_outputs = options
                .setup_outputs
                .iter()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>();
            let r2_commit = candidate
                .candidate_commit_excluding(
                    base_sha,
                    &r2_tree,
                    &approved.intent.frontmatter.title,
                    &approved.slug,
                    &setup_outputs,
                )
                .map_err(|error| controller_error("candidate_commit_failed", error.to_string()))?;
            publish_candidate(
                candidate.workspace(),
                candidate.origin(),
                &r2_commit.commit,
                &run_id,
                "R2",
            )?;
            let r2_advisory = advisory_failures(&report, &demoted);
            let r2_verdict = if report.is_landable() && !r2_advisory.is_empty() {
                "green-with-advisory-tests"
            } else if report.is_landable() {
                "green"
            } else {
                "unverified"
            };
            candidates.push(CandidateSnapshot::from_report(
                "R2",
                2,
                r2_commit.commit,
                r2_tree.clone(),
                r2_verdict,
                &rung_reason,
                &report,
                &demoted,
                r2_diff.clone(),
            ));
            for event in [
                RunEvent::new("verification", now_ms())
                    .with("rung", json!("R2"))
                    .with("tree", json!(r2_tree.clone()))
                    .with("result", json!(r2_verdict))
                    .with("checks", checks_json(&report))
                    .with("acceptance", acceptance_json(&report, &demoted))
                    .with("count", json!(support::gate_failure_count(&report)))
                    .with("blocking_count", json!(red_count(&report, &demoted))),
                RunEvent::new("rung_finished", now_ms())
                    .with("rung", json!("R2"))
                    .with("reason", json!(rung_reason.clone()))
                    .with("verdict", json!(r2_verdict))
                    .with("tree", json!(r2_tree.clone()))
                    .with(
                        "candidate_ref",
                        json!(format!("refs/kogen/candidates/{run_id}/R2")),
                    )
                    .with("diff_lines", json!(changed_line_count(&r2_diff)))
                    .with("turns", json!(develop.turns))
                    .with("changed", json!(develop.changed))
                    .with("repairs", json!(repairs))
                    .with("tool_calls", json!(develop.tool_outputs.len()))
                    .with("model_stages", json!(develop.model_stages)),
            ] {
                provider.record_event(snapshot, &event)?;
            }

            let r3_path = project.state_root.join(format!("{run_id}-R3"));
            cleanup_path(candidate.workspace());
            candidate = LandingRepository::clone_fresh(&project.origin, &r3_path, base_sha)
                .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
            let r3_runner = support::sandboxed(
                project,
                options,
                candidate.workspace(),
                run_dir,
                &supervisor,
                &integrity,
            );
            let r3_env =
                support::child_environment(project, run_dir, candidate.workspace(), options)?;
            let (setup_key, setup_outcome) = support::run_setup_cached(
                project,
                options,
                &r3_runner,
                candidate.workspace(),
                run_dir,
                &r3_env,
            )?;
            if setup_outcome.reused {
                provider.record_event(
                    snapshot,
                    &RunEvent::new("setup_reused", now_ms())
                        .with("setup_key", json!(setup_key))
                        .with("saved_wall_ms", json!(setup_outcome.saved_wall_ms)),
                )?;
            }
            install_approved(candidate.workspace(), approved, options)?;
            let baseline_tree =
                crate::gate::snapshot_tree_excluding(candidate.workspace(), &excluded_paths)
                    .map_err(|error| {
                        environment_error("candidate_snapshot_failed", error.to_string())
                    })?;
            let model = options.rung3_model.as_str();
            let effort = options.rung3_effort.as_str();
            provider.record_event(
                snapshot,
                &RunEvent::new("rung_started", now_ms())
                    .with("rung", json!("R3"))
                    .with("model", json!(model))
                    .with("effort", json!(effort))
                    .with("entered_because", json!("escalation"))
                    .with(
                        "wall_ms",
                        json!(stage_wall_ms(build_started, options.wall_ms)),
                    ),
            )?;
            let r2_summary = format!(
                "R2 {model}/{effort}: {}; {}",
                rung_reason,
                support::gate_feedback(approved, &report, run_dir)
                    .lines()
                    .take(4)
                    .map(|line| line.chars().take(180).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let mut history = candidates
                .iter()
                .filter(|candidate| candidate.rung != "R2")
                .map(|item| format!("{}: {}; {}", item.rung, item.reason, item.verdict))
                .collect::<Vec<_>>();
            history.push(r2_summary);
            let r3_first =
                first_message_with_history(approved, &acceptance_text, plan_text, &history);
            develop = provider.develop_on_rung(
                snapshot,
                run_dir,
                candidate.workspace(),
                "R3",
                model,
                effort,
                &r3_first,
                None,
                &baseline_tree,
                &excluded_paths,
                &r3_runner,
                r3_env.clone(),
                &protection,
                matches!(options.recipe.as_str(), "direct" | "direct-escalate"),
                None,
            )?;
            report = run_candidate_gate(
                project,
                approved,
                options,
                run_dir,
                base.workspace(),
                &candidate,
                &supervisor,
                &integrity,
                &r3_env,
                protection.clone(),
            )?;
            current_rung = "R3".to_owned();
            rung_reason = gate_end_reason(&report, &develop.reason);
            record_verification(&mut provider, snapshot, &report, "R3", &demoted)?;
            audit_acceptance(
                &mut provider,
                snapshot,
                &mut auditor,
                approved,
                base_sha,
                run_dir,
                candidate.workspace(),
                &mut report,
                "R3",
                &mut demoted,
            )?;
            if report.is_landable() {
                rung_reason = "green".to_owned();
            }
            repairs = 0;
            previous_red_count = None;
            while !report.is_landable()
                && repairs < 6
                && rung_reason != "budget"
                && rung_reason != "protected_restore_limit"
            {
                let count = red_count(&report, &demoted);
                if previous_red_count.is_some_and(|previous| count >= previous) {
                    rung_reason = "no_progress".to_owned();
                    break;
                }
                previous_red_count = Some(count);
                let before = candidate_working_diff(candidate.workspace(), base_sha)?;
                provider.record_event(
                    snapshot,
                    &RunEvent::new("repair", now_ms())
                        .with("rung", json!("R3"))
                        .with("number", json!(repairs + 1))
                        .with("count", json!(count)),
                )?;
                let repaired = provider.develop_on_rung(
                    snapshot,
                    run_dir,
                    candidate.workspace(),
                    "R3",
                    model,
                    effort,
                    "",
                    Some(&support::gate_feedback(approved, &report, run_dir)),
                    &baseline_tree,
                    &excluded_paths,
                    &r3_runner,
                    r3_env.clone(),
                    &protection,
                    matches!(options.recipe.as_str(), "direct" | "direct-escalate"),
                    None,
                )?;
                repairs = repairs.saturating_add(1);
                develop.turns = repaired.turns;
                develop.changed |= repaired.changed;
                develop.tool_outputs.extend(repaired.tool_outputs);
                develop.model_stages = develop.model_stages.saturating_add(repaired.model_stages);
                report = run_candidate_gate(
                    project,
                    approved,
                    options,
                    run_dir,
                    base.workspace(),
                    &candidate,
                    &supervisor,
                    &integrity,
                    &r3_env,
                    protection.clone(),
                )?;
                record_verification(&mut provider, snapshot, &report, "R3", &demoted)?;
                audit_acceptance(
                    &mut provider,
                    snapshot,
                    &mut auditor,
                    approved,
                    base_sha,
                    run_dir,
                    candidate.workspace(),
                    &mut report,
                    "R3",
                    &mut demoted,
                )?;
                if repaired.reason == "turn_cap" {
                    rung_reason = "turn_cap".to_owned();
                    break;
                }
                if repaired.reason == "protected_restore_limit" {
                    rung_reason = "protected_restore_limit".to_owned();
                    break;
                }
                if repaired.reason == "budget" {
                    rung_reason = "budget".to_owned();
                    break;
                }
                if !repaired.progressed {
                    rung_reason = "unchanged".to_owned();
                    break;
                }
                let after = candidate_working_diff(candidate.workspace(), base_sha)?;
                if before == after {
                    rung_reason = "unchanged".to_owned();
                    break;
                }
                rung_reason = gate_end_reason(&report, &repaired.reason);
            }
            if repairs == 6 && !report.is_landable() {
                rung_reason = "repair_cap".to_owned();
            }
            advisory = advisory_failures(&report, &demoted);
            landable =
                report.is_landable() && (advisory.is_empty() || options.land_policy != "green");
        }
    }
    let tree = report.verified_tree.clone().unwrap_or(
        crate::gate::snapshot_tree(candidate.workspace())
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?,
    );
    record_scope_warnings(
        project,
        approved,
        options,
        candidate.workspace(),
        base_sha,
        store,
        snapshot,
    )?;
    advisory = advisory_failures(&report, &demoted);
    let verdict = if report.is_landable() && !advisory.is_empty() {
        "green-with-advisory-tests"
    } else if report.is_landable() {
        "green"
    } else {
        "unverified"
    };
    let diff = candidate_diff(candidate.workspace(), base_sha, &tree)?;
    write_private(
        &run_dir.join(format!("candidate-{current_rung}.diff")),
        &diff,
    )?;
    let rung_number = current_rung
        .strip_prefix('R')
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(1);
    candidates.push(CandidateSnapshot::from_report(
        &current_rung,
        rung_number,
        String::new(),
        tree.clone(),
        verdict,
        &rung_reason,
        &report,
        &demoted,
        diff.clone(),
    ));
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
        candidate.origin(),
        &candidate_commit.commit,
        &snapshot.run_id,
        &current_rung,
    )?;
    candidates
        .last_mut()
        .expect("final rung snapshot was appended")
        .commit
        .clone_from(&candidate_commit.commit);
    snapshot.fields.insert("verdict".to_owned(), json!(verdict));
    snapshot
        .fields
        .insert("rung".to_owned(), json!(current_rung));
    snapshot.fields.insert(
        "advisory_items".to_owned(),
        json!(advisory.iter().cloned().collect::<Vec<_>>()),
    );
    snapshot.fields.insert(
        "candidate_commit".to_owned(),
        json!(candidate_commit.commit),
    );
    record(
        store,
        snapshot,
        RunEvent::new("rung_finished", now_ms())
            .with("rung", json!(current_rung))
            .with("reason", json!(rung_reason.clone()))
            .with("verdict", json!(verdict))
            .with("tree", json!(tree.clone()))
            .with(
                "candidate_ref",
                json!(format!(
                    "refs/kogen/candidates/{}/{}",
                    snapshot.run_id, current_rung
                )),
            )
            .with("diff_lines", json!(changed_line_count(&diff)))
            .with("turns", json!(develop.turns))
            .with("changed", json!(develop.changed))
            .with("repairs", json!(repairs))
            .with("tool_calls", json!(develop.tool_outputs.len()))
            .with("model_stages", json!(develop.model_stages)),
    )?;
    if landable {
        record(
            store,
            snapshot,
            commit_result_event(&candidate_commit.commit, &tree),
        )?;
    }
    if !landable {
        let winner = best_candidate(&candidates)
            .expect("at least the final rung has a candidate")
            .clone();
        write_private(&run_dir.join("candidate.diff"), &winner.diff)?;
        let winner_workspace = project
            .state_root
            .join(format!("{}-winner", snapshot.run_id));
        if winner.commit != candidate_commit.commit {
            cleanup_path(candidate.workspace());
            candidate =
                LandingRepository::clone_fresh(&project.origin, &winner_workspace, &winner.commit)
                    .map_err(|error| {
                        environment_error("workspace_create_failed", error.to_string())
                    })?;
        }
        candidate
            .park(&snapshot.run_id, &winner.commit)
            .map_err(|error| controller_error("candidate_park_failed", error.to_string()))?;
        snapshot.status = "failed".to_owned();
        snapshot
            .fields
            .insert("verdict".to_owned(), json!(winner.verdict));
        snapshot
            .fields
            .insert("rung".to_owned(), json!(winner.rung));
        snapshot
            .fields
            .insert("candidate_commit".to_owned(), json!(winner.commit));
        record(
            store,
            snapshot,
            RunEvent::new("selection", now_ms())
                .with("winner_rung", json!(winner.rung))
                .with("rung", json!(winner.rung))
                .with("verdict", json!(winner.verdict))
                .with("ranking", json!(candidate_ranking(&candidates))),
        )?;
        record(
            store,
            snapshot,
            RunEvent::new("finished", now_ms())
                .with("status", json!("failed"))
                .with("rung", json!(winner.rung))
                .with("verdict", json!(winner.verdict))
                .with("advisory_items", json!(advisory))
                .with("reason", json!(winner.reason)),
        )?;
        cleanup_path(base.workspace());
        cleanup_path(candidate.workspace());
        return Ok(BuildOutcome {
            status: BuildStatus::Failed,
            run_id: snapshot.run_id.clone(),
            commit: winner.commit,
            verdict: winner.verdict,
            advisory_items: advisory.iter().cloned().collect(),
            reason: winner.reason,
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
        &mut provider,
        &baseline_tree,
        &excluded_paths,
    )?;
    cleanup_path(base.workspace());
    match result.outcome {
        crate::git::landing::LandingOutcome::Landed {
            commit, warnings, ..
        } => Ok(BuildOutcome {
            status: BuildStatus::Landed,
            run_id: snapshot.run_id.clone(),
            commit,
            verdict: verdict.to_owned(),
            advisory_items: advisory.iter().cloned().collect(),
            reason: String::new(),
            stderr: landing_stderr(&sandbox_warning, &warnings),
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
                advisory_items: advisory.iter().cloned().collect(),
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
                advisory_items: advisory.iter().cloned().collect(),
                reason,
                stderr: sandbox_warning,
                has_run: true,
            })
        }
    }
}

pub(super) fn run_witness_build(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
    run_dir: &Path,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
) -> Result<super::super::WitnessBuildResult, CoreError> {
    let witness_ref = format!("refs/kogen/witness/{}", approved.slug);
    let _ = crate::git::GitRepo::new(&project.origin).output(&["update-ref", "-d", &witness_ref]);
    let candidate_path = project
        .state_root
        .join(format!("{}-witness-R1", snapshot.run_id));
    let base_path = project
        .state_root
        .join(format!("{}-witness-base", snapshot.run_id));
    let candidate = LandingRepository::clone_fresh(&project.origin, &candidate_path, base_sha)
        .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
    let base = LandingRepository::clone_fresh(&project.origin, &base_path, base_sha)
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
    let mut provider = BuildProvider::new(project, approved, options, store)?;
    let account = provider.account().clone();
    record_started(
        &mut provider,
        snapshot,
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
            snapshot,
            &RunEvent::new("sandbox_unavailable", now_ms()).with("reason", json!(reason)),
        )?;
    }
    let files = tracked_paths(project, base_sha)?;
    let (plan, _) = provider.plan(snapshot, run_dir, &files)?;
    let (_, plan_text) = plan.split_once('\0').unwrap_or(("easy", plan.as_str()));
    provider.record_event(
        snapshot,
        &RunEvent::new("rung_started", now_ms())
            .with("rung", json!("R1"))
            .with("model", json!(options.builder_model))
            .with("effort", json!(options.builder_effort))
            .with("entered_because", json!("witness"))
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
        let (key, result) =
            support::run_setup_cached(project, options, runner, workspace, run_dir, environment)?;
        if result.reused {
            provider.record_event(
                snapshot,
                &RunEvent::new("setup_reused", now_ms())
                    .with("setup_key", json!(key))
                    .with("saved_wall_ms", json!(result.saved_wall_ms)),
            )?;
        }
    }
    install_approved(candidate.workspace(), approved, options)?;
    let excluded_paths = progress_exclusions(approved, options);
    let baseline_tree =
        crate::gate::snapshot_tree_excluding(candidate.workspace(), &excluded_paths)
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?;
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
        cleanup_path(base.workspace());
        cleanup_path(candidate.workspace());
        return Err(environment_error(
            "tool_missing",
            "acceptance runner is unavailable on the build base",
        ));
    }
    let acceptance_text = base_acceptance_text(approved, &base_acceptance);
    provider.record_event(snapshot,
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
    let _develop = provider.develop(
        snapshot,
        run_dir,
        candidate.workspace(),
        &baseline_tree,
        &excluded_paths,
        plan_text,
        &acceptance_text,
        &builder_runner,
        child_env.clone(),
        &protection,
        false,
    )?;
    let report = run_candidate_gate(
        project,
        approved,
        options,
        run_dir,
        base.workspace(),
        &candidate,
        &supervisor,
        &integrity,
        &child_env,
        protection,
    )?;
    let tree = report.verified_tree.clone().unwrap_or(
        crate::gate::snapshot_tree(candidate.workspace())
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?,
    );
    let green = report.is_landable();
    provider.record_event(
        snapshot,
        &RunEvent::new("verification", now_ms())
            .with("rung", json!("R1"))
            .with("tree", json!(tree.clone()))
            .with("result", json!(if green { "green" } else { "red" }))
            .with("checks", checks_json(&report))
            .with("acceptance", acceptance_json(&report, &BTreeSet::new()))
            .with("count", json!(usize::from(!green))),
    )?;
    if !green {
        let failed_ids = report
            .acceptance
            .item_pass
            .iter()
            .filter_map(|(id, passed)| (!passed).then_some(id.clone()))
            .collect::<Vec<_>>();
        let mut warnings = Vec::new();
        if !failed_ids.is_empty() {
            let diff = candidate_diff(candidate.workspace(), base_sha, &tree)?;
            let request = crate::run::orchestration::BuildAuditRequest {
                ids: failed_ids.clone(),
                request: approved.intent.request.clone().unwrap_or_default(),
                test_source: String::from_utf8_lossy(&approved.acceptance_bytes).into_owned(),
                failure_output: String::from_utf8_lossy(&report.acceptance.process.output_tail)
                    .into_owned(),
                candidate_diff: String::from_utf8_lossy(&diff).into_owned(),
            };
            let reply = provider.witness_audit(snapshot, run_dir, "R1", &request)?;
            let judgments = decode_witness_audit(&reply, &failed_ids);
            provider.record_event(
                snapshot,
                &RunEvent::new("audit", now_ms())
                    .with("rung", json!("R1"))
                    .with(
                        "items",
                        json!(
                            judgments
                                .iter()
                                .map(|item| json!({
                                    "id": item.id,
                                    "verdict": item.verdict,
                                    "citation": item.citation,
                                    "reason": item.reason,
                                }))
                                .collect::<Vec<_>>()
                        ),
                    ),
            )?;
            warnings.extend(
                judgments
                    .iter()
                    .filter(|item| item.verdict == "UNDECIDED")
                    .map(|item| crate::intent::shaping::ShapeWarning {
                        code: "feasibility_concern".to_owned(),
                        item_ids: vec![item.id.clone()],
                        message: item.reason.clone(),
                    }),
            );
        }
        drop(provider);
        cleanup_path(base.workspace());
        cleanup_path(candidate.workspace());
        return Ok(super::super::WitnessBuildResult {
            proven: false,
            warnings,
        });
    }
    let setup_outputs = options
        .setup_outputs
        .iter()
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    let witness_commit = candidate
        .candidate_commit_excluding(
            base_sha,
            &tree,
            &approved.intent.frontmatter.title,
            &approved.slug,
            &setup_outputs,
        )
        .map_err(|error| controller_error("candidate_commit_failed", error.to_string()))?;
    let source = format!("{}:{witness_ref}", witness_commit.commit);
    let lease = format!("--force-with-lease={witness_ref}:");
    crate::git::GitRepo::new(candidate.workspace())
        .output(&[
            "push",
            "--porcelain",
            "--no-recurse-submodules",
            &lease,
            "origin",
            &source,
        ])
        .map_err(|error| environment_error("witness_publish_failed", error.to_string()))?;
    provider.record_event(
        snapshot,
        &commit_result_event(&witness_commit.commit, &tree),
    )?;
    drop(provider);
    cleanup_path(base.workspace());
    cleanup_path(candidate.workspace());
    Ok(super::super::WitnessBuildResult {
        proven: true,
        warnings: Vec::new(),
    })
}

struct WitnessAuditJudgment {
    id: String,
    verdict: String,
    citation: String,
    reason: String,
}

fn decode_witness_audit(reply: &str, failed_ids: &[String]) -> Vec<WitnessAuditJudgment> {
    let parsed = serde_json::from_str::<Value>(reply).ok();
    let mut rows = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    if let Some(items) = parsed
        .as_ref()
        .and_then(|value| value.get("items"))
        .and_then(Value::as_array)
    {
        for item in items {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !failed_ids.iter().any(|expected| expected == id) {
                continue;
            }
            let verdict = item.get("verdict").and_then(Value::as_str);
            let citation = item.get("citation").and_then(Value::as_str);
            let reason = item.get("reason").and_then(Value::as_str);
            let Some(verdict @ ("TEST-WRONG" | "WITNESS-WRONG" | "UNDECIDED")) = verdict else {
                continue;
            };
            let (Some(citation), Some(reason)) = (citation, reason) else {
                continue;
            };
            if rows
                .insert(
                    id.to_owned(),
                    (verdict.to_owned(), citation.to_owned(), reason.to_owned()),
                )
                .is_some()
            {
                rows.remove(id);
                duplicates.insert(id.to_owned());
            }
        }
    }
    failed_ids
        .iter()
        .map(|id| {
            let judgment = (!duplicates.contains(id)).then(|| rows.get(id)).flatten();
            let (verdict, citation, reason) = judgment.map_or_else(
                || {
                    (
                        "UNDECIDED".to_owned(),
                        String::new(),
                        "The auditor did not provide a usable adjudication.".to_owned(),
                    )
                },
                |(verdict, citation, reason)| (verdict.clone(), citation.clone(), reason.clone()),
            );
            WitnessAuditJudgment {
                id: id.clone(),
                verdict,
                citation,
                reason,
            }
        })
        .collect()
}

#[cfg(test)]
mod witness_audit_tests {
    use super::decode_witness_audit;

    #[test]
    fn red_witness_preserves_an_undecided_judgment() {
        let failed = vec!["A1".to_owned()];
        let judgments = decode_witness_audit(
            r#"{"items":[{"id":"A1","verdict":"UNDECIDED","citation":"","reason":"Insufficient evidence."}]}"#,
            &failed,
        );
        assert_eq!(judgments.len(), 1);
        assert_eq!(judgments[0].verdict, "UNDECIDED");
        assert_eq!(judgments[0].reason, "Insufficient evidence.");
    }
}

struct WitnessCandidate {
    candidate: LandingRepository,
    tree: String,
    commit: String,
}

#[allow(clippy::too_many_arguments)]
fn try_witness(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
    run_dir: &Path,
    integrity: &support::IntegritySnapshot,
    supervisor: &crate::run::ProcessSupervisor,
    provider: &mut BuildProvider<'_>,
    snapshot: &mut RunSnapshot,
    run_id: &str,
) -> Result<Option<WitnessCandidate>, CoreError> {
    let Some(witness) = approved
        .approval
        .get("witness")
        .filter(|value| value.is_object())
    else {
        return Ok(None);
    };
    let Some(witness_commit) = witness.get("commit").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(witness_base) = witness.get("base_sha").and_then(Value::as_str) else {
        return Ok(None);
    };
    let origin = crate::git::GitRepo::new(&project.origin);
    if origin
        .output(&["merge-base", "--is-ancestor", witness_base, base_sha])
        .is_err()
    {
        return Ok(None);
    }

    let base_path = project.state_root.join(format!("{run_id}-witness-base"));
    let candidate_path = project.state_root.join(format!("{run_id}-witness"));
    let base = LandingRepository::clone_fresh(&project.origin, &base_path, base_sha)
        .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
    let candidate = LandingRepository::clone_fresh(&project.origin, &candidate_path, base_sha)
        .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
    let base_runner = support::sandboxed(
        project,
        options,
        base.workspace(),
        run_dir,
        supervisor,
        integrity,
    );
    let candidate_runner = support::sandboxed(
        project,
        options,
        candidate.workspace(),
        run_dir,
        supervisor,
        integrity,
    );
    let base_env = support::child_environment(project, run_dir, base.workspace(), options)?;
    let candidate_env =
        support::child_environment(project, run_dir, candidate.workspace(), options)?;
    for (workspace, runner, environment) in [
        (
            base.workspace(),
            &base_runner as &dyn crate::run::ProcessPort,
            &base_env,
        ),
        (
            candidate.workspace(),
            &candidate_runner as &dyn crate::run::ProcessPort,
            &candidate_env,
        ),
    ] {
        let (key, result) =
            support::run_setup_cached(project, options, runner, workspace, run_dir, environment)?;
        if result.reused {
            provider.record_event(
                snapshot,
                &RunEvent::new("setup_reused", now_ms())
                    .with("setup_key", json!(key))
                    .with("saved_wall_ms", json!(result.saved_wall_ms)),
            )?;
        }
    }

    let intent_path = format!(".kogen/intents/{}/intent.md", approved.slug);
    let candidate_test = support::candidate_path(options, &approved.slug)
        .to_string_lossy()
        .into_owned();
    let source_test = approved.acceptance_path.clone();
    let excluded_intent = format!(":(exclude){intent_path}");
    let excluded_source = format!(":(exclude){source_test}");
    let excluded_candidate = format!(":(exclude){candidate_test}");
    let diff_args = [
        "diff",
        "--binary",
        witness_base,
        witness_commit,
        "--",
        ".",
        excluded_intent.as_str(),
        excluded_source.as_str(),
        excluded_candidate.as_str(),
    ];
    let patch_bytes = origin
        .output(&diff_args)
        .map_err(|error| environment_error("witness_read_failed", error.to_string()))?;
    if !patch_bytes.is_empty()
        && crate::git::GitRepo::new(candidate.workspace())
            .output_with_env(&["apply", "--3way", "--index"], &[], Some(&patch_bytes))
            .is_err()
    {
        let _ =
            crate::git::GitRepo::new(candidate.workspace()).output(&["reset", "--hard", base_sha]);
        cleanup_path(base.workspace());
        cleanup_path(candidate.workspace());
        provider.record_event(
            snapshot,
            &RunEvent::new("verification", now_ms())
                .with("rung", json!("witness"))
                .with("result", json!("red"))
                .with("count", json!(1)),
        )?;
        return Ok(None);
    }

    install_approved(candidate.workspace(), approved, options)?;
    let protection = support::protected_workspace(project, approved, options, base_sha)?;
    let report = run_candidate_gate(
        project,
        approved,
        options,
        run_dir,
        base.workspace(),
        &candidate,
        supervisor,
        integrity,
        &candidate_env,
        protection,
    )?;
    let Some(tree) = report.verified_tree.clone() else {
        cleanup_path(base.workspace());
        cleanup_path(candidate.workspace());
        provider.record_event(
            snapshot,
            &RunEvent::new("verification", now_ms())
                .with("rung", json!("witness"))
                .with("result", json!("red"))
                .with("checks", checks_json(&report))
                .with("acceptance", acceptance_json(&report, &BTreeSet::new()))
                .with("count", json!(1)),
        )?;
        return Ok(None);
    };
    if !report.is_landable() {
        cleanup_path(base.workspace());
        cleanup_path(candidate.workspace());
        provider.record_event(
            snapshot,
            &RunEvent::new("verification", now_ms())
                .with("rung", json!("witness"))
                .with("tree", json!(tree))
                .with("result", json!("red"))
                .with("checks", checks_json(&report))
                .with("acceptance", acceptance_json(&report, &BTreeSet::new()))
                .with("count", json!(1)),
        )?;
        return Ok(None);
    }

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
        candidate.origin(),
        &candidate_commit.commit,
        run_id,
        "witness",
    )?;
    let diff = candidate_diff(candidate.workspace(), base_sha, &tree)?;
    write_private(&run_dir.join("candidate.diff"), &diff)?;
    snapshot.fields.insert("verdict".to_owned(), json!("green"));
    snapshot.fields.insert("rung".to_owned(), json!("witness"));
    snapshot.fields.insert(
        "candidate_commit".to_owned(),
        json!(candidate_commit.commit),
    );
    provider.record_event(
        snapshot,
        &RunEvent::new("verification", now_ms())
            .with("rung", json!("witness"))
            .with("tree", json!(tree.clone()))
            .with("result", json!("green"))
            .with("checks", checks_json(&report))
            .with("acceptance", acceptance_json(&report, &BTreeSet::new()))
            .with("count", json!(0)),
    )?;
    provider.record_event(
        snapshot,
        &commit_result_event(&candidate_commit.commit, &tree),
    )?;
    cleanup_path(base.workspace());
    Ok(Some(WitnessCandidate {
        candidate,
        tree,
        commit: candidate_commit.commit,
    }))
}

#[cfg(test)]
mod ladder_tests {
    use super::{
        CandidateSnapshot, best_candidate, candidate_ranking, changed_line_count, landing_stderr,
    };

    #[test]
    fn landing_warnings_end_with_newlines() {
        let warning = "land: warning: checkout is dirty".to_owned();
        assert_eq!(
            landing_stderr("", std::slice::from_ref(&warning)),
            format!("{warning}\n")
        );
        assert_eq!(
            landing_stderr(
                "kogen: warning: sandbox unavailable\n",
                std::slice::from_ref(&warning)
            ),
            format!("kogen: warning: sandbox unavailable\n{warning}\n")
        );
    }

    fn candidate(
        rung: &str,
        rung_number: u8,
        pass_count: usize,
        blocking_count: usize,
        diff_lines: usize,
    ) -> CandidateSnapshot {
        CandidateSnapshot {
            rung: rung.to_owned(),
            rung_number,
            commit: format!("commit-{rung}"),
            tree: format!("tree-{rung}"),
            verdict: "unverified".to_owned(),
            reason: "verification_red".to_owned(),
            pass_count,
            blocking_count,
            diff_lines,
            diff: vec![b'\n'; diff_lines],
        }
    }

    #[test]
    fn selector_ranks_passing_items_then_findings_diff_and_rung_order() {
        let cases = [
            (
                vec![candidate("R1", 1, 1, 0, 1), candidate("R2", 2, 2, 9, 99)],
                "R2",
            ),
            (
                vec![candidate("R1", 1, 1, 2, 1), candidate("R2", 2, 1, 1, 99)],
                "R2",
            ),
            (
                vec![candidate("R1", 1, 1, 1, 7), candidate("R2", 2, 1, 1, 3)],
                "R2",
            ),
            (
                vec![candidate("R1", 1, 1, 1, 3), candidate("R2", 2, 1, 1, 3)],
                "R1",
            ),
        ];
        for (candidates, expected) in cases {
            assert_eq!(
                best_candidate(&candidates).map(|row| row.rung.as_str()),
                Some(expected)
            );
            assert_eq!(candidate_ranking(&candidates)[0]["rung"], expected);
        }
    }

    #[test]
    fn diff_line_count_excludes_git_headers() {
        let diff = b"diff --git a/lib/a b/lib/a\n--- a/lib/a\n+++ b/lib/a\n-old\n+new\n";
        assert_eq!(changed_line_count(diff), 2);
    }
}

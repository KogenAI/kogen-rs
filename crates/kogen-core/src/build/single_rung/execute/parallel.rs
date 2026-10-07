use super::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Instant;

fn rung_gate_dir(run_dir: &Path, rung: &str) -> PathBuf {
    run_dir.join(format!("gate-{rung}"))
}

struct RungResult {
    candidate: LandingRepository,
    metadata: CandidateSnapshot,
    report: crate::gate::GateReport,
    demoted: BTreeSet<String>,
    landable: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
    run_dir: &Path,
    candidate_r1: LandingRepository,
    base: LandingRepository,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    mut provider: BuildProvider<'_>,
    plan_text: &str,
    build_started: Instant,
    integrity: &support::IntegritySnapshot,
    supervisor: &crate::run::ProcessSupervisor,
    sandbox_warning: &str,
) -> Result<BuildOutcome, CoreError> {
    let run_id = snapshot.run_id.clone();
    let result = run_inner(
        project,
        approved,
        options,
        base_sha,
        run_dir,
        candidate_r1,
        base,
        store,
        snapshot,
        &mut provider,
        plan_text,
        build_started,
        integrity,
        supervisor,
        sandbox_warning,
    );
    if result.is_err() {
        for rung in 2..=4 {
            cleanup_path(&project.state_root.join(format!("{run_id}-R{rung}")));
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn run_inner(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
    run_dir: &Path,
    candidate_r1: LandingRepository,
    base: LandingRepository,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    provider: &mut BuildProvider<'_>,
    plan_text: &str,
    build_started: Instant,
    integrity: &support::IntegritySnapshot,
    supervisor: &crate::run::ProcessSupervisor,
    sandbox_warning: &str,
) -> Result<BuildOutcome, CoreError> {
    let run_id = snapshot.run_id.clone();
    let candidate_r2_path = project.state_root.join(format!("{run_id}-R2"));
    let candidate_r2 =
        LandingRepository::clone_fresh(&project.origin, &candidate_r2_path, base_sha)
            .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
    let excluded_paths = progress_exclusions(approved, options);
    let r1_environment =
        support::child_environment(project, run_dir, candidate_r1.workspace(), options)?;
    let r2_environment =
        support::child_environment(project, run_dir, candidate_r2.workspace(), options)?;
    let base_environment = support::child_environment(project, run_dir, base.workspace(), options)?;
    let r1_runner = support::sandboxed(
        project,
        options,
        candidate_r1.workspace(),
        run_dir,
        supervisor,
        integrity,
    );
    let r2_runner = support::sandboxed(
        project,
        options,
        candidate_r2.workspace(),
        run_dir,
        supervisor,
        integrity,
    );
    let base_runner = support::sandboxed(
        project,
        options,
        base.workspace(),
        run_dir,
        supervisor,
        integrity,
    );
    prepare_workspace(
        provider,
        snapshot,
        project,
        options,
        run_dir,
        candidate_r1.workspace(),
        &r1_runner,
        &r1_environment,
    )?;
    prepare_workspace(
        provider,
        snapshot,
        project,
        options,
        run_dir,
        candidate_r2.workspace(),
        &r2_runner,
        &r2_environment,
    )?;
    let (base_setup_key, base_setup) = support::run_setup_cached(
        project,
        options,
        &base_runner,
        base.workspace(),
        run_dir,
        &base_environment,
    )?;
    if base_setup.reused {
        provider.record_event(
            snapshot,
            &RunEvent::new("setup_reused", now_ms())
                .with("setup_key", json!(base_setup_key))
                .with("saved_wall_ms", json!(base_setup.saved_wall_ms)),
        )?;
    }

    install_approved(candidate_r1.workspace(), approved, options)?;
    install_approved(candidate_r2.workspace(), approved, options)?;
    let r1_baseline =
        crate::gate::snapshot_tree_excluding(candidate_r1.workspace(), &excluded_paths)
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?;
    let r2_baseline =
        crate::gate::snapshot_tree_excluding(candidate_r2.workspace(), &excluded_paths)
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?;
    let base_acceptance = support::base_acceptance(
        &base_runner,
        options,
        approved,
        base.workspace(),
        run_dir,
        &base_environment,
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
        snapshot,
        &RunEvent::new("base_acceptance", now_ms()).with(
            "items",
            json!(
                approved
                    .intent
                    .verify
                    .iter()
                    .map(|item| json!({
                        "id": item.id,
                        "kind": item.kind().unwrap_or("test"),
                        "base_status": if base_acceptance.item_pass.get(&item.id) == Some(&true) {
                            "passed"
                        } else {
                            "failed"
                        }
                    }))
                    .collect::<Vec<_>>()
            ),
        ),
    )?;
    let r1_protection = support::protected_workspace(project, approved, options, base_sha)?;
    let r2_protection = support::protected_workspace(project, approved, options, base_sha)?;
    provider.record_event(
        snapshot,
        &RunEvent::new("parallel_started", now_ms()).with("attempts", json!(2)),
    )?;
    for (rung, model, effort) in [
        (
            "R1",
            options.builder_model.as_str(),
            options.builder_effort.as_str(),
        ),
        (
            "R2",
            options.rung2_model.as_str(),
            options.rung2_effort.as_str(),
        ),
    ] {
        provider.record_event(
            snapshot,
            &RunEvent::new("rung_started", now_ms())
                .with("rung", json!(rung))
                .with("model", json!(model))
                .with("effort", json!(effort))
                .with("entered_because", json!("hard_parallel"))
                .with(
                    "wall_ms",
                    json!(stage_wall_ms(build_started, options.wall_ms)),
                ),
        )?;
    }
    let mut provider_r2 = provider.fork()?;
    let mut snapshot_r1 = snapshot.clone();
    let mut snapshot_r2 = snapshot.clone();
    let first_message = super::super::super::provider_prompt::builder_message(
        &approved.intent_bytes,
        &acceptance_text,
        plan_text,
    );
    let r1_run_id = run_id.clone();
    let r2_run_id = run_id.clone();
    let r1_base_path = base.workspace().to_path_buf();
    let r2_base_path = base.workspace().to_path_buf();
    let r1_excluded_paths = excluded_paths.as_slice();
    let r2_excluded_paths = excluded_paths.as_slice();
    let r1_first_message = first_message.clone();
    let r2_first_message = first_message;
    let (r1_result, r2_result) = thread::scope(|scope| {
        let provider_r1 = &mut *provider;
        let provider_r2 = &mut provider_r2;
        let r1_handle = scope.spawn(move || {
            execute_rung(
                project,
                approved,
                options,
                base_sha,
                run_dir,
                store,
                &mut snapshot_r1,
                provider_r1,
                candidate_r1,
                &r1_base_path,
                r1_environment,
                &r1_runner,
                &r1_protection,
                &r1_baseline,
                r1_excluded_paths,
                &r1_run_id,
                "R1",
                &options.builder_model,
                &options.builder_effort,
                r1_first_message,
                integrity,
                supervisor,
            )
        });
        let r2_handle = scope.spawn(move || {
            execute_rung(
                project,
                approved,
                options,
                base_sha,
                run_dir,
                store,
                &mut snapshot_r2,
                provider_r2,
                candidate_r2,
                &r2_base_path,
                r2_environment,
                &r2_runner,
                &r2_protection,
                &r2_baseline,
                r2_excluded_paths,
                &r2_run_id,
                "R2",
                &options.rung2_model,
                &options.rung2_effort,
                r2_first_message,
                integrity,
                supervisor,
            )
        });
        let r1 = r1_handle
            .join()
            .map_err(|_| controller_error("parallel_rung_failed", "R1 worker panicked"))?;
        let r2 = r2_handle
            .join()
            .map_err(|_| controller_error("parallel_rung_failed", "R2 worker panicked"))?;
        Ok::<_, CoreError>((r1, r2))
    })?;
    let mut rungs = vec![r1_result?, r2_result?];
    let last_rung = if options.experimental_r4 {
        options.max_rungs
    } else {
        options.max_rungs.min(3)
    };
    while rungs.iter().all(|rung| !rung.landable) && rungs.len() < usize::from(last_rung) {
        let next_number = rungs.len() as u8 + 1;
        let rung = format!("R{next_number}");
        let workspace_path = project
            .state_root
            .join(format!("{}-{rung}", snapshot.run_id));
        let candidate = LandingRepository::clone_fresh(&project.origin, &workspace_path, base_sha)
            .map_err(|error| environment_error("workspace_create_failed", error.to_string()))?;
        let environment =
            support::child_environment(project, run_dir, candidate.workspace(), options)?;
        let runner = support::sandboxed(
            project,
            options,
            candidate.workspace(),
            run_dir,
            supervisor,
            integrity,
        );
        prepare_workspace(
            provider,
            snapshot,
            project,
            options,
            run_dir,
            candidate.workspace(),
            &runner,
            &environment,
        )?;
        install_approved(candidate.workspace(), approved, options)?;
        let baseline = crate::gate::snapshot_tree_excluding(candidate.workspace(), &excluded_paths)
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?;
        let (model, effort) = (options.rung3_model.as_str(), options.rung3_effort.as_str());
        provider.record_event(
            snapshot,
            &RunEvent::new("rung_started", now_ms())
                .with("rung", json!(rung))
                .with("model", json!(model))
                .with("effort", json!(effort))
                .with("entered_because", json!("escalation"))
                .with(
                    "wall_ms",
                    json!(stage_wall_ms(build_started, options.wall_ms)),
                ),
        )?;
        let history = rungs
            .iter()
            .map(|prior| {
                format!(
                    "{}: {}; {}",
                    prior.metadata.rung, prior.metadata.reason, prior.metadata.verdict
                )
            })
            .collect::<Vec<_>>();
        let first_message =
            first_message_with_history(approved, &acceptance_text, plan_text, &history);
        let protection = support::protected_workspace(project, approved, options, base_sha)?;
        let mut rung_provider = provider.fork()?;
        let mut rung_snapshot = snapshot.clone();
        let result = execute_rung(
            project,
            approved,
            options,
            base_sha,
            run_dir,
            store,
            &mut rung_snapshot,
            &mut rung_provider,
            candidate,
            base.workspace(),
            environment,
            &runner,
            &protection,
            &baseline,
            &excluded_paths,
            &snapshot.run_id,
            &rung,
            model,
            effort,
            first_message,
            integrity,
            supervisor,
        )?;
        rungs.push(result);
    }

    let all_candidates = rungs
        .iter()
        .map(|rung| rung.metadata.clone())
        .collect::<Vec<_>>();
    let eligible = rungs
        .iter()
        .filter(|rung| rung.landable)
        .map(|rung| rung.metadata.clone())
        .collect::<Vec<_>>();
    let selected = if eligible.is_empty() {
        best_candidate(&all_candidates)
    } else {
        best_candidate(&eligible)
    }
    .expect("parallel ladder has at least two candidate rungs");
    let winner_index = rungs
        .iter()
        .position(|rung| rung.metadata.rung == selected.rung)
        .expect("selected candidate has a rung result");
    let winner = rungs.remove(winner_index);
    for loser in rungs {
        cleanup_path(loser.candidate.workspace());
    }
    write_private(&run_dir.join("candidate.diff"), &winner.metadata.diff)?;
    let advisory = advisory_failures(&winner.report, &winner.demoted)
        .into_iter()
        .collect::<Vec<_>>();
    snapshot
        .fields
        .insert("verdict".to_owned(), json!(winner.metadata.verdict));
    snapshot
        .fields
        .insert("rung".to_owned(), json!(winner.metadata.rung));
    snapshot
        .fields
        .insert("advisory_items".to_owned(), json!(advisory));
    snapshot
        .fields
        .insert("candidate_commit".to_owned(), json!(winner.metadata.commit));
    if !winner.landable {
        winner
            .candidate
            .park(&run_id, &winner.metadata.commit)
            .map_err(|error| controller_error("candidate_park_failed", error.to_string()))?;
        snapshot.status = "failed".to_owned();
        provider.record_event(
            snapshot,
            &RunEvent::new("selection", now_ms())
                .with("winner_rung", json!(winner.metadata.rung))
                .with("rung", json!(winner.metadata.rung))
                .with("verdict", json!(winner.metadata.verdict))
                .with("ranking", json!(candidate_ranking(&all_candidates))),
        )?;
        provider.record_event(
            snapshot,
            &RunEvent::new("finished", now_ms())
                .with("status", json!("failed"))
                .with("rung", json!(winner.metadata.rung))
                .with("verdict", json!(winner.metadata.verdict))
                .with("advisory_items", json!(advisory))
                .with("reason", json!(winner.metadata.reason)),
        )?;
        cleanup_path(base.workspace());
        cleanup_path(winner.candidate.workspace());
        return Ok(BuildOutcome {
            status: BuildStatus::Failed,
            run_id,
            commit: winner.metadata.commit,
            verdict: winner.metadata.verdict,
            advisory_items: advisory,
            reason: winner.metadata.reason,
            stderr: sandbox_warning.to_owned(),
            has_run: true,
        });
    }

    provider.record_event(
        snapshot,
        &commit_result_event(&winner.metadata.commit, &winner.metadata.tree),
    )?;
    let landing = super::super::landing::land_candidate(
        project,
        approved,
        options,
        store,
        snapshot,
        &winner.candidate,
        base_sha,
        &winner.metadata.tree,
        integrity,
        supervisor,
        provider,
        &winner.metadata.tree,
        &excluded_paths,
    );
    cleanup_path(base.workspace());
    let result = match landing {
        Ok(result) => result,
        Err(error) => {
            cleanup_path(winner.candidate.workspace());
            return Err(error);
        }
    };
    let verdict = winner.metadata.verdict.clone();
    match result.outcome {
        crate::git::landing::LandingOutcome::Landed {
            commit, warnings, ..
        } => Ok(BuildOutcome {
            status: BuildStatus::Landed,
            run_id,
            commit,
            verdict,
            advisory_items: advisory,
            reason: String::new(),
            stderr: landing_stderr(sandbox_warning, &warnings),
            has_run: true,
        }),
        crate::git::landing::LandingOutcome::Parked { commit, reason, .. } => {
            snapshot.status = "parked".to_owned();
            cleanup_path(winner.candidate.workspace());
            Ok(BuildOutcome {
                status: BuildStatus::Parked,
                run_id,
                commit,
                verdict,
                advisory_items: advisory,
                reason,
                stderr: sandbox_warning.to_owned(),
                has_run: true,
            })
        }
        crate::git::landing::LandingOutcome::Stopped { reason, .. } => {
            snapshot.status = "stopped".to_owned();
            cleanup_path(winner.candidate.workspace());
            Ok(BuildOutcome {
                status: BuildStatus::Stopped {
                    class: "controller".to_owned(),
                    reason: reason.clone(),
                },
                run_id,
                commit: winner.metadata.commit,
                verdict,
                advisory_items: advisory,
                reason,
                stderr: sandbox_warning.to_owned(),
                has_run: true,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_workspace(
    provider: &mut BuildProvider<'_>,
    snapshot: &mut RunSnapshot,
    project: &ProjectResolution,
    options: &BuildOptions,
    run_dir: &Path,
    workspace: &Path,
    runner: &dyn crate::run::ProcessPort,
    environment: &crate::run::ChildEnvironment,
) -> Result<(), CoreError> {
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
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_rung(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
    run_dir: &Path,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    provider: &mut BuildProvider<'_>,
    candidate: LandingRepository,
    base_workspace: &Path,
    environment: crate::run::ChildEnvironment,
    runner: &dyn crate::run::ProcessPort,
    protection: &crate::gate::ProtectedWorkspace,
    baseline_tree: &str,
    excluded_paths: &[PathBuf],
    run_id: &str,
    rung: &str,
    model: &str,
    effort: &str,
    first_message: String,
    integrity: &support::IntegritySnapshot,
    supervisor: &crate::run::ProcessSupervisor,
) -> Result<RungResult, CoreError> {
    let mut auditor = BuildAuditor::default();
    let mut demoted = BTreeSet::new();
    let mut develop = provider.develop_on_rung(
        snapshot,
        run_dir,
        candidate.workspace(),
        rung,
        model,
        effort,
        &first_message,
        None,
        baseline_tree,
        excluded_paths,
        runner,
        environment.clone(),
        protection,
        false,
        None,
    )?;
    let mut report = run_candidate_gate(
        project,
        approved,
        options,
        &rung_gate_dir(run_dir, rung),
        base_workspace,
        &candidate,
        supervisor,
        integrity,
        &environment,
        protection.clone(),
    )?;
    record_verification(provider, snapshot, &report, rung, &demoted)?;
    audit_acceptance(
        provider,
        snapshot,
        &mut auditor,
        approved,
        base_sha,
        run_dir,
        candidate.workspace(),
        &mut report,
        rung,
        &mut demoted,
    )?;
    let mut repairs = 0_u8;
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
                .with("rung", json!(rung))
                .with("number", json!(repairs + 1))
                .with("count", json!(count)),
        )?;
        let repaired = provider.develop_on_rung(
            snapshot,
            run_dir,
            candidate.workspace(),
            rung,
            model,
            effort,
            "",
            Some(&feedback),
            baseline_tree,
            excluded_paths,
            runner,
            environment.clone(),
            protection,
            false,
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
            &rung_gate_dir(run_dir, rung),
            base_workspace,
            &candidate,
            supervisor,
            integrity,
            &environment,
            protection.clone(),
        )?;
        record_verification(provider, snapshot, &report, rung, &demoted)?;
        audit_acceptance(
            provider,
            snapshot,
            &mut auditor,
            approved,
            base_sha,
            run_dir,
            candidate.workspace(),
            &mut report,
            rung,
            &mut demoted,
        )?;
        if repaired.reason == "turn_cap"
            || repaired.reason == "protected_restore_limit"
            || repaired.reason == "budget"
        {
            rung_reason = repaired.reason;
            break;
        }
        if !repaired.progressed
            || before == candidate_working_diff(candidate.workspace(), base_sha)?
        {
            rung_reason = "unchanged".to_owned();
            break;
        }
        rung_reason = gate_end_reason(&report, &repaired.reason);
    }
    if repairs == 6 && !report.is_landable() {
        rung_reason = "repair_cap".to_owned();
    }

    record_scope_warnings(
        project,
        approved,
        options,
        candidate.workspace(),
        base_sha,
        store,
        snapshot,
    )?;
    let advisory = advisory_failures(&report, &demoted);
    let landable = report.is_landable() && (advisory.is_empty() || options.land_policy != "green");
    let verdict = if report.is_landable() && !advisory.is_empty() {
        "green-with-advisory-tests"
    } else if report.is_landable() {
        "green"
    } else {
        "unverified"
    };
    let tree = report.verified_tree.clone().unwrap_or(
        crate::gate::snapshot_tree(candidate.workspace())
            .map_err(|error| environment_error("candidate_snapshot_failed", error.to_string()))?,
    );
    let diff = candidate_diff(candidate.workspace(), base_sha, &tree)?;
    write_private(&run_dir.join(format!("candidate-{rung}.diff")), &diff)?;
    let rung_number = rung
        .strip_prefix('R')
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(1);
    let mut metadata = CandidateSnapshot::from_report(
        rung,
        rung_number,
        String::new(),
        tree.clone(),
        verdict,
        &rung_reason,
        &report,
        &demoted,
        diff,
    );
    let setup_outputs = options
        .setup_outputs
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let commit = candidate
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
        &commit.commit,
        run_id,
        rung,
    )?;
    metadata.commit.clone_from(&commit.commit);
    snapshot.fields.insert("verdict".to_owned(), json!(verdict));
    snapshot.fields.insert("rung".to_owned(), json!(rung));
    snapshot.fields.insert(
        "advisory_items".to_owned(),
        json!(advisory.iter().cloned().collect::<Vec<_>>()),
    );
    snapshot
        .fields
        .insert("candidate_commit".to_owned(), json!(commit.commit));
    provider.record_event(
        snapshot,
        &RunEvent::new("rung_finished", now_ms())
            .with("rung", json!(rung))
            .with("reason", json!(rung_reason))
            .with("verdict", json!(verdict))
            .with("tree", json!(tree))
            .with(
                "candidate_ref",
                json!(format!("refs/kogen/candidates/{run_id}/{rung}")),
            )
            .with("diff_lines", json!(changed_line_count(&metadata.diff)))
            .with("turns", json!(develop.turns))
            .with("changed", json!(develop.changed))
            .with("repairs", json!(repairs))
            .with("tool_calls", json!(develop.tool_outputs.len()))
            .with("model_stages", json!(develop.model_stages)),
    )?;
    Ok(RungResult {
        candidate,
        metadata,
        report,
        demoted,
        landable,
    })
}

#[cfg(test)]
mod tests {
    use super::rung_gate_dir;
    use std::path::Path;

    #[test]
    fn parallel_rungs_have_distinct_gate_report_directories() {
        let run_dir = Path::new("/tmp/run");
        assert_eq!(rung_gate_dir(run_dir, "R1"), run_dir.join("gate-R1"));
        assert_eq!(rung_gate_dir(run_dir, "R2"), run_dir.join("gate-R2"));
        assert_ne!(rung_gate_dir(run_dir, "R1"), rung_gate_dir(run_dir, "R2"));
    }
}

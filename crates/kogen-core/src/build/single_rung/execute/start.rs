use super::*;
use std::collections::BTreeMap;
use std::fs;

pub fn run(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
) -> Result<BuildOutcome, CoreError> {
    let run_id = crate::queue::new_run_id()?;
    let claim = match crate::queue::claim(&project.origin, &run_id)? {
        crate::queue::ClaimStart::Acquired(claim) => claim,
        crate::queue::ClaimStart::Held { run_id } => {
            return Ok(BuildOutcome {
                status: BuildStatus::Stopped {
                    class: "environment".to_owned(),
                    reason: "build_already_claimed".to_owned(),
                },
                run_id,
                commit: String::new(),
                verdict: "none".to_owned(),
                reason: "build_already_claimed".to_owned(),
                stderr: String::new(),
                has_run: false,
            });
        }
    };

    let latest = ApprovedBuild::load(project, &approved.slug)?;
    if latest.commit != approved.commit || latest.approval_sha256 != approved.approval_sha256 {
        return Err(controller_error(
            "approval_invalid",
            "the approval changed while the queue was waiting",
        ));
    }
    let options = BuildOptions::load(project)?;
    let origin = crate::git::GitRepo::new(&project.origin);
    let base_sha = origin
        .resolve_commit(&project.base)
        .map_err(|error| environment_error("base_read_failed", error.to_string()))?;
    let started_ms = now_ms();
    let run_dir = project.state_root.join("runs").join(&run_id);
    fs::create_dir_all(run_dir.join("logs"))
        .and_then(|()| fs::create_dir_all(run_dir.join("tmp")))
        .map_err(|error| environment_error("run_directory_unavailable", error.to_string()))?;
    let store = RunStore::new(&run_dir);
    let mut snapshot = RunSnapshot {
        schema: 2,
        run_id: run_id.clone(),
        slug: approved.slug.clone(),
        approval_sha256: approved.approval_sha256.clone(),
        approval_commit: approved.commit.clone(),
        target_branch: approved.target_branch.clone(),
        status: "running".to_owned(),
        landing: None,
        owner_pid: std::process::id(),
        owner_started_ms: crate::recovery::process_started_ms(std::process::id())
            .unwrap_or(started_ms / 1000 * 1000),
        started_ms,
        fields: BTreeMap::new(),
    };
    store
        .create(&snapshot)
        .map_err(|error| controller_error("run_journal_failed", error.to_string()))?;

    let candidate_path = project.state_root.join(format!("{run_id}-R1"));
    let base_path = project.state_root.join(format!("{run_id}-base"));
    let result = super::run_rung(
        project,
        approved,
        &options,
        &base_sha,
        &run_dir,
        &candidate_path,
        &base_path,
        &store,
        &mut snapshot,
    );
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            cleanup_path(&candidate_path);
            cleanup_path(&base_path);
            stop_run(&store, &mut snapshot, &error)?;
            BuildOutcome {
                status: BuildStatus::Stopped {
                    class: error.class.as_str().to_owned(),
                    reason: error.reason.clone(),
                },
                run_id: run_id.clone(),
                commit: String::new(),
                verdict: snapshot
                    .fields
                    .get("verdict")
                    .and_then(Value::as_str)
                    .unwrap_or("none")
                    .to_owned(),
                reason: error.reason,
                stderr: snapshot
                    .fields
                    .get("sandbox_warning")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                has_run: true,
            }
        }
    };
    drop(claim);
    Ok(outcome)
}

//! Public queue dispatch and the pluggable single-rung Build core.

mod approval;
mod config;
mod interrupt;
mod provider;
mod provider_error;
mod provider_journal;
mod provider_prompt;
mod queue;
mod single_rung;
mod support;

use config::BuildOptions;

pub use queue::{queue_start, queue_stop};

use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};

pub(crate) struct WitnessBuildResult {
    pub proven: bool,
    pub warnings: Vec<crate::intent::shaping::ShapeWarning>,
}

fn controller_error(reason: &str, detail: impl Into<String>) -> CoreError {
    CoreError::new(ErrorClass::Controller, reason, detail, ExitCode::Bug)
}

fn environment_error(reason: &str, detail: impl Into<String>) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        detail,
        ExitCode::Environment,
    )
}

pub(crate) fn run_witness_build(
    project: &crate::project::ProjectResolution,
    slug: &str,
    intent_bytes: Vec<u8>,
    acceptance_path: String,
    acceptance_bytes: Vec<u8>,
) -> Result<WitnessBuildResult, CoreError> {
    let origin = crate::git::GitRepo::new(&project.origin);
    let base_sha = origin
        .resolve_commit(&project.base)
        .map_err(|error| environment_error("base_read_failed", error.to_string()))?;
    let intent = crate::intent::Intent::parse(slug, &intent_bytes)
        .map_err(|error| controller_error("intent_invalid", error.to_string()))?;
    let options = BuildOptions::load(project)?;
    let mut manifest = crate::approval::witness_build_manifest(
        project,
        &base_sha,
        &intent,
        &intent_bytes,
        &acceptance_path,
        &acceptance_bytes,
    )
    .map_err(|error| controller_error("protected_manifest_invalid", error))?;
    if origin
        .blob_at(&base_sha, &acceptance_path)
        .map_err(|error| environment_error("protected_manifest_read_failed", error.to_string()))?
        .is_none()
    {
        manifest.insert(
            acceptance_path.clone(),
            crate::gate::ABSENT_SHA256.to_owned(),
        );
    }
    let approved = approval::ApprovedBuild {
        slug: slug.to_owned(),
        commit: base_sha.clone(),
        approval_sha256: crate::intent::approval_sha256(&intent_bytes, &acceptance_bytes),
        target_branch: project.base.clone(),
        base_sha: base_sha.clone(),
        intent_bytes,
        acceptance_path,
        acceptance_bytes,
        approval: serde_json::json!({"protected_manifest":manifest,"check_baseline":[]}),
        intent,
        approval_time: 0,
    };
    let run_id = crate::queue::new_run_id()?;
    let run_dir = project
        .state_root
        .join("runs")
        .join(format!("{run_id}-witness"));
    let started_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let mut snapshot = crate::run::RunSnapshot {
        schema: 2,
        run_id,
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
        recovery: Vec::new(),
        cleanup_pending: false,
        fields: Default::default(),
    };
    let store = crate::run::RunStore::new(&run_dir);
    store
        .create(&snapshot)
        .map_err(|error| controller_error("run_journal_failed", error.to_string()))?;
    for directory in ["logs", "tmp", "reports"] {
        crate::safe_fs::ensure_dir(&run_dir, std::path::Path::new(directory))
            .map_err(|error| environment_error("run_directory_unavailable", error.to_string()))?;
    }
    single_rung::run_witness_build(
        project,
        &approved,
        &options,
        &base_sha,
        &run_dir,
        &store,
        &mut snapshot,
    )
}

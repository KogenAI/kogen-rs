use super::super::approval::ApprovedBuild;
use super::super::config::BuildOptions;
use super::super::support::{self, IntegritySnapshot};
use super::super::{controller_error, environment_error};
use crate::error::CoreError;
use crate::git::landing::{
    IntegrationGate, IntegrationResult, LandingOutcome, LandingRepository, LandingRequest,
    LandingWait, RebaseAttempt, RebaseKind, RepairResult,
};
use crate::project::ProjectResolution;
use crate::run::{ChildEnvironment, ProcessSupervisor, RunSnapshot, RunStore};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) struct LandResult {
    pub outcome: LandingOutcome,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn land_candidate(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    candidate: &LandingRepository,
    expected_parent: &str,
    tree: &str,
    integrity: &IntegritySnapshot,
    supervisor: &ProcessSupervisor,
) -> Result<LandResult, CoreError> {
    let mut integration = Reverify {
        project,
        approved,
        options,
        store,
        run_id: snapshot.run_id.clone(),
        run_dir: store.directory().to_path_buf(),
        candidate_workspace: candidate.workspace().to_path_buf(),
        integrity,
        supervisor,
    };
    let mut wait = ScaledLandingWait;
    let setup_outputs = options
        .setup_outputs
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let outcome = crate::git::landing::land_excluding(
        LandingRequest {
            repository: candidate,
            store,
            snapshot,
            title: &approved.intent.frontmatter.title,
            expected_parent,
            verified_tree: tree,
        },
        &setup_outputs,
        &mut integration,
        &mut wait,
    )
    .map_err(|error| match error.kind {
        crate::git::landing::LandingErrorKind::Controller
        | crate::git::landing::LandingErrorKind::InvalidRequest => {
            controller_error("landing_failed", error.to_string())
        }
        _ => environment_error("landing_failed", error.to_string()),
    })?;
    Ok(LandResult { outcome })
}

struct Reverify<'a> {
    project: &'a ProjectResolution,
    approved: &'a ApprovedBuild,
    options: &'a BuildOptions,
    store: &'a RunStore,
    run_id: String,
    run_dir: PathBuf,
    candidate_workspace: PathBuf,
    integrity: &'a IntegritySnapshot,
    supervisor: &'a ProcessSupervisor,
}

impl IntegrationGate for Reverify<'_> {
    fn reverify_and_repair(
        &mut self,
        _workspace: &Path,
        new_parent: &str,
        rebase: &RebaseAttempt,
        _deadline: Instant,
    ) -> Result<IntegrationResult, crate::git::landing::LandingError> {
        if !matches!(rebase, RebaseAttempt::Clean) {
            return Ok(IntegrationResult {
                rebase: RebaseKind::Conflict,
                repairs: vec![RepairResult::Red],
                verified_tree: None,
            });
        }
        let base_path = self
            .project
            .state_root
            .join(format!("{}-moved-base", self.run_id));
        let base = LandingRepository::clone_fresh(&self.project.origin, &base_path, new_parent)
            .map_err(landing_failure)?;
        let environment = support::child_environment(
            self.project,
            &self.run_dir,
            &self.candidate_workspace,
            self.options,
        )
        .map_err(|error| landing_failure(core_error_text(error)))?;
        let result = self.verify_moved_base(&base, new_parent, &environment);
        let _ = std::fs::remove_dir_all(base.workspace());
        result
    }
}

impl Reverify<'_> {
    fn verify_moved_base(
        &self,
        base: &LandingRepository,
        new_parent: &str,
        candidate_env: &ChildEnvironment,
    ) -> Result<IntegrationResult, crate::git::landing::LandingError> {
        let protection =
            support::protected_workspace(self.project, self.approved, self.options, new_parent)
                .map_err(|error| landing_failure(core_error_text(error)))?;
        let request = support::gate_request(
            self.project,
            self.approved,
            self.options,
            &self.run_dir,
            base.workspace(),
            &self.candidate_workspace,
            candidate_env.clone(),
            protection,
        )
        .map_err(|error| landing_failure(core_error_text(error)))?;
        let runner = support::sandboxed_pair(
            self.project,
            self.options,
            &self.candidate_workspace,
            base.workspace(),
            &self.run_dir,
            self.supervisor,
            self.integrity,
        );
        let report = crate::gate::run_gate(&runner, &request).map_err(landing_failure)?;
        let green = report.is_landable();
        let tree = report.verified_tree.clone();
        let event = crate::run::RunEvent::new("verification", now_ms())
            .with("rung", serde_json::json!("R1"))
            .with("tree", serde_json::json!(tree))
            .with(
                "result",
                serde_json::json!(if green { "green" } else { "red" }),
            );
        let snapshot = self.store.read_snapshot().map_err(landing_failure)?;
        self.store
            .record(&event, &snapshot)
            .map_err(landing_failure)?;
        if green {
            Ok(IntegrationResult {
                rebase: RebaseKind::Green,
                repairs: Vec::new(),
                verified_tree: tree,
            })
        } else {
            Ok(IntegrationResult {
                rebase: RebaseKind::Red,
                repairs: vec![RepairResult::Red],
                verified_tree: None,
            })
        }
    }
}

fn landing_failure(error: impl std::fmt::Display) -> crate::git::landing::LandingError {
    crate::git::landing::LandingError {
        kind: crate::git::landing::LandingErrorKind::Controller,
        operation: "reverify moved base",
        detail: error.to_string(),
    }
}

fn core_error_text(error: CoreError) -> String {
    format!(
        "{}/{}: {}",
        error.class.as_str(),
        error.reason,
        error.detail
    )
}

#[derive(Default)]
struct ScaledLandingWait;

impl LandingWait for ScaledLandingWait {
    fn wait(&mut self, delay: Duration) {
        let scale = std::env::var("KOGEN_TIME_SCALE")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(1.0);
        std::thread::sleep(Duration::from_millis(
            (delay.as_millis() as f64 * scale).floor().max(1.0) as u64,
        ));
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

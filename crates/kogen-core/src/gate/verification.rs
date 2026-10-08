use super::checks::{
    CheckBaseline, CheckCommand, CheckResult, CheckRunError, FixResult, is_excused,
    run_check_excluding, run_fix,
};
use super::ledger::{AcceptanceFailure, CommandAcceptanceResult};
use super::protection::{
    ProtectedFinding, ProtectedWorkspace, ProtectionError, install_approved_acceptance,
};
use super::tree::{TreeSnapshotError, snapshot_tree_excluding};
use super::workspace::{WorkspaceError, WorkspaceTree};
use crate::run::{ChildEnvironment, ProcessError, ProcessPort};
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

#[path = "verification_support.rs"]
mod support;

#[derive(Clone, Debug)]
pub struct AcceptancePlan {
    pub slug: String,
    pub source_path: String,
    pub candidate_path: String,
    pub approved_bytes: Vec<u8>,
    pub command: Vec<std::ffi::OsString>,
    pub timeout: std::time::Duration,
    pub expected_items: BTreeSet<String>,
    pub change_items: BTreeSet<String>,
    pub adapter_unavailable: bool,
}

#[derive(Clone, Debug)]
pub struct GateRequest {
    pub base_workspace: PathBuf,
    pub candidate_workspace: PathBuf,
    pub run_dir: PathBuf,
    pub environment: ChildEnvironment,
    pub fixes: Vec<CheckCommand>,
    pub checks: Vec<CheckCommand>,
    pub setup_outputs: Vec<PathBuf>,
    pub approved_baseline: Vec<CheckBaseline>,
    pub acceptance: AcceptancePlan,
    pub protection: ProtectedWorkspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateVerdict {
    Green,
    Unverified,
    None,
}

impl GateVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Unverified => "unverified",
            Self::None => "none",
        }
    }
}

/// A receipt can only be created by a complete green verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationReceipt {
    tree_id: String,
}

impl VerificationReceipt {
    #[must_use]
    pub fn tree_id(&self) -> &str {
        &self.tree_id
    }
}

#[derive(Clone, Debug)]
pub struct GateReport {
    pub verdict: GateVerdict,
    pub verified_tree: Option<String>,
    pub fix_results: Vec<FixResult>,
    pub base_checks: Vec<CheckResult>,
    pub checks: Vec<CheckResult>,
    pub acceptance: CommandAcceptanceResult,
    pub protection_findings: Vec<ProtectedFinding>,
    pub restored_paths: Vec<String>,
    pub demoted_items: BTreeSet<String>,
    change_item_passes: bool,
    receipt: Option<VerificationReceipt>,
}

impl GateReport {
    #[must_use]
    pub fn is_verified(&self) -> bool {
        self.receipt.is_some()
    }

    #[must_use]
    pub fn is_landable(&self) -> bool {
        self.is_verified() && self.change_item_passes
    }

    #[must_use]
    pub fn receipt(&self) -> Option<&VerificationReceipt> {
        self.receipt.as_ref()
    }

    /// Auditor advice is observational until an exact policy is admitted.
    /// Retained for callers replaying historical advice; it cannot create a receipt.
    pub fn apply_acceptance_demotions(
        &mut self,
        _demoted: &BTreeSet<String>,
        _change_items: &BTreeSet<String>,
    ) -> bool {
        self.is_verified()
    }
}

#[derive(Debug)]
pub enum GateError {
    InvalidRequest(String),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Protection(ProtectionError),
    Check(CheckRunError),
    Acceptance(super::ledger::AcceptanceRunError),
    Snapshot(TreeSnapshotError),
    Workspace(WorkspaceError),
    Process(ProcessError),
}

impl fmt::Display for GateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(detail) => write!(formatter, "invalid gate request: {detail}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::Protection(error) => write!(formatter, "{error}"),
            Self::Check(error) => write!(formatter, "{error}"),
            Self::Acceptance(error) => write!(formatter, "{error}"),
            Self::Snapshot(error) => write!(formatter, "{error}"),
            Self::Workspace(error) => write!(formatter, "{error}"),
            Self::Process(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for GateError {}

impl From<ProtectionError> for GateError {
    fn from(error: ProtectionError) -> Self {
        Self::Protection(error)
    }
}

impl From<CheckRunError> for GateError {
    fn from(error: CheckRunError) -> Self {
        Self::Check(error)
    }
}

impl From<super::ledger::AcceptanceRunError> for GateError {
    fn from(error: super::ledger::AcceptanceRunError) -> Self {
        Self::Acceptance(error)
    }
}

impl From<TreeSnapshotError> for GateError {
    fn from(error: TreeSnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

impl From<WorkspaceError> for GateError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<ProcessError> for GateError {
    fn from(error: ProcessError) -> Self {
        Self::Process(error)
    }
}

/// Installs the approved test, applies fixes once, runs checks on both trees,
/// evaluates the ledger, and issues a receipt only for a green unchanged tree.
pub fn run_gate(runner: &dyn ProcessPort, request: &GateRequest) -> Result<GateReport, GateError> {
    support::validate_request(request)?;
    support::ensure_private_run_dir(&request.run_dir)?;
    install_approved_acceptance(
        &request.candidate_workspace,
        &request.acceptance.source_path,
        &request.acceptance.candidate_path,
        &request.acceptance.approved_bytes,
    )?;

    let mut restored_paths = request
        .protection
        .restore_after_batch(&request.candidate_workspace)?;
    let mut protection_findings = request.protection.guard(&request.candidate_workspace)?;

    let mut fix_results = Vec::with_capacity(request.fixes.len());
    for fix in &request.fixes {
        let result = run_fix(
            runner,
            fix,
            &request.candidate_workspace,
            &request.run_dir,
            &request.environment,
        );
        restored_paths.extend(
            request
                .protection
                .restore_after_batch(&request.candidate_workspace)?,
        );
        fix_results.push(result?);
    }
    protection_findings.extend(request.protection.guard(&request.candidate_workspace)?);

    let verified_snapshot =
        WorkspaceTree::capture_excluding(&request.candidate_workspace, &request.setup_outputs)?;
    let verified_tree = Some(snapshot_tree_excluding(
        &request.candidate_workspace,
        &request.setup_outputs,
    )?);
    let mut base_checks = Vec::with_capacity(request.checks.len());
    let mut checks = Vec::with_capacity(request.checks.len());
    for command in &request.checks {
        base_checks.push(run_check_excluding(
            runner,
            command,
            &request.base_workspace,
            &request.run_dir,
            &request.environment,
            &request.setup_outputs,
        )?);
        let mut current = run_check_excluding(
            runner,
            command,
            &request.candidate_workspace,
            &request.run_dir,
            &request.environment,
            &request.setup_outputs,
        )?;
        let approved = request
            .approved_baseline
            .iter()
            .find(|baseline| baseline.name == command.name)
            .cloned();
        let baseline = approved.or_else(|| {
            base_checks.last().map(|base| CheckBaseline {
                name: base.name.clone(),
                status: base.status,
                exit_status: base.exit_status,
                findings: base.findings.clone(),
            })
        });
        if let Some(baseline) = baseline {
            current.excused = is_excused(&baseline, &current);
        }
        checks.push(current);
    }

    let acceptance_before =
        WorkspaceTree::capture_excluding(&request.candidate_workspace, &request.setup_outputs)?;
    let acceptance_result = support::run_acceptance(runner, request);
    let acceptance_after = match WorkspaceTree::capture_excluding(
        &request.candidate_workspace,
        &request.setup_outputs,
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            acceptance_before.restore()?;
            restored_paths.extend(
                request
                    .protection
                    .restore_after_batch(&request.candidate_workspace)?,
            );
            return Err(GateError::Workspace(error));
        }
    };
    let acceptance_changed = !acceptance_before
        .changed_paths(&acceptance_after)
        .is_empty();
    if acceptance_changed {
        acceptance_before.restore()?;
    }
    restored_paths.extend(
        request
            .protection
            .restore_after_batch(&request.candidate_workspace)?,
    );
    protection_findings.extend(request.protection.guard(&request.candidate_workspace)?);
    let acceptance_result = acceptance_result?;

    let final_tree = snapshot_tree_excluding(&request.candidate_workspace, &request.setup_outputs)?;
    if final_tree != verified_tree.as_deref().unwrap_or_default() {
        verified_snapshot.restore()?;
    }
    let tree_stable =
        snapshot_tree_excluding(&request.candidate_workspace, &request.setup_outputs)?
            == verified_tree.as_deref().unwrap_or_default();
    let mut acceptance = acceptance_result;
    if (acceptance_changed || !tree_stable)
        && !acceptance
            .failures
            .contains(&AcceptanceFailure::TreeMutated)
    {
        acceptance.failures.push(AcceptanceFailure::TreeMutated);
    }
    let protection_findings = support::unique_protection_findings(protection_findings);
    let checks_green = checks.iter().all(|check| !check.blocks_gate());
    let fixes_green = fix_results.iter().all(FixResult::passed);
    let is_green = fixes_green
        && checks_green
        && acceptance.failures.is_empty()
        && acceptance.item_pass.values().all(|passed| *passed)
        && !acceptance.item_pass.is_empty()
        && protection_findings.is_empty()
        && tree_stable;
    let changed_item_passes = request
        .acceptance
        .change_items
        .iter()
        .any(|item| acceptance.item_pass.get(item) == Some(&true));
    let receipt = is_green.then(|| VerificationReceipt {
        tree_id: verified_tree.clone().unwrap_or_default(),
    });
    Ok(GateReport {
        verdict: if is_green {
            GateVerdict::Green
        } else {
            GateVerdict::Unverified
        },
        verified_tree,
        fix_results,
        base_checks,
        checks,
        acceptance,
        protection_findings,
        restored_paths: support::deduplicate_paths(restored_paths),
        demoted_items: BTreeSet::new(),
        change_item_passes: changed_item_passes,
        receipt,
    })
}

#[cfg(test)]
#[path = "verification_tests.rs"]
mod tests;

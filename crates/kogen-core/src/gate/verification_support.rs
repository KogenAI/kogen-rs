use super::super::ledger::{
    CommandAcceptanceRequest, CommandAcceptanceResult, run_command_acceptance,
};
use super::super::protection::ProtectedFinding;
use super::super::tree::GitTreeSnapshotWithExclusions;
use super::{GateError, GateRequest};
use crate::run::ProcessPort;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn run_acceptance(
    runner: &dyn ProcessPort,
    request: &GateRequest,
) -> Result<CommandAcceptanceResult, GateError> {
    let command = CommandAcceptanceRequest {
        slug: request.acceptance.slug.clone(),
        command: request.acceptance.command.clone(),
        candidate_path: request
            .candidate_workspace
            .join(&request.acceptance.candidate_path),
        workdir: request.candidate_workspace.clone(),
        run_dir: request.run_dir.clone(),
        report_path: request.run_dir.join("reports/ledger.jsonl"),
        env: request.environment.clone(),
        timeout: request.acceptance.timeout,
        expected_items: request.acceptance.expected_items.clone(),
        adapter_unavailable: request.acceptance.adapter_unavailable,
    };
    let tree = GitTreeSnapshotWithExclusions::new(request.setup_outputs.clone());
    run_command_acceptance(runner, &tree, command).map_err(GateError::Acceptance)
}

pub(super) fn validate_request(request: &GateRequest) -> Result<(), GateError> {
    if request.acceptance.command.is_empty() || request.acceptance.command[0].is_empty() {
        return Err(GateError::InvalidRequest(
            "acceptance adapter requires a nonempty argv".to_owned(),
        ));
    }
    if request.acceptance.expected_items.is_empty() {
        return Err(GateError::InvalidRequest(
            "Intent has no tagged acceptance items".to_owned(),
        ));
    }
    if !request
        .acceptance
        .change_items
        .is_subset(&request.acceptance.expected_items)
    {
        return Err(GateError::InvalidRequest(
            "change items must be tagged acceptance items".to_owned(),
        ));
    }
    let base = resolve_workspace(&request.base_workspace, "base")?;
    let candidate = resolve_workspace(&request.candidate_workspace, "candidate")?;
    if base == candidate {
        return Err(GateError::InvalidRequest(
            "base and candidate must use separate workspaces".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn ensure_private_run_dir(path: &Path) -> Result<(), GateError> {
    crate::safe_fs::ensure_directory_path(path).map_err(|source| GateError::Io {
        operation: "create run directory",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| GateError::Io {
        operation: "inspect run directory",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(GateError::InvalidRequest(
            "run directory must be a real directory".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            GateError::Io {
                operation: "protect run directory",
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

pub(super) fn unique_protection_findings(findings: Vec<ProtectedFinding>) -> Vec<ProtectedFinding> {
    let mut unique = std::collections::BTreeMap::new();
    for finding in findings {
        unique.insert(finding.path.clone(), finding);
    }
    unique.into_values().collect()
}

pub(super) fn deduplicate_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn resolve_workspace(path: &Path, name: &str) -> Result<PathBuf, GateError> {
    fs::canonicalize(path).map_err(|source| GateError::Io {
        operation: if name == "base" {
            "resolve base workspace"
        } else {
            "resolve candidate workspace"
        },
        path: path.to_path_buf(),
        source,
    })
}

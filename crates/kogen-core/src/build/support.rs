use super::approval::ApprovedBuild;
use super::config::BuildOptions;
use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use crate::gate::{AcceptancePlan, GateRequest, ProtectedEntry, ProtectedWorkspace};
use crate::project::ProjectResolution;
use crate::run::{
    ChildEnvironment, EnvironmentRequest, ProcessError, ProcessPort, ProcessRequest,
    ProcessSupervisor, SandboxIntegrityPort, SandboxPolicy, SandboxedProcessPort,
};
use std::path::{Path, PathBuf};

mod acceptance;
pub(super) use acceptance::candidate_path;
use acceptance::{acceptance_request, baseline};

pub(super) struct IntegritySnapshot {
    checkout: PathBuf,
    origin: PathBuf,
}

impl IntegritySnapshot {
    pub fn new(project: &ProjectResolution) -> Self {
        Self {
            checkout: project.checkout.clone(),
            origin: project.origin.clone(),
        }
    }
}

impl SandboxIntegrityPort for IntegritySnapshot {
    fn snapshot(&self) -> Result<String, String> {
        let checkout = crate::git::GitRepo::new(&self.checkout);
        let origin = crate::git::GitRepo::new(&self.origin);
        let mut material = Vec::new();
        for args in [
            vec!["rev-parse", "HEAD"],
            vec!["diff", "--binary", "HEAD", "--"],
            vec!["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        ] {
            material.extend(checkout.output(&args).map_err(|error| error.to_string())?);
            material.push(0);
        }
        material.extend(
            origin
                .output(&["for-each-ref", "--format=%(refname)%00%(objectname)"])
                .map_err(|error| error.to_string())?,
        );
        use sha2::{Digest as _, Sha256};
        Ok(format!("{:x}", Sha256::digest(material)))
    }
}

pub(super) fn sandboxed<'a>(
    _project: &ProjectResolution,
    options: &BuildOptions,
    workspace: &Path,
    run_dir: &Path,
    process: &'a ProcessSupervisor,
    integrity: &'a IntegritySnapshot,
) -> SandboxedProcessPort<'a> {
    let host = crate::run::host_environment();
    let policy = SandboxPolicy::for_build(options.sandbox, workspace, run_dir, &host);
    SandboxedProcessPort::new(process, policy, Some(integrity))
}

pub(super) fn sandboxed_pair<'a>(
    _project: &ProjectResolution,
    options: &BuildOptions,
    candidate: &Path,
    base: &Path,
    run_dir: &Path,
    process: &'a ProcessSupervisor,
    integrity: &'a IntegritySnapshot,
) -> SandboxedProcessPort<'a> {
    let host = crate::run::host_environment();
    let mut policy = SandboxPolicy::for_build(options.sandbox, candidate, run_dir, &host);
    policy.allow_write(base.to_path_buf());
    SandboxedProcessPort::new(process, policy, Some(integrity))
}

pub(super) fn child_environment(
    project: &ProjectResolution,
    run_dir: &Path,
    workspace: &Path,
    options: &BuildOptions,
) -> Result<ChildEnvironment, CoreError> {
    let mut request = EnvironmentRequest::new(
        crate::run::host_environment(),
        run_dir,
        &project.checkout,
        workspace,
    );
    request.project.clone_from(&options.environment);
    crate::run::build_child_environment(&ProcessSupervisor, request).map_err(|error| {
        CoreError::new(
            ErrorClass::Environment,
            "child_environment_failed",
            error.to_string(),
            ExitCode::Environment,
        )
    })
}

pub(super) fn run_setup(
    runner: &dyn ProcessPort,
    workspace: &Path,
    run_dir: &Path,
    environment: &ChildEnvironment,
    setup: &[crate::gate::CheckCommand],
) -> Result<bool, CoreError> {
    for command in setup {
        let Some((program, args)) = command.argv.split_first() else {
            continue;
        };
        let mut succeeded = false;
        for attempt in 0..2 {
            let mut request = ProcessRequest::new(program.clone(), workspace, run_dir);
            request.args = args.to_vec();
            request.env.clone_from(environment);
            request.timeout = command.timeout;
            request.log_name = if attempt == 0 {
                format!("setup-{}", safe_log_name(&command.name))
            } else {
                format!("setup-{}-retry", safe_log_name(&command.name))
            };
            let result = runner
                .run(request)
                .map_err(|error| process_error("setup_failed", error))?;
            if !result.timed_out && !result.unavailable && result.exit_status == Some(0) {
                succeeded = true;
                break;
            }
        }
        if !succeeded {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn run_setup_cached(
    project: &ProjectResolution,
    options: &BuildOptions,
    runner: &dyn ProcessPort,
    workspace: &Path,
    run_dir: &Path,
    environment: &ChildEnvironment,
) -> Result<(String, crate::run::setup_cache::SetupCacheOutcome), CoreError> {
    let tree = crate::git::GitRepo::new(workspace)
        .resolve_tree("HEAD")
        .map_err(|error| environment_error("setup_key_failed", error.to_string()))?;
    let key = crate::run::setup_cache::SetupCacheKey::from_project(
        project.config.as_ref(),
        workspace,
        &tree,
        environment,
    )
    .map_err(|error| environment_error("setup_key_failed", error.to_string()))?;
    let key = key.digest();
    let cache_root = project.state_root.join("setup-cache");
    let request = crate::run::setup_cache::SetupCacheRequest {
        checkout: workspace,
        cache_root: &cache_root,
        key: &key,
        outputs: options.setup_outputs.clone(),
        enabled: true,
    };
    let outcome = crate::run::setup_cache::run_setup(request, || {
        let before = std::time::Instant::now();
        let ok = run_setup(runner, workspace, run_dir, environment, &options.setup)?;
        if !ok {
            return Err(environment_error(
                "setup_failed",
                "setup failed after one retry",
            ));
        }
        Ok(before.elapsed().as_millis().min(u64::MAX as u128) as u64)
    })?;
    Ok((key, outcome))
}

pub(super) fn base_acceptance(
    runner: &dyn ProcessPort,
    options: &BuildOptions,
    approved: &ApprovedBuild,
    workspace: &Path,
    run_dir: &Path,
    environment: &ChildEnvironment,
) -> Result<crate::gate::CommandAcceptanceResult, CoreError> {
    let relative = candidate_path(options, &approved.slug);
    let path = workspace.join(&relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| environment_error("workspace_write_failed", error))?;
    }
    std::fs::write(&path, &approved.acceptance_bytes)
        .map_err(|error| environment_error("workspace_write_failed", error))?;
    let request = acceptance_request(
        options,
        approved,
        &path,
        workspace,
        run_dir,
        environment.clone(),
        "ledger-base.jsonl",
    );
    let tree = crate::gate::GitTreeSnapshotWithExclusions::new(
        options.setup_outputs.iter().map(PathBuf::from).collect(),
    );
    let result = if options.adapter == "exunit" {
        crate::gate::adapters::exunit::run_acceptance(runner, &tree, request, true)
            .map_err(|error| environment_error("acceptance_runner_failed", error))?
    } else {
        crate::gate::run_command_acceptance(runner, &tree, request)
            .map_err(|error| environment_error("acceptance_runner_failed", error))?
    };
    std::fs::remove_file(path)
        .map_err(|error| environment_error("workspace_cleanup_failed", error))?;
    Ok(result)
}

pub(super) fn protected_workspace(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
) -> Result<ProtectedWorkspace, CoreError> {
    let hashes = approved.protected_hashes()?;
    let origin = crate::git::GitRepo::new(&project.origin);
    let intent_path = format!(".kogen/intents/{}/intent.md", approved.slug);
    let candidate_path = candidate_path(options, &approved.slug)
        .to_string_lossy()
        .into_owned();
    let mut manifest = std::collections::BTreeMap::new();
    for (path, expected) in hashes {
        let bytes = if path == intent_path {
            Some(approved.intent_bytes.clone())
        } else if path == candidate_path {
            Some(approved.acceptance_bytes.clone())
        } else {
            origin
                .blob_at(base_sha, &path)
                .map_err(|error| environment_error("protected_manifest_read_failed", error))?
        };
        if let Some(bytes) = &bytes
            && crate::intent::intent_sha256(bytes) != expected
        {
            return Err(controller_error(
                "approval_invalid",
                format!("protected manifest does not match base path {path}"),
            ));
        }
        if bytes.is_none() && expected != crate::gate::ABSENT_SHA256 {
            return Err(controller_error(
                "approval_invalid",
                format!("protected manifest bytes are unavailable for {path}"),
            ));
        }
        manifest.insert(
            path,
            ProtectedEntry {
                sha256: expected,
                bytes,
            },
        );
    }
    ProtectedWorkspace::new(manifest, vec![approved.acceptance_path.clone()])
        .map_err(|error| controller_error("protected_manifest_invalid", error))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn gate_request(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    run_dir: &Path,
    base_workspace: &Path,
    candidate_workspace: &Path,
    environment: ChildEnvironment,
    protection: ProtectedWorkspace,
) -> Result<GateRequest, CoreError> {
    let acceptance_relative = candidate_path(options, &approved.slug);
    let mut command = options.acceptance_run.clone();
    if options.adapter == "exunit" {
        let formatter = crate::gate::adapters::exunit::write_formatter(run_dir)
            .map_err(|error| environment_error("acceptance_runner_failed", error))?;
        let use_mise = std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("mise").is_file()));
        command = crate::gate::adapters::exunit::runner_command(&formatter, use_mise)
            .map_err(|error| environment_error("acceptance_runner_failed", error))?;
    }
    let acceptance = AcceptancePlan {
        slug: approved.slug.clone(),
        source_path: approved.acceptance_path.clone(),
        candidate_path: acceptance_relative.to_string_lossy().into_owned(),
        approved_bytes: approved.acceptance_bytes.clone(),
        command,
        timeout: options.acceptance_timeout,
        expected_items: approved
            .intent
            .verify
            .iter()
            .map(|item| item.id.clone())
            .collect(),
        change_items: approved
            .intent
            .verify
            .iter()
            .filter(|item| item.is_change())
            .map(|item| item.id.clone())
            .collect(),
        adapter_unavailable: false,
    };
    let approved_baseline = baseline(options, approved)?;
    let _ = project;
    Ok(GateRequest {
        base_workspace: base_workspace.to_path_buf(),
        candidate_workspace: candidate_workspace.to_path_buf(),
        run_dir: run_dir.to_path_buf(),
        environment,
        fixes: options.fixes.clone(),
        checks: options.checks.clone(),
        setup_outputs: options.setup_outputs.iter().map(PathBuf::from).collect(),
        approved_baseline,
        acceptance,
        protection,
    })
}

fn safe_log_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn process_error(reason: &str, error: ProcessError) -> CoreError {
    environment_error(reason, error)
}

fn environment_error(reason: &str, detail: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        detail.to_string(),
        ExitCode::Environment,
    )
}

fn controller_error(reason: &str, detail: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Controller,
        reason,
        detail.to_string(),
        ExitCode::Bug,
    )
}

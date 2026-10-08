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
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

mod acceptance;
pub(super) use acceptance::candidate_path;
use acceptance::{AcceptanceContext, acceptance_request, baseline};

pub(super) fn gate_feedback(
    approved: &ApprovedBuild,
    report: &crate::gate::GateReport,
    run_dir: &Path,
) -> String {
    let mut lines = Vec::new();
    let mut total_findings = 0_usize;
    let mut errors = 0_usize;
    let warnings = report.checks.iter().filter(|check| check.excused).count();
    let mut tool_counts = BTreeMap::<String, usize>::new();
    let mut omitted_findings = BTreeMap::<String, usize>::new();
    for fix in report.fix_results.iter().filter(|fix| !fix.passed()) {
        lines.push(format!("fix/{}: {}", fix.name, fix_failure_status(fix)));
        lines.push(format!("raw log: {}", fix.log_path.display()));
        append_raw_tail(&mut lines, &fix.name, &fix.log_path, run_dir);
    }
    for check in report.checks.iter().filter(|check| check.blocks_gate()) {
        errors += 1;
        append_check_details(
            &mut lines,
            check,
            report
                .base_checks
                .iter()
                .find(|base| base.name == check.name),
            &mut total_findings,
            &mut tool_counts,
            &mut omitted_findings,
        );
        lines.push(format!("raw log: {}", check.log_path.display()));
        append_raw_tail(&mut lines, &check.name, &check.log_path, run_dir);
    }
    for (tool, count) in omitted_findings {
        lines.push(format!("… {count} more {tool} findings"));
    }
    let failed_items = approved
        .intent
        .verify
        .iter()
        .filter(|item| {
            report.acceptance.item_pass.get(&item.id) != Some(&true)
                && !report.demoted_items.contains(&item.id)
        })
        .collect::<Vec<_>>();
    for item in &failed_items {
        lines.push(format!("acceptance {}: failed", item.id));
    }
    if warnings > 0 {
        for check in report.checks.iter().filter(|check| check.excused) {
            lines.push(format!(
                "Base-red warning: check \"{}\" still has only findings recorded at approval.",
                check.name
            ));
        }
    }
    let check_status = report
        .checks
        .iter()
        .map(|check| format!("{}={}", check.name, check.status.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let passed = approved.intent.verify.len().saturating_sub(
        approved
            .intent
            .verify
            .iter()
            .filter(|item| report.acceptance.item_pass.get(&item.id) != Some(&true))
            .count(),
    );
    let counts = tool_counts
        .into_iter()
        .map(|(tool, count)| format!("{tool} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(format!(
        "gate: {errors} errors, {warnings} warnings ({counts}); checks {check_status}; acceptance {passed}/{}",
        approved.intent.verify.len()
    ));
    lines.join("\n")
}

fn append_check_details(
    lines: &mut Vec<String>,
    check: &crate::gate::CheckResult,
    base: Option<&crate::gate::CheckResult>,
    total_findings: &mut usize,
    tool_counts: &mut BTreeMap<String, usize>,
    omitted_findings: &mut BTreeMap<String, usize>,
) {
    use crate::gate::CheckStatus;

    match check.status {
        CheckStatus::Green => (),
        CheckStatus::Mutating => {
            let paths = check.changed_paths.join(", ");
            if paths.is_empty() {
                lines.push(format!("check {}: Mutating", check.name));
            } else {
                lines.push(format!(
                    "check {}: Mutating; changed paths: {paths}",
                    check.name
                ));
            }
        }
        CheckStatus::Timeout => lines.push(format!(
            "check {}: timed out after {} s",
            check.name,
            check.timeout.as_secs_f64()
        )),
        CheckStatus::Unavailable if base.is_some_and(|base| base.status == CheckStatus::Green) => {
            lines.push(format!(
                "{} is not available, but it ran on the base",
                check.program
            ));
        }
        CheckStatus::Unavailable => {
            lines.push(format!("check {}: Unavailable", check.name));
        }
        CheckStatus::Red => {
            lines.push(format!("check {}: Red", check.name));
            for finding in &check.findings {
                let tool = finding
                    .rule
                    .split('/')
                    .next()
                    .unwrap_or(&finding.rule)
                    .to_owned();
                let count = tool_counts.entry(tool.clone()).or_default();
                if *total_findings >= 20 || *count >= 10 {
                    *omitted_findings.entry(tool).or_default() += 1;
                    continue;
                }
                *total_findings += 1;
                *count += 1;
                let position = format!(
                    "{}:{}",
                    finding.line.unwrap_or(0),
                    finding.column.unwrap_or(1)
                );
                let symbol = if finding.symbol.is_empty() {
                    String::new()
                } else {
                    format!(" {}:", finding.symbol)
                };
                let message = finding.message.chars().take(200).collect::<String>();
                lines.push(format!(
                    "{}:{position}: error: [{}]{symbol} {message}",
                    finding.path, finding.rule
                ));
            }
        }
    }
}

fn fix_failure_status(fix: &crate::gate::FixResult) -> String {
    if fix.timed_out {
        "timed out".to_owned()
    } else if fix.unavailable {
        "unavailable".to_owned()
    } else {
        format!("exit {}", fix.exit_status.unwrap_or(-1))
    }
}

fn append_raw_tail(lines: &mut Vec<String>, step: &str, path: &Path, run_dir: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut tail = text.lines().rev().take(8).collect::<Vec<_>>();
    tail.reverse();
    let mut tail = tail.join("\n");
    tail = tail.replace(&run_dir.display().to_string(), "$TMPDIR");
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        tail = tail.replace(&home, "$HOME");
    }
    if !tail.is_empty() {
        let tail = tail.chars().take(600).collect::<String>();
        lines.push(format!("raw tail (first failed step {step}):\n{tail}"));
    }
}

pub(super) fn gate_failure_count(report: &crate::gate::GateReport) -> usize {
    let mut findings = BTreeSet::new();
    let mut checks_without_identity = 0_usize;
    for check in report.checks.iter().filter(|check| check.blocks_gate()) {
        if check.findings.is_empty() {
            checks_without_identity += 1;
        } else {
            findings.extend(check.findings.iter().map(|finding| {
                (
                    finding.path.clone(),
                    finding.rule.clone(),
                    finding.symbol.clone(),
                )
            }));
        }
    }
    let failed_items = report
        .acceptance
        .item_pass
        .values()
        .filter(|passed| !**passed)
        .count();
    findings.len() + checks_without_identity + failed_items
}

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
    project: &ProjectResolution,
    options: &BuildOptions,
    workspace: &Path,
    run_dir: &Path,
    process: &'a ProcessSupervisor,
    integrity: &'a IntegritySnapshot,
) -> SandboxedProcessPort<'a> {
    let policy = workspace_policy(project, options, workspace, run_dir);
    SandboxedProcessPort::new(process, policy, Some(integrity))
}

pub(super) fn sandboxed_pair<'a>(
    project: &ProjectResolution,
    options: &BuildOptions,
    candidate: &Path,
    base: &Path,
    run_dir: &Path,
    process: &'a ProcessSupervisor,
    integrity: &'a IntegritySnapshot,
) -> SandboxedProcessPort<'a> {
    let policy = pair_policy(project, options, candidate, base, run_dir);
    SandboxedProcessPort::new(process, policy, Some(integrity))
}

fn workspace_policy(
    project: &ProjectResolution,
    options: &BuildOptions,
    workspace: &Path,
    run_dir: &Path,
) -> SandboxPolicy {
    let host = crate::run::host_environment();
    let mut policy = SandboxPolicy::for_build(options.sandbox, workspace, run_dir, &host);
    policy.deny_write(project.checkout.clone());
    policy.deny_write(project.origin.clone());
    policy
}

fn pair_policy(
    project: &ProjectResolution,
    options: &BuildOptions,
    candidate: &Path,
    base: &Path,
    run_dir: &Path,
) -> SandboxPolicy {
    let mut policy = workspace_policy(project, options, candidate, run_dir);
    policy.allow_write(base.to_path_buf());
    policy
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
    let tree = crate::git::GitRepo::workspace(workspace)
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
    project: &ProjectResolution,
    runner: &dyn ProcessPort,
    options: &BuildOptions,
    approved: &ApprovedBuild,
    workspace: &Path,
    run_dir: &Path,
    environment: &ChildEnvironment,
) -> Result<crate::gate::CommandAcceptanceResult, CoreError> {
    let relative = candidate_path(options, &approved.slug);
    let path = workspace.join(&relative);
    crate::safe_fs::validate_write(workspace, &relative)
        .and_then(|()| crate::safe_fs::write_file(workspace, &relative, &approved.acceptance_bytes))
        .map_err(|error| environment_error("workspace_write_failed", error))?;
    let use_mise = workspace_policy(project, options, workspace, run_dir)
        .find_executable("mise", environment)
        .is_some();
    let request = acceptance_request(
        options,
        approved,
        AcceptanceContext {
            candidate_path: &path,
            workspace,
            run_dir,
            environment: environment.clone(),
            report: "ledger-base.jsonl",
            use_mise,
        },
    );
    let tree = crate::gate::GitTreeSnapshotWithExclusions::new(
        options.setup_outputs.iter().map(PathBuf::from).collect(),
    );
    let result = if options.adapter == "exunit" {
        crate::gate::adapters::exunit::run_acceptance(runner, &tree, request, use_mise)
            .map_err(|error| environment_error("acceptance_runner_failed", error))?
    } else {
        crate::gate::run_command_acceptance(runner, &tree, request)
            .map_err(|error| environment_error("acceptance_runner_failed", error))?
    };
    crate::safe_fs::remove_file(workspace, &relative)
        .map_err(|error| environment_error("workspace_cleanup_failed", error))?;
    Ok(result)
}

pub(super) fn base_acceptance_environment_error(
    options: &BuildOptions,
    result: &crate::gate::CommandAcceptanceResult,
) -> Option<CoreError> {
    if options.adapter == "exunit"
        && !result.process.timed_out
        && (result.process.unavailable
            || result.process.exit_status.is_some_and(|status| status != 0))
        && let Some(failure) =
            crate::gate::adapters::exunit::process_environment_failure(&result.process, true)
    {
        return Some(environment_error(failure.reason, failure.detail));
    }
    result
        .failures
        .contains(&crate::gate::AcceptanceFailure::ToolMissing)
        .then(|| {
            environment_error(
                "tool_missing",
                "acceptance runner is unavailable on the build base",
            )
        })
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
        if path == approved.acceptance_path {
            let bytes = origin
                .blob_at(base_sha, &path)
                .map_err(|error| environment_error("protected_manifest_read_failed", error))?;
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
            manifest.insert(
                candidate_path.clone(),
                ProtectedEntry {
                    sha256: crate::intent::intent_sha256(&approved.acceptance_bytes),
                    bytes: Some(approved.acceptance_bytes.clone()),
                },
            );
            continue;
        }
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
    mut environment: ChildEnvironment,
    protection: ProtectedWorkspace,
) -> Result<GateRequest, CoreError> {
    // Parallel rungs have fresh gate directories. Prepare them before adapters
    // write private assets, and before Linux selects the existing bind paths.
    prepare_gate_dir(run_dir)?;
    // The builder environment may belong to the parent run. Keep gate runtime
    // writes inside this gate's sandbox binds; explicit project env still wins.
    for (name, directory) in [
        ("TMPDIR", "tmp"),
        ("MISE_STATE_DIR", "mise-state"),
        ("MISE_CACHE_DIR", "mise-cache"),
    ] {
        if !options.environment.contains_key(name) {
            environment.insert(name.into(), run_dir.join(directory).into_os_string());
        }
    }
    let acceptance_relative = candidate_path(options, &approved.slug);
    let mut command = options.acceptance_run.clone();
    if options.adapter == "exunit" {
        let formatter = crate::gate::adapters::exunit::write_formatter(run_dir)
            .map_err(|error| environment_error("acceptance_runner_failed", error))?;
        let use_mise = pair_policy(
            project,
            options,
            candidate_workspace,
            base_workspace,
            run_dir,
        )
        .find_executable("mise", &environment)
        .is_some();
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

pub(super) fn prepare_gate_dir(run_dir: &Path) -> Result<(), CoreError> {
    crate::run::prepare_private_run_dir(run_dir)
        .map_err(|error| environment_error("acceptance_runner_failed", error))
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

#[cfg(test)]
mod tests {
    use super::{append_check_details, append_raw_tail, fix_failure_status};
    use crate::gate::{CheckResult, CheckStatus, FixResult};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn exunit_parallel_candidate_gate_prepares_fresh_rung_dirs_after_restore_and_setup_reuse() {
        use super::*;
        use crate::gate::{ProtectedEntry, ProtectedWorkspace};
        use crate::intent::{Intent, intent_sha256};
        use std::fs;

        struct AcceptanceRunner;
        impl ProcessPort for AcceptanceRunner {
            fn run(
                &self,
                request: ProcessRequest,
            ) -> Result<crate::run::ProcessResult, ProcessError> {
                assert_eq!(request.log_name, "acceptance");
                assert!(request.cwd.ends_with("candidate-R2"));
                assert_eq!(
                    fs::read(request.cwd.join("test/acceptance/greet_test.exs")).unwrap(),
                    b"approved test\n"
                );
                assert_eq!(
                    fs::read(request.run_dir.join("ledger_formatter.ex")).unwrap(),
                    crate::gate::adapters::exunit::formatter_source().as_bytes()
                );
                fs::write(
                    &request.env[std::ffi::OsStr::new("KOGEN_LEDGER_REPORT")],
                    "{\"tag\":\"greet/A1\",\"test\":\"invite\",\"status\":\"passed\"}\n",
                )
                .unwrap();
                Ok(crate::run::ProcessResult {
                    exit_status: Some(0),
                    timed_out: false,
                    unavailable: false,
                    output_tail: Vec::new(),
                    log_path: request.run_dir.join("logs/acceptance.log"),
                    duration_ms: 1,
                    sandbox: None,
                })
            }
        }
        let root = std::env::temp_dir().join(format!(
            "kogen-parallel-exunit-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let base = root.join("base");
        let candidate = root.join("candidate-R2");
        for path in [&base, &candidate] {
            fs::create_dir_all(path).unwrap();
            assert!(
                kogen_test_support::git_command()
                    .args(["init", "--quiet"])
                    .current_dir(path)
                    .status()
                    .unwrap()
                    .success()
            );
            // Cached setup outputs have already been copied to both clones.
            for output in ["deps", "_build"] {
                fs::create_dir_all(path.join(output)).unwrap();
                fs::write(path.join(output).join("cached"), "reused setup").unwrap();
            }
        }
        let project = ProjectResolution {
            checkout: root.join("checkout"),
            origin: root.join("origin"),
            base: "main".to_owned(),
            state_root: root.clone(),
            config: None,
        };
        let mut options = BuildOptions::load_with_machine(&project, &None).unwrap();
        options.setup_outputs = vec!["deps".to_owned(), "_build".to_owned()];
        let intent_bytes = b"---\ntitle: Invite member\nsize: hard\ndomains: [app]\n---\nInvite a member.\n\n## Acceptance\n- A1: invitation works\n\n## Verify\n- A1: test\n".to_vec();
        let approved = ApprovedBuild {
            slug: "greet".to_owned(),
            commit: String::new(),
            approval_sha256: String::new(),
            target_branch: "main".to_owned(),
            base_sha: String::new(),
            intent: Intent::parse("greet", &intent_bytes).unwrap(),
            intent_bytes,
            acceptance_path: ".kogen/acceptance/greet_test.exs".to_owned(),
            acceptance_bytes: b"approved test\n".to_vec(),
            approval: serde_json::json!({"check_baseline": []}),
            approval_time: 0,
        };
        let relative = "test/acceptance/greet_test.exs";
        fs::create_dir_all(candidate.join("test/acceptance")).unwrap();
        fs::write(candidate.join(relative), "builder changed protected test").unwrap();
        let protection = ProtectedWorkspace::new(
            BTreeMap::from([(
                relative.to_owned(),
                ProtectedEntry {
                    sha256: intent_sha256(&approved.acceptance_bytes),
                    bytes: Some(approved.acceptance_bytes.clone()),
                },
            )]),
            vec![approved.acceptance_path.clone()],
        )
        .unwrap();
        let restored = protection.restore_after_batch(&candidate).unwrap();
        assert_eq!(restored, vec![relative.to_owned()]);
        let run_dir = root.join("run/gate-R2");
        assert!(!run_dir.exists());
        let request = gate_request(
            &project,
            &approved,
            &options,
            &run_dir,
            &base,
            &candidate,
            ChildEnvironment::new(),
            protection,
        )
        .unwrap();
        assert!(run_dir.join("logs").is_dir());
        assert!(run_dir.join("reports").is_dir());
        assert_eq!(
            request.environment[std::ffi::OsStr::new("TMPDIR")],
            run_dir.join("tmp")
        );
        let report = crate::gate::run_gate(&AcceptanceRunner, &request).unwrap();
        assert!(report.acceptance.item_pass["A1"]);
        assert!(report.is_landable());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gate_feedback_details_cover_mutation_timeout_unavailable_fixes_and_raw_tail() {
        let mut lines = Vec::new();
        let mut total_findings = 0;
        let mut tool_counts = BTreeMap::new();
        let mut omitted_findings = BTreeMap::new();

        let mut mutating = check("unit", CheckStatus::Mutating);
        mutating.changed_paths = vec!["lib/generated.txt".to_owned()];
        append_check_details(
            &mut lines,
            &mutating,
            None,
            &mut total_findings,
            &mut tool_counts,
            &mut omitted_findings,
        );
        assert!(lines.join("\n").contains("lib/generated.txt"));

        append_check_details(
            &mut lines,
            &check("unit", CheckStatus::Timeout),
            None,
            &mut total_findings,
            &mut tool_counts,
            &mut omitted_findings,
        );
        assert!(lines.join("\n").contains("timed out after 3 s"));

        append_check_details(
            &mut lines,
            &check("unit", CheckStatus::Unavailable),
            Some(&check("unit", CheckStatus::Green)),
            &mut total_findings,
            &mut tool_counts,
            &mut omitted_findings,
        );
        assert!(
            lines
                .join("\n")
                .contains("sh is not available, but it ran on the base")
        );

        let failed_fix = FixResult {
            name: "fmt".to_owned(),
            exit_status: Some(1),
            timed_out: false,
            unavailable: false,
            log_path: PathBuf::from("fix.log"),
            duration_ms: 1,
        };
        assert_eq!(fix_failure_status(&failed_fix), "exit 1");

        let root = std::env::temp_dir().join(format!(
            "kogen-gate-feedback-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(root.join("logs")).expect("create feedback log directory");
        let log = root.join("logs/unit.log");
        std::fs::write(&log, format!("{}/diagnostic\n", root.display()))
            .expect("write feedback log");
        append_raw_tail(&mut lines, "unit", &log, &root);
        assert!(lines.last().is_some_and(|line| {
            line == "raw tail (first failed step unit):\n$TMPDIR/diagnostic"
        }));
        std::fs::remove_dir_all(root).expect("remove feedback log directory");
    }

    fn check(name: &str, status: CheckStatus) -> CheckResult {
        CheckResult {
            name: name.to_owned(),
            program: "sh".to_owned(),
            status,
            exit_status: Some(1),
            findings: Vec::new(),
            changed_paths: Vec::new(),
            log_path: PathBuf::from("check.log"),
            duration_ms: 1,
            timeout: std::time::Duration::from_secs(3),
            excused: false,
        }
    }
}

use super::*;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(clippy::too_many_arguments)]
pub(super) fn run_gate(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    run_dir: &Path,
    base: &Path,
    candidate: &Path,
    supervisor: &crate::run::ProcessSupervisor,
    integrity: &support::IntegritySnapshot,
    environment: &crate::run::ChildEnvironment,
    protection: crate::gate::ProtectedWorkspace,
) -> Result<crate::gate::GateReport, CoreError> {
    let request = support::gate_request(
        project,
        approved,
        options,
        run_dir,
        base,
        candidate,
        environment.clone(),
        protection,
    )?;
    let gate_runner = support::sandboxed_pair(
        project, options, candidate, base, run_dir, supervisor, integrity,
    );
    crate::gate::run_gate(&gate_runner, &request)
        .map_err(|error| environment_error("gate_failed", error.to_string()))
}

pub(super) fn install_approved(
    workspace: &Path,
    approved: &ApprovedBuild,
    options: &BuildOptions,
) -> Result<(), CoreError> {
    let intent = PathBuf::from(format!(".kogen/intents/{}/intent.md", approved.slug));
    let acceptance = support::candidate_path(options, &approved.slug);
    install_approved_bytes(
        workspace,
        &intent,
        &acceptance,
        &approved.intent_bytes,
        &approved.acceptance_bytes,
    )
    .map_err(|error| environment_error("workspace_write_failed", error.to_string()))
}

fn install_approved_bytes(
    workspace: &Path,
    intent: &Path,
    acceptance: &Path,
    intent_bytes: &[u8],
    acceptance_bytes: &[u8],
) -> std::io::Result<()> {
    crate::safe_fs::validate_write(workspace, intent)?;
    crate::safe_fs::validate_write(workspace, acceptance)?;
    crate::safe_fs::write_file(workspace, intent, intent_bytes)?;
    crate::safe_fs::write_file(workspace, acceptance, acceptance_bytes)
}

pub(super) fn record_started(
    provider: &mut BuildProvider<'_>,
    snapshot: &mut RunSnapshot,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
    account: &crate::provider::RunAccount,
    sandbox: &str,
) -> Result<(), CoreError> {
    let mut roles = serde_json::Map::new();
    roles.insert(
        "planner".to_owned(),
        json!({"model":options.planner_model,"effort":options.planner_effort}),
    );
    roles.insert(
        "builder".to_owned(),
        json!({"model":options.builder_model,"effort":options.builder_effort}),
    );
    roles.insert(
        "rung2".to_owned(),
        json!({"model":options.rung2_model,"effort":options.rung2_effort}),
    );
    roles.insert(
        "rung3".to_owned(),
        json!({"model":options.rung3_model,"effort":options.rung3_effort}),
    );
    provider.record_event(
        snapshot,
        &RunEvent::new("started", now_ms())
            .with("approval_commit", json!(approved.commit))
            .with(
                "approved_by",
                approved.approval.get("by").cloned().unwrap_or(Value::Null),
            )
            .with("base_sha", json!(base_sha))
            .with("recipe", json!(options.recipe))
            .with("max_rungs", json!(options.max_rungs))
            .with("roles", Value::Object(roles))
            .with("land", json!(options.land_policy))
            .with("budget_ms", json!(options.wall_ms))
            .with("credential_source", json!(account.credential_source))
            .with("credential_label", json!(account.label))
            .with("sandbox", json!(sandbox)),
    )
}

pub(super) fn progress_exclusions(
    approved: &ApprovedBuild,
    options: &BuildOptions,
) -> Vec<std::path::PathBuf> {
    options
        .setup_outputs
        .iter()
        .map(std::path::PathBuf::from)
        .chain([
            std::path::PathBuf::from(format!(".kogen/intents/{}/intent.md", approved.slug)),
            std::path::PathBuf::from(&approved.acceptance_path),
            support::candidate_path(options, &approved.slug),
        ])
        .collect()
}

pub(super) fn record_scope_warnings(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    workspace: &Path,
    base: &str,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
) -> Result<(), CoreError> {
    let repo = crate::git::GitRepo::new(workspace);
    let _ = repo.output(&["add", "-N", "--all"]);
    let paths = repo
        .output(&["diff", "--name-only", "-z", "--no-renames", base, "--"])
        .map_err(|error| environment_error("candidate_diff_failed", error.to_string()))?;
    let configured = project
        .config
        .as_ref()
        .and_then(|config| config.raw.get("domains"))
        .and_then(serde_yaml::Value::as_mapping);
    let mut allowed = Vec::new();
    if let Some(configured) = configured {
        for domain in &approved.intent.frontmatter.domains {
            if let Some(paths) = configured
                .get(serde_yaml::Value::String(domain.clone()))
                .and_then(serde_yaml::Value::as_sequence)
            {
                allowed.extend(paths.iter().filter_map(serde_yaml::Value::as_str));
            }
        }
    }
    let candidate = support::candidate_path(options, &approved.slug);
    let candidate = candidate.to_string_lossy();
    let intent_copy = format!(".kogen/intents/{}/intent.md", approved.slug);
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(String::from_utf8_lossy)
        .map(|path| path.into_owned())
    {
        if path == candidate
            || path == intent_copy
            || options
                .setup_outputs
                .iter()
                .any(|output| path == *output || path.starts_with(&format!("{output}/")))
        {
            continue;
        }
        if allowed
            .iter()
            .any(|prefix| path == **prefix || path.starts_with(&format!("{prefix}/")))
        {
            continue;
        }
        record(
            store,
            snapshot,
            RunEvent::new("scope_warning", now_ms())
                .with("path", json!(path))
                .with(
                    "declared_domains",
                    json!(approved.intent.frontmatter.domains),
                ),
        )?;
    }
    Ok(())
}

pub(super) fn probe_sandbox(
    runner: &dyn crate::run::ProcessPort,
    workspace: &Path,
    run_dir: &Path,
    environment: &crate::run::ChildEnvironment,
) -> Result<crate::run::SandboxObservation, CoreError> {
    let mut request = crate::run::ProcessRequest::new("/usr/bin/true", workspace, run_dir);
    request.env.clone_from(environment);
    request.timeout = std::time::Duration::from_secs(10);
    request.log_name = "sandbox-probe".to_owned();
    let result = runner
        .run(request)
        .map_err(|error| environment_error("sandbox_probe_failed", error.to_string()))?;
    result.sandbox.ok_or_else(|| {
        environment_error(
            "sandbox_probe_failed",
            "sandbox runner returned no observation",
        )
    })
}

pub(super) fn sandbox_warning(observation: &crate::run::SandboxObservation) -> String {
    match (observation.status, observation.warning_reason.as_deref()) {
        (crate::run::SandboxStatus::Unconfined, Some(reason)) => {
            format!("kogen: warning: sandbox unavailable: {reason}; building unconfined\n")
        }
        _ => String::new(),
    }
}

pub(super) fn stop_run(
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    error: &CoreError,
) -> Result<(), CoreError> {
    snapshot.status = "stopped".to_owned();
    let reason = format!("{}/{}", error.class.as_str(), error.reason);
    record(
        store,
        snapshot,
        RunEvent::new("finished", now_ms())
            .with("status", json!("stopped"))
            .with("reason", json!(reason)),
    )
}

pub(super) fn record(
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    event: RunEvent,
) -> Result<(), CoreError> {
    store
        .record(&event, snapshot)
        .map_err(|error| controller_error("run_journal_failed", error.to_string()))
}

pub(super) fn tracked_paths(project: &ProjectResolution, base: &str) -> Result<String, CoreError> {
    let paths = crate::git::GitRepo::new(&project.origin)
        .list_paths(base)
        .map_err(|error| environment_error("base_read_failed", error.to_string()))?;
    Ok(paths.join("\n"))
}

pub(super) fn is_ancestor(project: &ProjectResolution, approved: &str, tip: &str) -> bool {
    crate::git::GitRepo::new(&project.origin)
        .output(&["merge-base", "--is-ancestor", approved, tip])
        .is_ok()
}

pub(super) fn base_acceptance_text(
    approved: &ApprovedBuild,
    result: &crate::gate::CommandAcceptanceResult,
) -> String {
    approved
        .intent
        .verify
        .iter()
        .map(|item| {
            let passed = result.item_pass.get(&item.id) == Some(&true);
            let status = if passed { "passed" } else { "failed" };
            let tail = String::from_utf8_lossy(&result.process.output_tail);
            let output = tail.lines().take(5).collect::<Vec<_>>().join("\n");
            format!(
                "{} ({}): {status} — {}",
                item.id,
                item.kind().unwrap_or("test"),
                output
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn candidate_diff(
    workspace: &Path,
    base: &str,
    tree: &str,
) -> Result<Vec<u8>, CoreError> {
    crate::git::GitRepo::workspace(workspace)
        .output(&[
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            base,
            tree,
            "--",
        ])
        .map_err(|error| environment_error("candidate_diff_failed", error.to_string()))
}

pub(super) fn candidate_working_diff(workspace: &Path, base: &str) -> Result<Vec<u8>, CoreError> {
    let repo = crate::git::GitRepo::workspace(workspace);
    let _ = repo.output(&["add", "-N", "--all"]);
    repo.output(&["diff", "--binary", base, "--"])
        .map_err(|error| environment_error("candidate_diff_failed", error.to_string()))
}

pub(super) fn publish_candidate(
    workspace: &Path,
    origin: &Path,
    commit: &str,
    run_id: &str,
    rung: &str,
) -> Result<(), CoreError> {
    let reference = format!("refs/kogen/candidates/{run_id}/{rung}");
    let lease = format!("--force-with-lease={reference}:");
    let source = format!("{commit}:{reference}");
    let origin = origin.to_string_lossy();
    crate::git::GitRepo::workspace(workspace)
        .output(&[
            "-c",
            "core.hooksPath=/dev/null",
            "push",
            "--porcelain",
            "--no-recurse-submodules",
            "--no-verify",
            &lease,
            &origin,
            &source,
        ])
        .map_err(|error| environment_error("candidate_publish_failed", error.to_string()))?;
    Ok(())
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        environment_error(
            "candidate_diff_write_failed",
            "output path has no file name",
        )
    })?;
    crate::safe_fs::write_file(parent, Path::new(name), bytes)
        .map_err(|error| environment_error("candidate_diff_write_failed", error.to_string()))
}

pub(super) fn checks_json(report: &crate::gate::GateReport) -> Value {
    json!(report.checks.iter().map(|check| json!({
        "name":check.name,
        "exit_status":check.exit_status,
        "status":format!("{:?}", check.status).to_ascii_lowercase(),
        "excused":check.excused,
        "findings":check.findings.iter().map(|finding| json!({"path":finding.path,"rule":finding.rule,"symbol":finding.symbol,"message":finding.message})).collect::<Vec<_>>()
    })).collect::<Vec<_>>())
}

pub(super) fn acceptance_json(
    report: &crate::gate::GateReport,
    demoted: &std::collections::BTreeSet<String>,
) -> Value {
    json!(
        report
            .acceptance
            .item_pass
            .iter()
            .map(|(id, passed)| json!({
                "id":id,
                "status":acceptance_item_status(*passed),
                "demoted":report.demoted_items.contains(id) || demoted.contains(id)
            }))
            .collect::<Vec<_>>()
    )
}

fn acceptance_item_status(passed: bool) -> &'static str {
    if passed { "pass" } else { "fail" }
}

pub(super) fn commit_result_event(commit: &str, tree: &str) -> RunEvent {
    RunEvent::new("commit_result", now_ms())
        .with("commit", json!(commit))
        .with("tree", json!(tree))
}

pub(super) fn acceptance_only_red(report: &crate::gate::GateReport) -> bool {
    report.checks.iter().all(|check| !check.blocks_gate())
        && report
            .fix_results
            .iter()
            .all(crate::gate::FixResult::passed)
        && report.protection_findings.is_empty()
        && report.acceptance.item_pass.values().any(|passed| !passed)
}

#[cfg(test)]
fn check_feedback(
    check: &crate::gate::CheckResult,
    base: Option<&crate::gate::CheckResult>,
    total_findings: &mut usize,
    tool_findings: &mut std::collections::BTreeMap<String, usize>,
    omitted_findings: &mut std::collections::BTreeMap<String, usize>,
) -> Vec<String> {
    use crate::gate::CheckStatus;

    let mut details = Vec::new();
    match check.status {
        CheckStatus::Green => return details,
        CheckStatus::Mutating => {
            let paths = check.changed_paths.join(", ");
            if paths.is_empty() {
                details.push(format!("check {}: Mutating", check.name));
            } else {
                details.push(format!(
                    "check {}: Mutating; changed paths: {paths}",
                    check.name
                ));
            }
        }
        CheckStatus::Timeout => {
            details.push(format!(
                "check {}: timed out after {} s",
                check.name,
                check.timeout.as_secs_f64()
            ));
            details.extend(log_tail(&check.log_path));
        }
        CheckStatus::Unavailable if base.is_some_and(|base| base.status == CheckStatus::Green) => {
            details.push(format!(
                "{} is not available, but it ran on the base",
                check.program
            ));
        }
        CheckStatus::Unavailable => {
            details.push(format!("check {}: Unavailable", check.name));
        }
        CheckStatus::Red => {
            details.push(format!("check {}: Red", check.name));
            for finding in &check.findings {
                let tool = finding
                    .rule
                    .split('/')
                    .next()
                    .unwrap_or(&finding.rule)
                    .to_owned();
                let count = tool_findings.entry(tool.clone()).or_default();
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
                let detail = if finding.symbol.is_empty() {
                    format!(
                        "{}: error: [{}] {}",
                        position, finding.rule, finding.message
                    )
                } else {
                    format!(
                        "{}: error: [{}] {}: {}",
                        position, finding.rule, finding.symbol, finding.message
                    )
                };
                details.push(format!("{}:{detail}", finding.path));
            }
        }
    }
    details.push(format!("raw log: {}", check.log_path.display()));
    details
}

#[cfg(test)]
fn log_tail(path: &Path) -> Vec<String> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(20);
    lines[start..]
        .iter()
        .filter(|line| !line.is_empty())
        .map(|line| format!("  {line}"))
        .collect()
}

#[cfg(test)]
fn failed_fix_feedback(fixes: &[crate::gate::FixResult]) -> Vec<String> {
    fixes
        .iter()
        .filter(|fix| !fix.passed())
        .map(|fix| {
            let result = if fix.timed_out {
                "timed out".to_owned()
            } else if fix.unavailable {
                "unavailable".to_owned()
            } else {
                format!("exit {}", fix.exit_status.unwrap_or(-1))
            };
            format!("fix/{}: {result}", fix.name)
        })
        .collect()
}

fn failed_fix_count(fixes: &[crate::gate::FixResult]) -> usize {
    fixes.iter().filter(|fix| !fix.passed()).count()
}

pub(super) fn red_count(
    report: &crate::gate::GateReport,
    demoted: &std::collections::BTreeSet<String>,
) -> usize {
    let mut identities = std::collections::BTreeSet::new();
    let mut anonymous = 0;
    for check in report.checks.iter().filter(|check| check.blocks_gate()) {
        if check.findings.is_empty() {
            anonymous += 1;
        } else {
            identities.extend(
                check.findings.iter().map(|finding| {
                    format!("{}\0{}\0{}", finding.path, finding.rule, finding.symbol)
                }),
            );
        }
    }
    anonymous
        + identities.len()
        + failed_fix_count(&report.fix_results)
        + report
            .acceptance
            .item_pass
            .iter()
            .filter(|(id, passed)| !**passed && !demoted.contains(*id))
            .count()
}

pub(super) fn cleanup_path(path: &Path) {
    crate::git::forget_workspace(path);
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => record_cleanup_failure(path, &error),
    }
}

fn record_cleanup_failure(path: &Path, error: &std::io::Error) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        eprintln!("kogen: cleanup_failure {}: {error}", path.display());
        return;
    };
    let Some((run_id, _)) = name.split_once('-') else {
        eprintln!("kogen: cleanup_failure {}: {error}", path.display());
        return;
    };
    if run_id.len() != 32
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        eprintln!("kogen: cleanup_failure {}: {error}", path.display());
        return;
    }
    let Some(state_root) = path.parent() else {
        eprintln!("kogen: cleanup_failure {}: {error}", path.display());
        return;
    };
    let store = RunStore::new(state_root.join("runs").join(run_id));
    let record_result = store.read_snapshot().and_then(|snapshot| {
        store.prepare_cleanup(&snapshot)?;
        store.record(
            &RunEvent::new("cleanup_failure", now_ms())
                .with("operation", json!("remove workspace"))
                .with("path", json!(path.to_string_lossy()))
                .with("detail", json!(error.to_string())),
            &snapshot,
        )
    });
    if let Err(record_error) = record_result {
        eprintln!(
            "kogen: cleanup_failure {}: {error}; recording obligation failed: {record_error}",
            path.display()
        );
    }
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::snapshot_tree;
    use crate::git::GitRepo;
    use crate::git::landing::LandingRepository;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn acceptance_status_uses_pass_and_fail() {
        assert_eq!(acceptance_item_status(true), "pass");
        assert_eq!(acceptance_item_status(false), "fail");
    }

    #[test]
    fn commit_result_event_uses_the_journal_commit_and_tree_fields() {
        let event = commit_result_event("abc123", "def456");
        let value = serde_json::to_value(event).expect("serialize commit result");
        assert_eq!(value["event"], "commit_result");
        assert_eq!(value["commit"], "abc123");
        assert_eq!(value["tree"], "def456");
        assert!(value.get("candidate_commit").is_none());
    }

    #[test]
    fn failed_fixes_are_named_in_repair_feedback_and_counted() {
        let fixes = [crate::gate::FixResult {
            name: "fmt".to_owned(),
            exit_status: Some(1),
            timed_out: false,
            unavailable: false,
            log_path: PathBuf::from("fix.log"),
            duration_ms: 3,
        }];

        assert_eq!(failed_fix_feedback(&fixes), ["fix/fmt: exit 1"]);
        assert_eq!(failed_fix_count(&fixes), 1);
    }

    #[test]
    fn check_feedback_includes_mutation_paths_timeout_tail_and_unavailable_context() {
        let mut total = 0;
        let mut per_tool = std::collections::BTreeMap::new();
        let mut omitted = std::collections::BTreeMap::new();
        let mut mutating = check_result("unit", crate::gate::CheckStatus::Mutating);
        mutating.changed_paths = vec!["lib/generated.txt".to_owned()];
        let mutation = check_feedback(&mutating, None, &mut total, &mut per_tool, &mut omitted);
        assert!(mutation.join("\n").contains("lib/generated.txt"));

        let root = test_dir();
        let log = root.join("timeout.log");
        fs::write(&log, "last timeout diagnostic\n").expect("write timeout log");
        let mut timeout = check_result("unit", crate::gate::CheckStatus::Timeout);
        timeout.timeout = std::time::Duration::from_secs(3);
        timeout.log_path = log;
        let timeout_feedback =
            check_feedback(&timeout, None, &mut total, &mut per_tool, &mut omitted).join("\n");
        assert!(timeout_feedback.contains("timed out after 3 s"));
        assert!(timeout_feedback.contains("last timeout diagnostic"));

        let unavailable = check_result("unit", crate::gate::CheckStatus::Unavailable);
        let base = check_result("unit", crate::gate::CheckStatus::Green);
        let unavailable_feedback = check_feedback(
            &unavailable,
            Some(&base),
            &mut total,
            &mut per_tool,
            &mut omitted,
        )
        .join("\n");
        assert!(unavailable_feedback.contains("is not available, but it ran on the base"));
        cleanup_path(&root);
    }

    fn check_result(name: &str, status: crate::gate::CheckStatus) -> crate::gate::CheckResult {
        crate::gate::CheckResult {
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

    #[cfg(unix)]
    #[test]
    fn candidate_diff_output_rejects_symlink_redirection() {
        use std::os::unix::fs::symlink;

        let root = test_dir();
        let run_dir = root.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let outside = root.join("outside");
        fs::write(&outside, b"untouched\n").unwrap();
        symlink(&outside, run_dir.join("candidate.diff")).unwrap();
        assert!(write_private(&run_dir.join("candidate.diff"), b"controller bytes").is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"untouched\n");
        cleanup_path(&root);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_path_records_a_retryable_obligation_when_workspace_removal_fails() {
        use std::os::unix::fs::PermissionsExt;

        let state_root = test_dir();
        let run_id = "0123456789abcdef0123456789abcdef";
        let run_dir = state_root.join("runs").join(run_id);
        let workspace = state_root.join(format!("{run_id}-R1"));
        fs::create_dir_all(&workspace).expect("create workspace to clean");
        fs::write(workspace.join("file"), b"candidate").expect("write workspace file");
        let snapshot = RunSnapshot {
            schema: 2,
            run_id: run_id.to_owned(),
            slug: "alpha".to_owned(),
            approval_sha256: "approval-hash".to_owned(),
            approval_commit: "approval-commit".to_owned(),
            target_branch: "main".to_owned(),
            status: "running".to_owned(),
            landing: None,
            owner_pid: 1,
            owner_started_ms: 1,
            started_ms: 1,
            fields: Default::default(),
        };
        let store = RunStore::new(&run_dir);
        store.create(&snapshot).expect("create run state");
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o500))
            .expect("make workspace parent non-writable");

        cleanup_path(&workspace);

        assert!(workspace.exists());
        assert!(store.cleanup_pending(run_id).unwrap());
        assert!(
            store
                .read_events()
                .unwrap()
                .iter()
                .any(|event| event.event == "cleanup_failure")
        );
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))
            .expect("restore writable state root");
        cleanup_path(&workspace);
        store
            .clear_cleanup()
            .expect("clear completed fixture obligation");
        cleanup_path(&state_root);
    }

    #[test]
    fn candidate_diff_ignores_workspace_filter_and_exclude_controls() {
        let root = test_dir();
        let origin = root.join("origin.git");
        let seed = root.join("seed");
        let workspace = root.join("workspace");
        git(
            &root,
            &["init", "--bare", "--initial-branch=main", path(&origin)],
        );
        git(&root, &["init", "--initial-branch=main", path(&seed)]);
        kogen_test_support::set_identity(&seed, "Kogen Test", "test@kogen.invalid")
            .expect("set fixture identity");
        fs::write(seed.join("greet.txt"), b"Hello!\n").expect("write base file");
        git(&seed, &["add", "-A"]);
        git(&seed, &["commit", "-m", "base"]);
        git(&seed, &["remote", "add", "origin", path(&origin)]);
        git(&seed, &["push", "origin", "main"]);
        let base = GitRepo::new(&origin)
            .resolve_commit("refs/heads/main")
            .expect("resolve base");
        let repository = LandingRepository::clone_fresh(&origin, &workspace, &base)
            .expect("clone candidate workspace");

        fs::write(workspace.join("greet.txt"), b"Hello, Almir!\n")
            .expect("write changed tracked file");
        fs::create_dir_all(workspace.join("lib")).expect("create hidden file directory");
        fs::write(workspace.join("lib/hidden.txt"), b"secret\n")
            .expect("write excluded untracked file");
        let marker = root.join("filter-ran");
        let filter = format!("sh -c 'touch {}; cat'", path(&marker));
        git(
            &workspace,
            &["config", "filter.evil.clean", filter.as_str()],
        );
        git(&workspace, &["config", "filter.evil.smudge", "cat"]);
        fs::write(workspace.join(".git/info/attributes"), "* filter=evil\n")
            .expect("install workspace filter attribute");
        fs::write(workspace.join(".git/info/exclude"), "lib/hidden.txt\n")
            .expect("install workspace exclude");

        repository
            .reset_workspace_git_settings()
            .expect("restore trusted workspace Git settings");
        let tree = snapshot_tree(&workspace).expect("snapshot every workspace file");
        let diff = candidate_diff(&workspace, &base, &tree).expect("render candidate diff");
        let diff = String::from_utf8_lossy(&diff);
        assert!(diff.contains("lib/hidden.txt"), "diff was {diff}");
        assert!(diff.contains("secret"), "diff was {diff}");
        assert!(!marker.exists(), "workspace clean filter ran");
        assert!(!workspace.join(".git/info/attributes").exists());
        assert!(!workspace.join(".git/info/exclude").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn approved_install_rejects_symlink_components_before_any_write() {
        use std::os::unix::fs::symlink;

        let root = test_dir();
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&outside).expect("create outside directory");
        symlink(&outside, workspace.join("test")).expect("install setup-created symlink");

        let intent = PathBuf::from(".kogen/intents/greet/intent.md");
        let acceptance = PathBuf::from("test/acceptance/greet_test.exs");
        let result = install_approved_bytes(&workspace, &intent, &acceptance, b"intent", b"test");
        assert!(result.is_err());
        assert!(!workspace.join(&intent).exists());
        assert!(!outside.join("acceptance/greet_test.exs").exists());

        fs::remove_file(workspace.join("test")).expect("remove parent symlink");
        fs::create_dir_all(workspace.join(".kogen/intents/greet"))
            .expect("create approved Intent parent");
        symlink(
            outside.join("intent.md"),
            workspace.join(".kogen/intents/greet/intent.md"),
        )
        .expect("install repository-provided final symlink");
        let result = install_approved_bytes(
            &workspace,
            &intent,
            &PathBuf::from("test.greet"),
            b"new intent",
            b"test",
        );
        assert!(result.is_err());
        assert!(!outside.join("intent.md").exists());
        let _ = fs::remove_dir_all(root);
    }

    fn git(directory: &Path, args: &[&str]) {
        let output = kogen_test_support::git_command()
            .args(args)
            .current_dir(directory)
            .output()
            .expect("run fixture Git command");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn path(path: &Path) -> &str {
        path.to_str().expect("temporary path is UTF-8")
    }

    fn test_dir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "kogen-candidate-diff-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create candidate diff fixture root");
        path
    }
}

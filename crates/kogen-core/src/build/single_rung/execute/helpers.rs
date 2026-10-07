use super::*;
use std::fs;
use std::io::Write as _;
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
    let intent = workspace.join(format!(".kogen/intents/{}/intent.md", approved.slug));
    let acceptance = workspace.join(support::candidate_path(options, &approved.slug));
    for path in [&intent, &acceptance] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| environment_error("workspace_write_failed", error.to_string()))?;
        }
    }
    fs::write(intent, &approved.intent_bytes)
        .and_then(|()| fs::write(acceptance, &approved.acceptance_bytes))
        .map_err(|error| environment_error("workspace_write_failed", error.to_string()))
}

pub(super) fn record_started(
    provider: &mut BuildProvider<'_>,
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
    provider.record_event(
        &RunEvent::new("started", now_ms())
            .with("approval_commit", json!(approved.commit))
            .with(
                "approved_by",
                approved.approval.get("by").cloned().unwrap_or(Value::Null),
            )
            .with("base_sha", json!(base_sha))
            .with("recipe", json!(options.recipe))
            .with("max_rungs", json!(1))
            .with("roles", Value::Object(roles))
            .with("land", json!("green-or-advisory"))
            .with("budget_ms", json!(options.wall_ms))
            .with("credential_source", json!(account.credential_source))
            .with("credential_label", json!(account.label))
            .with("sandbox", json!(sandbox)),
    )
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

pub(super) fn publish_candidate(
    workspace: &Path,
    origin: &Path,
    commit: &str,
    run_id: &str,
) -> Result<(), CoreError> {
    let reference = format!("refs/kogen/candidates/{run_id}/R1");
    let lease = format!("--force-with-lease={reference}:");
    let source = format!("{commit}:{reference}");
    let origin = origin.to_string_lossy();
    crate::git::GitRepo::workspace(workspace)
        .output(&[
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
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| environment_error("candidate_diff_write_failed", error.to_string()))?;
    file.write_all(bytes)
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

pub(super) fn acceptance_json(report: &crate::gate::GateReport) -> Value {
    json!(
        report
            .acceptance
            .item_pass
            .iter()
            .map(|(id, passed)| json!({
                "id":id,
                "status":if *passed {"passed"} else {"failed"},
                "demoted":report.demoted_items.contains(id)
            }))
            .collect::<Vec<_>>()
    )
}

pub(super) fn cleanup_path(path: &Path) {
    let _ = fs::remove_dir_all(path);
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

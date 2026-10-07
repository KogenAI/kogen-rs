use super::*;
use crate::run::{ChildEnvironment, ProcessSupervisor};
use std::sync::atomic::{AtomicUsize, Ordering};

struct FixedRunner(AtomicUsize);

impl ProcessPort for FixedRunner {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ProcessResult {
            exit_status: Some(0),
            timed_out: false,
            unavailable: false,
            output_tail: Vec::new(),
            log_path: request.run_dir.join("logs/check.log"),
            duration_ms: 1,
            sandbox: None,
        })
    }
}

struct ChangingIntegrity(AtomicUsize);

impl SandboxIntegrityPort for ChangingIntegrity {
    fn snapshot(&self) -> Result<String, String> {
        Ok(if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            "before".to_owned()
        } else {
            "after".to_owned()
        })
    }
}

struct StableIntegrity;

impl SandboxIntegrityPort for StableIntegrity {
    fn snapshot(&self) -> Result<String, String> {
        Ok("same".to_owned())
    }
}

#[test]
fn forced_unavailable_records_warning_and_integrity_check() {
    let root = test_dir("unavailable");
    let runner = FixedRunner(AtomicUsize::new(0));
    let integrity = ChangingIntegrity(AtomicUsize::new(0));
    let policy = SandboxPolicy::new(true, &root, &root)
        .with_unavailable_reason("test seam")
        .with_integrity_check(true);
    let sandboxed = SandboxedProcessPort::new(&runner, policy, Some(&integrity));
    let result = sandboxed
        .run(ProcessRequest::new("check", &root, &root))
        .expect_err("mutated tree is rejected");
    assert!(matches!(result, ProcessError::SandboxIntegrityChanged));
    assert_eq!(runner.0.load(Ordering::SeqCst), 1);
    assert_eq!(integrity.0.load(Ordering::SeqCst), 2);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn sandbox_exec_missing_target_is_normalized_to_unavailable_127() {
    assert!(sandbox_exec_target_missing(
        b"sandbox-exec: execvp() of 'missing-check' failed: No such file or directory\n"
    ));
    assert!(!sandbox_exec_target_missing(
        b"check: No such file or directory\n"
    ));
}

#[test]
fn unconfined_result_exposes_warning_reason() {
    let root = test_dir("warning");
    let runner = FixedRunner(AtomicUsize::new(0));
    let integrity = StableIntegrity;
    let policy = SandboxPolicy::new(true, &root, &root)
        .with_unavailable_reason("test seam")
        .with_integrity_check(true);
    let sandboxed = SandboxedProcessPort::new(&runner, policy, Some(&integrity));
    let result = sandboxed
        .run(ProcessRequest::new("check", &root, &root))
        .expect("process result");
    let observation = result.sandbox.expect("sandbox result");
    assert_eq!(observation.status, SandboxStatus::Unconfined);
    assert_eq!(observation.warning_reason.as_deref(), Some("test seam"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unconfined_build_requires_integrity_port_before_spawning() {
    let root = test_dir("integrity-required");
    let runner = FixedRunner(AtomicUsize::new(0));
    let policy = SandboxPolicy::new(true, &root, &root)
        .with_unavailable_reason("test seam")
        .with_integrity_check(true);
    let sandboxed = SandboxedProcessPort::new(&runner, policy, None);
    let result = sandboxed.run(ProcessRequest::new("check", &root, &root));
    assert!(matches!(
        result,
        Err(ProcessError::SandboxIntegrityRequired)
    ));
    assert_eq!(runner.0.load(Ordering::SeqCst), 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn sandbox_off_is_reported_and_does_not_require_build_integrity_seam() {
    let root = test_dir("off");
    let runner = FixedRunner(AtomicUsize::new(0));
    let policy = SandboxPolicy::new(false, &root, &root);
    let sandboxed = SandboxedProcessPort::new(&runner, policy, None);
    let result = sandboxed
        .run(ProcessRequest::new("check", &root, &root))
        .expect("process result");
    assert_eq!(result.sandbox.unwrap().status, SandboxStatus::Off);
    assert_eq!(runner.0.load(Ordering::SeqCst), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn build_policy_reads_only_documented_host_seams() {
    let root = test_dir("policy");
    let home = root.join("home");
    fs::create_dir_all(home.join(".kogen")).expect("create home");
    let credential = home.join(".kogen/credentials.json");
    fs::write(&credential, "secret").expect("write credential sentinel");
    let host = EnvironmentMap::from([
        ("HOME".into(), home.as_os_str().to_owned()),
        ("KOGEN_AUTH_PATH".into(), "/tmp/auth.json".into()),
        ("KOGEN_SANDBOX".into(), "unavailable".into()),
        ("KOGEN_SANDBOXED".into(), "1".into()),
        ("GOMODCACHE".into(), "/tmp/go-cache".into()),
    ]);
    let policy = SandboxPolicy::for_build(true, root.join("workspace"), root.join("run"), &host);
    assert!(policy.already_sandboxed);
    assert_eq!(
        policy.forced_unavailable.as_deref(),
        Some("forced by KOGEN_SANDBOX=unavailable")
    );
    assert!(policy.verify_integrity);
    assert!(policy.protected_paths.contains(&credential));
    assert!(
        policy
            .protected_paths
            .contains(&PathBuf::from("/tmp/auth.json"))
    );
    assert!(
        policy
            .writable_paths
            .contains(&PathBuf::from("/tmp/go-cache"))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn build_policy_grants_only_scratch_and_report_write_access_in_the_run_tree() {
    let root = test_dir("run-state-policy");
    let run_dir = root.join("run");
    let policy = SandboxPolicy::for_build(
        true,
        root.join("workspace"),
        &run_dir,
        &EnvironmentMap::new(),
    );
    assert!(!policy.writable_paths.contains(&run_dir));
    for name in ["logs", "tmp", "reports", "mise-state", "mise-cache"] {
        assert!(policy.writable_paths.contains(&run_dir.join(name)));
    }
    let _ = fs::remove_dir_all(root);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_profile_allows_workspace_writes_and_denies_secret_reads() {
    let root = test_dir("macos-profile");
    let workspace = root.join("workspace");
    let run_dir = root.join("run");
    let home = root.join("home");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(home.join(".codex")).expect("create home secret directory");
    fs::create_dir_all(&run_dir).expect("create run directory");
    let secret = home.join(".codex/auth.json");
    fs::write(&secret, "credential").expect("write secret");
    let allowed = workspace.join("allowed.txt");
    let outside = root.join("outside.txt");
    let command = format!(
        "printf allowed > '{}' && if /bin/cat '{}' >/dev/null 2>&1; then exit 43; fi && if printf denied > '{}'; then exit 42; fi",
        allowed.display(),
        secret.display(),
        outside.display()
    );
    let mut request = ProcessRequest::new("/bin/sh", &workspace, &run_dir);
    request.args = vec!["-c".into(), command.into()];
    request.env = ChildEnvironment::from([
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), "/bin:/usr/bin".into()),
    ]);
    let supervisor = ProcessSupervisor;
    let mut policy = SandboxPolicy::new(true, &workspace, &run_dir);
    policy.protect_read(secret);
    let sandboxed = SandboxedProcessPort::new(&supervisor, policy, None);
    let result = sandboxed.run(request).expect("sandboxed process");
    assert_eq!(
        result.exit_status,
        Some(0),
        "sandbox output: {}",
        String::from_utf8_lossy(&result.output_tail)
    );
    assert_eq!(fs::read_to_string(allowed).unwrap(), "allowed");
    assert!(!outside.exists());
    assert_eq!(result.sandbox.unwrap().status, SandboxStatus::Confined);
    let _ = fs::remove_dir_all(root);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_build_policy_denies_checkout_and_origin_under_tmp() {
    let root =
        PathBuf::from("/tmp").join(format!("kogen-sandbox-deny-write-{}", std::process::id()));
    let workspace = root.join("workspace");
    let run_dir = root.join("run");
    let checkout = root.join("checkout");
    let origin = root.join("origin");
    for path in [&workspace, &run_dir, &checkout, &origin] {
        fs::create_dir_all(path).expect("create sandbox fixture directory");
    }
    let allowed = workspace.join("allowed.txt");
    let checkout_write = checkout.join("escaped.txt");
    let origin_write = origin.join("escaped.txt");
    let command = format!(
        "printf allowed > '{}' && if printf denied > '{}'; then exit 42; fi && if printf denied > '{}'; then exit 43; fi",
        allowed.display(),
        checkout_write.display(),
        origin_write.display()
    );
    let mut request = ProcessRequest::new("/bin/sh", &workspace, &run_dir);
    request.args = vec!["-c".into(), command.into()];
    request.env = ChildEnvironment::from([
        ("HOME".into(), root.join("home").into_os_string()),
        ("PATH".into(), "/bin:/usr/bin".into()),
    ]);
    let supervisor = ProcessSupervisor;
    let mut policy = SandboxPolicy::new(true, &workspace, &run_dir);
    policy.deny_write(&checkout);
    policy.deny_write(&origin);
    let sandboxed = SandboxedProcessPort::new(&supervisor, policy, None);
    let result = sandboxed.run(request).expect("sandboxed process");
    assert_eq!(
        result.exit_status,
        Some(0),
        "sandbox output: {}",
        String::from_utf8_lossy(&result.output_tail)
    );
    assert_eq!(fs::read_to_string(allowed).unwrap(), "allowed");
    assert!(!checkout_write.exists());
    assert!(!origin_write.exists());
    assert_eq!(result.sandbox.unwrap().status, SandboxStatus::Confined);
    let _ = fs::remove_dir_all(root);
}

fn test_dir(label: &str) -> PathBuf {
    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "kogen-sandbox-{label}-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create temp directory");
    path
}

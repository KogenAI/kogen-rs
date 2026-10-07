use super::reconcile;
use crate::git::GitRepo;
use crate::project::ProjectResolution;
use crate::run::{LandingRecord, RunEvent, RunSnapshot, RunStore};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const RUN_ID: &str = "0123456789abcdef0123456789abcdef";
const MISSING_BRANCH_RUN_ID: &str = "fedcba9876543210fedcba9876543210";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "kogen-recovery-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create isolated recovery test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn crash_after_base_cas_before_incoming_cleanup_reconciles_as_landed() {
    let temp = TestDirectory::new();
    let origin = temp.0.join("origin.git");
    let seed = temp.0.join("seed");
    git(
        &temp.0,
        &["init", "--bare", "--initial-branch=main", path(&origin)],
    );
    git(&temp.0, &["init", "--initial-branch=main", path(&seed)]);
    kogen_test_support::set_identity(&seed, "Recovery Trace", "recovery@example.test")
        .expect("configure recovery fixture identity");
    fs::write(seed.join("README"), b"base\n").expect("write base fixture");
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-m", "base"]);
    git(&seed, &["remote", "add", "origin", path(&origin)]);
    git(&seed, &["push", "origin", "main"]);

    fs::write(seed.join("candidate"), b"landed\n").expect("write candidate fixture");
    git(&seed, &["add", "candidate"]);
    git(
        &seed,
        &["commit", "-m", "land alpha", "-m", "Kogen-Intent: alpha"],
    );
    let candidate = git_text(&seed, &["rev-parse", "HEAD"]);
    git(&seed, &["push", "origin", "main"]);

    fs::create_dir_all(seed.join(".kogen")).expect("create claim fixture directory");
    fs::write(seed.join(".kogen/claim"), format!("{RUN_ID}\n")).expect("write claim fixture");
    git(&seed, &["add", ".kogen/claim"]);
    git(&seed, &["commit", "-m", "claim owner"]);
    let claim = git_text(&seed, &["rev-parse", "HEAD"]);
    git(
        &seed,
        &["push", "origin", &format!("{claim}:refs/kogen/claim")],
    );
    git(
        &seed,
        &[
            "push",
            "origin",
            &format!("{candidate}:refs/kogen/incoming/{RUN_ID}"),
        ],
    );

    let state_root = temp.0.join("state");
    let run_dir = state_root.join("runs").join(RUN_ID);
    let snapshot = RunSnapshot {
        schema: 2,
        run_id: RUN_ID.to_owned(),
        slug: "alpha".to_owned(),
        approval_sha256: "approval-hash".to_owned(),
        approval_commit: "approval-commit".to_owned(),
        target_branch: "main".to_owned(),
        status: "running".to_owned(),
        landing: Some(LandingRecord {
            approval_commit: "approval-commit".to_owned(),
            run_id: RUN_ID.to_owned(),
            expected_parent: git_text(&seed, &["rev-parse", "HEAD^"]),
            final_tree: git_text(&seed, &["rev-parse", "HEAD^{tree}"]),
            candidate_commit: candidate.clone(),
            fields: BTreeMap::new(),
        }),
        owner_pid: 0,
        owner_started_ms: 0,
        started_ms: 1,
        fields: BTreeMap::new(),
    };
    let store = RunStore::new(&run_dir);
    store.create(&snapshot).expect("write running snapshot");
    store
        .record(
            &RunEvent::new("base_cas", 2).with("candidate_commit", json!(candidate)),
            &snapshot,
        )
        .expect("record base CAS before simulated crash");

    // Trace state: base reaches candidate, but incoming and owner claim still exist.
    let project = ProjectResolution {
        checkout: seed,
        origin: origin.clone(),
        base: "refs/heads/main".to_owned(),
        state_root: state_root.clone(),
        config: None,
    };
    assert_eq!(
        GitRepo::new(&origin)
            .ref_target("refs/kogen/incoming/0123456789abcdef0123456789abcdef")
            .unwrap()
            .as_deref(),
        Some(candidate.as_str())
    );
    assert_eq!(
        GitRepo::new(&origin)
            .ref_target("refs/kogen/claim")
            .unwrap()
            .as_deref(),
        Some(claim.as_str())
    );

    let report = reconcile(&project).expect("reconcile post-CAS crash");
    assert_eq!(report.reconciled.len(), 1);
    assert_eq!(report.reconciled[0].status, "landed");
    assert_eq!(report.reconciled[0].reason, "reconciled");
    assert_eq!(store.read_snapshot().unwrap().status, "landed");
    assert_eq!(
        store.read_events().unwrap().last().unwrap().event,
        "reconciled"
    );
    let repo = GitRepo::new(&origin);
    assert_eq!(repo.ref_target("refs/kogen/claim").unwrap(), None);
    assert_eq!(
        repo.ref_target("refs/kogen/incoming/0123456789abcdef0123456789abcdef")
            .unwrap(),
        None
    );
}

#[test]
fn recovery_uses_the_snapshot_target_branch_when_the_configured_base_changes() {
    let temp = TestDirectory::new();
    let origin = temp.0.join("origin.git");
    let seed = temp.0.join("seed");
    git(
        &temp.0,
        &["init", "--bare", "--initial-branch=main", path(&origin)],
    );
    git(&temp.0, &["init", "--initial-branch=main", path(&seed)]);
    kogen_test_support::set_identity(&seed, "Recovery Trace", "recovery@example.test")
        .expect("configure recovery fixture identity");
    fs::write(seed.join("README"), b"base\n").expect("write base fixture");
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-m", "base"]);
    git(&seed, &["remote", "add", "origin", path(&origin)]);
    git(&seed, &["push", "origin", "main"]);
    let main = git_text(&seed, &["rev-parse", "HEAD"]);

    git(&seed, &["switch", "-c", "feature"]);
    fs::write(seed.join("candidate"), b"landed\n").expect("write candidate fixture");
    git(&seed, &["add", "candidate"]);
    git(
        &seed,
        &["commit", "-m", "land alpha", "-m", "Kogen-Intent: alpha"],
    );
    let candidate = git_text(&seed, &["rev-parse", "HEAD"]);
    git(&seed, &["push", "origin", "feature"]);

    fs::create_dir_all(seed.join(".kogen")).expect("create claim fixture directory");
    fs::write(seed.join(".kogen/claim"), format!("{RUN_ID}\n")).expect("write claim fixture");
    git(&seed, &["add", ".kogen/claim"]);
    git(&seed, &["commit", "-m", "claim owner"]);
    let claim = git_text(&seed, &["rev-parse", "HEAD"]);
    git(
        &seed,
        &["push", "origin", &format!("{claim}:refs/kogen/claim")],
    );
    git(
        &seed,
        &[
            "push",
            "origin",
            &format!("{candidate}:refs/kogen/incoming/{RUN_ID}"),
        ],
    );

    let state_root = temp.0.join("state");
    let run_dir = state_root.join("runs").join(RUN_ID);
    let snapshot = RunSnapshot {
        schema: 2,
        run_id: RUN_ID.to_owned(),
        slug: "alpha".to_owned(),
        approval_sha256: "approval-hash".to_owned(),
        approval_commit: "approval-commit".to_owned(),
        target_branch: "feature".to_owned(),
        status: "running".to_owned(),
        landing: Some(LandingRecord {
            approval_commit: "approval-commit".to_owned(),
            run_id: RUN_ID.to_owned(),
            expected_parent: main,
            final_tree: git_text(&seed, &["rev-parse", "HEAD^{tree}"]),
            candidate_commit: candidate.clone(),
            fields: BTreeMap::new(),
        }),
        owner_pid: 0,
        owner_started_ms: 0,
        started_ms: 1,
        fields: BTreeMap::new(),
    };
    let store = RunStore::new(&run_dir);
    store.create(&snapshot).expect("write running snapshot");
    store
        .record(
            &RunEvent::new("base_cas", 2).with("candidate_commit", json!(candidate)),
            &snapshot,
        )
        .expect("record base CAS before simulated crash");

    let missing_branch_snapshot = RunSnapshot {
        run_id: MISSING_BRANCH_RUN_ID.to_owned(),
        target_branch: "deleted".to_owned(),
        ..snapshot.clone()
    };
    let missing_branch_store = RunStore::new(state_root.join("runs").join(MISSING_BRANCH_RUN_ID));
    missing_branch_store
        .create(&missing_branch_snapshot)
        .expect("create run whose recorded branch was deleted");
    missing_branch_store
        .record(
            &RunEvent::new("base_cas", 3).with("candidate_commit", json!(candidate)),
            &missing_branch_snapshot,
        )
        .expect("record missing-branch crash state");

    let project = ProjectResolution {
        checkout: seed,
        origin: origin.clone(),
        base: "main".to_owned(),
        state_root,
        config: None,
    };
    assert_ne!(
        GitRepo::new(&origin)
            .resolve_commit("refs/heads/main")
            .unwrap(),
        candidate,
        "fixture candidate must not be reachable from the newly configured base"
    );

    let report = reconcile(&project).expect("reconcile post-CAS crash");
    assert_eq!(report.reconciled.len(), 2);
    let feature_run = report
        .reconciled
        .iter()
        .find(|run| run.run_id == RUN_ID)
        .expect("feature run recovered");
    assert_eq!(feature_run.status, "landed");
    assert_eq!(feature_run.reason, "reconciled");
    let missing_branch_run = report
        .reconciled
        .iter()
        .find(|run| run.run_id == MISSING_BRANCH_RUN_ID)
        .expect("missing target policy recovered");
    assert_eq!(missing_branch_run.status, "failed");
    assert_eq!(missing_branch_run.reason, "crashed");
    assert_eq!(store.read_snapshot().unwrap().status, "landed");
    assert_eq!(
        missing_branch_store.read_snapshot().unwrap().status,
        "failed"
    );
    let repo = GitRepo::new(&origin);
    assert_eq!(repo.ref_target("refs/kogen/claim").unwrap(), None);
    assert_eq!(
        repo.ref_target(&format!("refs/kogen/incoming/{RUN_ID}"))
            .unwrap(),
        None
    );
}

#[cfg(unix)]
#[test]
fn terminal_cleanup_obligation_retries_after_crash_and_deletion_failure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TestDirectory::new();
    let origin = temp.0.join("origin.git");
    git(
        &temp.0,
        &["init", "--bare", "--initial-branch=main", path(&origin)],
    );
    let state_root = temp.0.join("state");
    let run_dir = state_root.join("runs").join(RUN_ID);
    let workspace = state_root.join(format!("{RUN_ID}-R1"));
    fs::create_dir_all(&workspace).expect("create leftover workspace");
    fs::write(workspace.join("candidate"), b"left behind").expect("write leftover file");
    let mut snapshot = RunSnapshot {
        schema: 2,
        run_id: RUN_ID.to_owned(),
        slug: "alpha".to_owned(),
        approval_sha256: "approval-hash".to_owned(),
        approval_commit: "approval-commit".to_owned(),
        target_branch: "main".to_owned(),
        status: "failed".to_owned(),
        landing: None,
        owner_pid: 0,
        owner_started_ms: 0,
        started_ms: 1,
        fields: BTreeMap::new(),
    };
    let store = RunStore::new(&run_dir);
    snapshot.status = "running".to_owned();
    store.create(&snapshot).expect("create run state");
    snapshot.status = "failed".to_owned();
    store
        .record(
            &RunEvent::new("finished", 2)
                .with("status", json!("failed"))
                .with("reason", json!("crashed")),
            &snapshot,
        )
        .expect("publish terminal snapshot and cleanup obligation before simulated crash");
    assert!(store.cleanup_pending(RUN_ID).unwrap());

    // Make the first cleanup attempt fail at the state-root boundary. The
    // run journal remains writable so the failure can be durably recorded.
    fs::set_permissions(&state_root, fs::Permissions::from_mode(0o500))
        .expect("make workspace parent non-writable");
    let project = ProjectResolution {
        checkout: temp.0.clone(),
        origin,
        base: "main".to_owned(),
        state_root: state_root.clone(),
        config: None,
    };
    reconcile(&project).expect("cleanup failure leaves run available for another recovery pass");
    assert!(
        workspace.exists(),
        "failed deletion leaves workspace for retry"
    );
    assert!(store.cleanup_pending(RUN_ID).unwrap());
    assert!(
        store
            .read_events()
            .unwrap()
            .iter()
            .any(|event| event.event == "cleanup_failure")
    );

    fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))
        .expect("restore writable state root");
    reconcile(&project).expect("retry terminal cleanup");
    assert!(!workspace.exists());
    assert!(!store.cleanup_pending(RUN_ID).unwrap());
}

fn git(directory: &Path, args: &[&str]) {
    let output = kogen_test_support::git_command()
        .args(args)
        .current_dir(directory)
        .output()
        .expect("run isolated Git fixture command");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_text(directory: &Path, args: &[&str]) -> String {
    let output = kogen_test_support::git_command()
        .args(args)
        .current_dir(directory)
        .output()
        .expect("run isolated Git fixture query");
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn path(value: &Path) -> &str {
    value.to_str().expect("temporary paths are valid UTF-8")
}

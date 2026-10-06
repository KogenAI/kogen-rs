use super::reconcile;
use crate::git::GitRepo;
use crate::project::ProjectResolution;
use crate::run::{LandingRecord, RunEvent, RunSnapshot, RunStore};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const RUN_ID: &str = "0123456789abcdef0123456789abcdef";
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
    git(&seed, &["config", "user.name", "Recovery Trace"]);
    git(&seed, &["config", "user.email", "recovery@example.test"]);
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

fn git(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
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
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run isolated Git fixture query");
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn path(value: &Path) -> &str {
    value.to_str().expect("temporary paths are valid UTF-8")
}

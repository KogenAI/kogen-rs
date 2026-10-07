use super::{witness_auditor_instructions, workspace_changed};
use crate::gate::commit_tree_id;
use crate::git::GitRepo;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

#[test]
fn finish_guard_ignores_acceptance_copy_and_detects_builder_commit() {
    let workspace = std::env::temp_dir().join(format!(
        "kogen-builder-progress-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(workspace.join("lib")).expect("create source directory");
    let repo = GitRepo::new(&workspace);
    repo.output(&["init", "--quiet"])
        .expect("initialize repository");
    kogen_test_support::set_identity(&workspace, "Progress Test", "progress@example.invalid")
        .expect("configure test identity");
    fs::write(workspace.join("lib/greet.txt"), b"Hello, world!\n").expect("write base source");
    repo.output(&["add", "-A"]).expect("stage base source");
    repo.output(&["commit", "-m", "base"])
        .expect("commit base source");
    let base = repo.resolve_commit("HEAD").expect("resolve base commit");
    let baseline = commit_tree_id(&workspace, &base).expect("resolve base tree");
    let excluded = [PathBuf::from("test/acceptance/greet_test.rs")];

    fs::create_dir_all(workspace.join("test/acceptance"))
        .expect("create generated acceptance directory");
    fs::write(
        workspace.join("test/acceptance/greet_test.rs"),
        b"#[test] fn acceptance() {}\n",
    )
    .expect("write generated acceptance copy");
    assert!(
        !workspace_changed(&workspace, &baseline, &excluded)
            .expect("snapshot unchanged source tree")
    );

    fs::write(workspace.join("lib/greet.txt"), b"Hello, Almir!\n").expect("write implementation");
    repo.output(&["add", "lib/greet.txt"])
        .expect("stage implementation");
    repo.output(&["commit", "-m", "builder commit"])
        .expect("commit implementation in builder session");
    assert!(
        workspace_changed(&workspace, &baseline, &excluded)
            .expect("compare committed implementation to build base")
    );

    fs::remove_dir_all(workspace).expect("remove temporary repository");
}

#[test]
fn witness_auditor_prompt_uses_the_witness_verdict_contract() {
    let prompt = witness_auditor_instructions();
    assert!(prompt.contains(crate::run::orchestration::BUILD_AUDITOR_MARKER));
    assert!(prompt.contains("TEST-WRONG|WITNESS-WRONG|UNDECIDED"));
    assert!(prompt.contains("\"citation\""));
}

#[test]
fn approved_intent_test_and_setup_outputs_are_not_implementation_changes() {
    let root = temporary_repository();
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(root.join("lib/greet.txt"), "Hello!\n").unwrap();
    git(&root, &["add", "lib/greet.txt"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Kogen Test",
            "-c",
            "user.email=test@kogen.invalid",
            "commit",
            "-qm",
            "base",
        ],
    );
    let base = crate::gate::commit_tree_id(&root, "HEAD").unwrap();

    fs::create_dir_all(root.join(".kogen/intents/greet")).unwrap();
    fs::create_dir_all(root.join(".kogen/acceptance")).unwrap();
    fs::create_dir_all(root.join("test/acceptance")).unwrap();
    fs::write(
        root.join(".kogen/intents/greet/intent.md"),
        "approved intent\n",
    )
    .unwrap();
    fs::write(root.join(".kogen/acceptance/greet.t.sh"), "approved test\n").unwrap();
    fs::write(root.join("test/acceptance/greet.t.sh"), "installed test\n").unwrap();
    fs::create_dir_all(root.join("build")).unwrap();
    fs::write(root.join("build/ready"), "setup output\n").unwrap();

    let excluded = [
        PathBuf::from(".kogen/intents/greet/intent.md"),
        PathBuf::from(".kogen/acceptance/greet.t.sh"),
        PathBuf::from("test/acceptance/greet.t.sh"),
        PathBuf::from("build"),
    ];
    assert!(!workspace_changed(&root, &base, &excluded).unwrap());

    fs::write(root.join("lib/greet.txt"), "Hello, Almir!\n").unwrap();
    assert!(workspace_changed(&root, &base, &excluded).unwrap());
    fs::remove_dir_all(root).unwrap();
}

fn temporary_repository() -> PathBuf {
    let id = NEXT_REPO.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "kogen-workspace-changed-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    root
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("git is available");
    assert!(status.success(), "git {args:?} succeeded");
}

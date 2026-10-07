use super::workspace_changed;
use crate::gate::commit_tree_id;
use crate::git::GitRepo;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

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

use super::super::fixtures::{Fixture, git, path};
use super::support::{FastWait, NeverMoved};
use crate::git::GitRepo;
use crate::git::landing::{LandingOutcome, land};
use std::fs;

#[test]
fn clean_checked_out_base_worktree_follows_landed_commit() {
    let mut fixture = Fixture::new("worktree-clean");
    let checkout = fixture.root.join("checked out base");
    git(
        &fixture.origin,
        &["worktree", "add", path(&checkout), "main"],
    );
    assert_eq!(
        git(
            &checkout,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
    let tree = fixture.verified_tree();
    let mut integration = NeverMoved;
    let mut wait = FastWait::default();
    let outcome = land(fixture.request(&tree), &mut integration, &mut wait)
        .expect("land and update clean checked-out base");
    let LandingOutcome::Landed {
        commit,
        warnings,
        cleanup_failures,
        ..
    } = outcome
    else {
        panic!("candidate should land")
    };
    assert!(
        warnings.is_empty(),
        "unexpected worktree warning: {warnings:?}"
    );
    assert!(cleanup_failures.is_empty());
    assert_eq!(
        GitRepo::new(&checkout).resolve_commit("HEAD").unwrap(),
        commit
    );
    assert_eq!(
        GitRepo::new(&checkout)
            .text(&["symbolic-ref", "HEAD"])
            .unwrap(),
        "refs/heads/main"
    );
    assert_eq!(
        fs::read(checkout.join("README.md")).unwrap(),
        b"candidate\n"
    );
}

#[test]
fn dirty_checked_out_base_worktree_is_preserved_and_warned() {
    let mut fixture = Fixture::new("worktree-dirty");
    let checkout = fixture.root.join("dirty checked out base");
    git(
        &fixture.origin,
        &["worktree", "add", path(&checkout), "main"],
    );
    fs::write(checkout.join("README.md"), b"local edit\n").expect("make checkout dirty");
    let tree = fixture.verified_tree();
    let mut integration = NeverMoved;
    let mut wait = FastWait::default();
    let outcome = land(fixture.request(&tree), &mut integration, &mut wait)
        .expect("land without overwriting dirty checked-out base");
    let LandingOutcome::Landed {
        commit,
        warnings,
        cleanup_failures,
        ..
    } = outcome
    else {
        panic!("candidate should land")
    };
    assert!(cleanup_failures.is_empty());
    assert_eq!(
        fs::read(checkout.join("README.md")).unwrap(),
        b"local edit\n"
    );
    assert_eq!(
        GitRepo::new(&checkout).resolve_commit("HEAD").unwrap(),
        commit
    );
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("has local changes and was not updated"));
}

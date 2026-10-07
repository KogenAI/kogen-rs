use super::*;
use std::fs;
use std::path::PathBuf;

#[test]
fn snapshot_includes_excluded_untracked_files() {
    let repo = test_dir("excluded");
    git(&repo, &["init", "--quiet"]);
    identity(&repo);
    fs::create_dir_all(repo.join(".git/info")).unwrap();
    fs::write(repo.join(".git/info/exclude"), "masked.txt\n").unwrap();
    let before = snapshot_tree(&repo).unwrap();
    fs::write(repo.join("masked.txt"), b"must be in the candidate tree").unwrap();
    let after = snapshot_tree(&repo).unwrap();
    assert_ne!(before, after);
    let _ = fs::remove_dir_all(repo);
}

#[test]
fn snapshot_uses_raw_bytes_and_git_modes_without_running_hooks() {
    let repo = test_dir("raw");
    git(&repo, &["init", "--quiet"]);
    identity(&repo);
    fs::create_dir_all(repo.join(".git/hooks")).unwrap();
    let marker = repo.join("hook-ran");
    let hook = repo.join(".git/hooks/post-checkout");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    fs::write(repo.join("script.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(repo.join("script.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let raw = snapshot_tree(&repo).unwrap();
    assert!(!marker.exists());
    assert!(matches!(raw.len(), 40 | 64));
    let _ = fs::remove_dir_all(repo);
}

#[cfg(unix)]
#[test]
fn snapshot_matches_a_git_tree_for_executable_files_and_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let repo = test_dir("modes");
    git(&repo, &["init", "--quiet"]);
    identity(&repo);
    fs::write(repo.join("run.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(repo.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("run.sh", repo.join("run-link")).unwrap();
    git(&repo, &["add", "-f", "--all"]);
    let expected = kogen_test_support::git_command()
        .args(["write-tree"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(expected.status.success());
    let expected = String::from_utf8_lossy(&expected.stdout).trim().to_owned();
    assert_eq!(snapshot_tree(&repo).unwrap(), expected);
    let _ = fs::remove_dir_all(repo);
}

fn git(root: &PathBuf, args: &[&str]) {
    let output = kogen_test_support::git_command()
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn identity(repository: &std::path::Path) {
    kogen_test_support::set_identity(repository, "Kogen Gate Test", "gate@example.invalid")
        .expect("configure gate fixture identity");
}

fn test_dir(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "kogen-gate-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

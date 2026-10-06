use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let root = env!("CARGO_MANIFEST_DIR");
    rerun_on_git_state(root);
    let revision =
        git(root, &["rev-parse", "--short=8", "HEAD"]).unwrap_or_else(|| "00000000".to_owned());
    let date = git(root, &["show", "-s", "--format=%cs", "HEAD"])
        .unwrap_or_else(|| "1970-01-01".to_owned());
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(root)
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty());

    println!("cargo:rustc-env=KOGEN_SOURCE_SHA={revision}");
    println!("cargo:rustc-env=KOGEN_SOURCE_DATE={date}");
    println!("cargo:rustc-env=KOGEN_UNCOMMITTED={dirty}");
}

fn rerun_on_git_state(root: &str) {
    println!("cargo:rerun-if-changed={root}/src");
    println!("cargo:rerun-if-changed={root}/data/help");
    for git_path in ["HEAD", "index"] {
        if let Some(path) = git_internal_path(root, git_path) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn git_internal_path(root: &str, path: &str) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", path])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let git_path = String::from_utf8(output.stdout).ok()?;
    let git_path = Path::new(git_path.trim());
    Some(if git_path.is_absolute() {
        git_path.to_path_buf()
    } else {
        Path::new(root).join(git_path)
    })
}

fn git(root: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_owned())
}

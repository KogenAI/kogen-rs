//! Check-tree snapshots for staged acceptance runs.

use crate::gate::ledger::TreeSnapshotPort;
use sha2::Digest as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) struct GitSnapshot;

impl TreeSnapshotPort for GitSnapshot {
    fn snapshot(&self, workdir: &Path) -> Result<String, String> {
        let diff = git_bytes(workdir, &["diff", "--binary", "HEAD"])?;
        let untracked = git_bytes(
            workdir,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        let mut digest = sha2::Sha256::new();
        digest.update(diff);
        for raw_path in untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            digest.update(raw_path);
            let Some(path) = path_from_git_bytes(raw_path) else {
                continue;
            };
            let full = workdir.join(path);
            if let Ok(bytes) = fs::read(&full) {
                digest.update(bytes);
            } else if let Ok(target) = fs::read_link(&full) {
                digest.update(target.as_os_str().as_encoded_bytes());
            }
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}

#[cfg(unix)]
fn path_from_git_bytes(path: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(path)))
}

#[cfg(not(unix))]
fn path_from_git_bytes(path: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(path).ok().map(PathBuf::from)
}

fn git_bytes(workdir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .map_err(|error| format!("run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!("git {} failed", args.join(" ")));
    }
    Ok(output.stdout)
}

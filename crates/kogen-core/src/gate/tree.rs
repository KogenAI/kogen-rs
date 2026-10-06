use super::ledger::TreeSnapshotPort;
use super::workspace::{WorkspaceError, WorkspaceTree};
use std::ffi::OsStr;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_INDEX: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub struct GitTreeSnapshot;

impl TreeSnapshotPort for GitTreeSnapshot {
    fn snapshot(&self, workdir: &Path) -> Result<String, String> {
        snapshot_tree(workdir).map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeSnapshotError {
    pub operation: &'static str,
    pub detail: String,
}

impl fmt::Display for TreeSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl std::error::Error for TreeSnapshotError {}

pub fn snapshot_tree(workdir: &Path) -> Result<String, TreeSnapshotError> {
    let tree = WorkspaceTree::capture(workdir).map_err(workspace_error)?;
    write_tree(&tree)
}

/// Resolves the tree object for a commit or tree-ish in a local repository.
pub fn commit_tree_id(repository: &Path, revision: &str) -> Result<String, TreeSnapshotError> {
    let output = git(
        repository,
        None,
        &["rev-parse", "--verify", &format!("{revision}^{{tree}}")],
        None,
    )?;
    let value = String::from_utf8_lossy(&output).trim().to_owned();
    if is_object_id(&value) {
        Ok(value)
    } else {
        Err(TreeSnapshotError {
            operation: "resolve tree",
            detail: "git returned an invalid tree id".to_owned(),
        })
    }
}

fn write_tree(tree: &WorkspaceTree) -> Result<String, TreeSnapshotError> {
    let index = PrivateIndex::new()?;
    git(
        &tree.root,
        Some(&index.path),
        &["read-tree", "--empty"],
        None,
    )?;
    for entry in tree.entries.values() {
        let hash = git(
            &tree.root,
            Some(&index.path),
            &["hash-object", "-w", "--no-filters", "--stdin"],
            Some(&entry.bytes),
        )?;
        let object = String::from_utf8_lossy(&hash).trim().to_owned();
        if !is_object_id(&object) {
            return Err(TreeSnapshotError {
                operation: "hash workspace entry",
                detail: "git returned an invalid object id".to_owned(),
            });
        }
        let cache_info = cache_info(entry.mode, &object, &entry.path);
        git(
            &tree.root,
            Some(&index.path),
            &["update-index", "-z", "--index-info"],
            Some(&cache_info),
        )?;
    }
    let output = git(&tree.root, Some(&index.path), &["write-tree"], None)?;
    let object = String::from_utf8_lossy(&output).trim().to_owned();
    if is_object_id(&object) {
        Ok(object)
    } else {
        Err(TreeSnapshotError {
            operation: "write workspace tree",
            detail: "git returned an invalid tree id".to_owned(),
        })
    }
}

fn cache_info(mode: u32, object: &str, path: &Path) -> Vec<u8> {
    let mut bytes = format!("{mode:o} {object}\t").into_bytes();
    bytes.extend_from_slice(path_bytes(path.as_os_str()));
    bytes.push(0);
    bytes
}

fn git(
    repository: &Path,
    index: Option<&Path>,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<Vec<u8>, TreeSnapshotError> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.filemode=true",
        ])
        .args(args)
        .current_dir(repository)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::temp_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut child = command.spawn().map_err(|error| TreeSnapshotError {
        operation: "start git",
        detail: error.to_string(),
    })?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("stdin is piped for commands with input")
            .write_all(input)
            .map_err(|error| TreeSnapshotError {
                operation: "write git input",
                detail: error.to_string(),
            })?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| TreeSnapshotError {
            operation: "wait for git",
            detail: error.to_string(),
        })?;
    success(args.first().copied().unwrap_or("git"), output)
}

fn success(operation: &str, output: Output) -> Result<Vec<u8>, TreeSnapshotError> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(TreeSnapshotError {
        operation: "git workspace tree",
        detail: format!(
            "{operation}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    })
}

struct PrivateIndex {
    directory: PathBuf,
    path: PathBuf,
}

impl PrivateIndex {
    fn new() -> Result<Self, TreeSnapshotError> {
        let directory = std::env::temp_dir();
        for _ in 0..32 {
            let id = NEXT_INDEX.fetch_add(1, Ordering::Relaxed);
            let private_dir =
                directory.join(format!("kogen-gate-index-{}-{id}", std::process::id()));
            match std::fs::create_dir(&private_dir) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(
                            &private_dir,
                            std::fs::Permissions::from_mode(0o700),
                        )
                        .map_err(|error| TreeSnapshotError {
                            operation: "protect private git index",
                            detail: error.to_string(),
                        })?;
                    }
                    return Ok(Self {
                        path: private_dir.join("index"),
                        directory: private_dir,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(TreeSnapshotError {
                        operation: "create private git index",
                        detail: error.to_string(),
                    });
                }
            }
        }
        Err(TreeSnapshotError {
            operation: "create private git index",
            detail: "could not allocate a unique index path".to_owned(),
        })
    }
}

impl Drop for PrivateIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn workspace_error(error: WorkspaceError) -> TreeSnapshotError {
    TreeSnapshotError {
        operation: "read workspace tree",
        detail: error.to_string(),
    }
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(unix)]
fn path_bytes(path: &OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    path.as_bytes()
}

#[cfg(not(unix))]
fn path_bytes(path: &OsStr) -> &[u8] {
    // Windows Git paths are UTF-8 in practice; keep the temporary command
    // protocol byte-based so names with spaces remain unambiguous.
    path.to_str().unwrap_or_default().as_bytes()
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;

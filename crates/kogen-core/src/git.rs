//! Small, explicit Git operations shared by approval and later ref owners.

pub mod landing;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static NEXT_INDEX: AtomicU64 = AtomicU64::new(0);

const WORKSPACE_SAFE_CONFIG: &[&str] = &[
    "core.hooksPath=/dev/null",
    "core.fsmonitor=false",
    "core.autocrlf=false",
    "core.filemode=true",
    "core.excludesFile=/dev/null",
    "core.attributesFile=/dev/null",
    "commit.gpgsign=false",
];

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_STDOUT_LIMIT: usize = 64 * 1024 * 1024;
static NEXT_GIT_RUN_DIR: AtomicU64 = AtomicU64::new(0);
static WORKSPACE_BASE_PATHS: OnceLock<Mutex<BTreeMap<PathBuf, BTreeSet<PathBuf>>>> =
    OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) struct GitCommandOutput {
    pub exit_status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl GitCommandOutput {
    pub fn success(&self) -> bool {
        self.exit_status == Some(0)
    }
}

/// All controller Git commands share this bounded process-group runner.
pub(crate) fn run_git_command(
    command: Command,
    input: Option<&[u8]>,
) -> Result<GitCommandOutput, GitError> {
    run_git_command_with_limits(command, input, GIT_TIMEOUT, GIT_STDOUT_LIMIT)
}

fn run_git_command_with_limits(
    command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
    stdout_limit: usize,
) -> Result<GitCommandOutput, GitError> {
    let run_dir = GitRunDirectory::new().map_err(|error| GitError {
        operation: "prepare supervised git".to_owned(),
        detail: error.to_string(),
    })?;
    let result =
        crate::run::run_bounded_command(command, &run_dir.path, input, timeout, stdout_limit)
            .map_err(|error| GitError {
                operation: "run supervised git".to_owned(),
                detail: error.to_string(),
            })?;
    if result.timed_out {
        return Err(GitError {
            operation: "run supervised git".to_owned(),
            detail: format!("timed out after {} ms", timeout.as_millis()),
        });
    }
    if result.stdout_truncated {
        return Err(GitError {
            operation: "run supervised git".to_owned(),
            detail: format!("stdout exceeded the {stdout_limit} byte capture limit"),
        });
    }
    Ok(GitCommandOutput {
        exit_status: result.exit_status,
        stdout: result.stdout,
        stderr: result.stderr_tail,
    })
}

struct GitRunDirectory {
    path: PathBuf,
}

impl GitRunDirectory {
    fn new() -> std::io::Result<Self> {
        for _ in 0..32 {
            let id = NEXT_GIT_RUN_DIR.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("kogen-git-run-{}-{id}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
                    }
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a private Git process directory",
        ))
    }
}

impl Drop for GitRunDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn register_workspace_base(root: &Path, base_commit: &str) -> Result<(), GitError> {
    let output =
        GitRepo::workspace(root).output(&["ls-tree", "-r", "--name-only", "-z", base_commit])?;
    let paths = output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(git_path_from_bytes)
        .collect();
    let root = std::fs::canonicalize(root).map_err(|error| GitError {
        operation: "register workspace base".to_owned(),
        detail: error.to_string(),
    })?;
    WORKSPACE_BASE_PATHS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|error| GitError {
            operation: "register workspace base".to_owned(),
            detail: error.to_string(),
        })?
        .insert(root, paths);
    Ok(())
}

pub(crate) fn workspace_base_paths(root: &Path) -> Option<BTreeSet<PathBuf>> {
    let root = std::fs::canonicalize(root).ok()?;
    WORKSPACE_BASE_PATHS.get()?.lock().ok()?.get(&root).cloned()
}

pub(crate) fn forget_workspace(root: &Path) {
    let key = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if let Some(workspaces) = WORKSPACE_BASE_PATHS.get()
        && let Ok(mut workspaces) = workspaces.lock()
    {
        workspaces.remove(&key);
    }
}

#[cfg(unix)]
fn git_path_from_bytes(path: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
fn git_path_from_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

#[derive(Clone, Debug)]
pub struct GitRepo {
    path: PathBuf,
    workspace: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitError {
    pub operation: String,
    pub detail: String,
}

impl fmt::Display for GitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl std::error::Error for GitError {}

impl GitRepo {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            workspace: false,
        }
    }

    /// A build workspace whose local and global Git controls must not run Kogen commands.
    #[must_use]
    pub fn workspace(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            workspace: true,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn output(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        self.output_with_env(args, &[], None)
    }

    pub fn output_with_env(
        &self,
        args: &[&str],
        env: &[(&str, &std::ffi::OsStr)],
        input: Option<&[u8]>,
    ) -> Result<Vec<u8>, GitError> {
        let mut command = Command::new("git");
        command
            .args(self.safe_workspace_args())
            .args(args)
            .current_dir(&self.path)
            .envs(env.iter().copied());
        self.configure_workspace_environment(&mut command);
        #[cfg(any(test, feature = "hermetic-git-tests"))]
        kogen_test_support::configure_git_command(&mut command);
        let output = run_git_command(command, input).map_err(|mut error| {
            error.operation = format!("git {}", args.first().copied().unwrap_or(""));
            error
        })?;
        if !output.success() {
            return Err(GitError {
                operation: format!("git {}", args.first().copied().unwrap_or("")),
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(output.stdout)
    }

    fn safe_workspace_args(&self) -> impl Iterator<Item = OsString> + '_ {
        self.workspace
            .then_some(WORKSPACE_SAFE_CONFIG)
            .into_iter()
            .flatten()
            .flat_map(|value| [OsString::from("-c"), OsString::from(*value)])
    }

    fn configure_workspace_environment(&self, command: &mut Command) {
        if self.workspace {
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_ATTR_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0");
        }
    }

    pub fn text(&self, args: &[&str]) -> Result<String, GitError> {
        self.output(args)
            .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
    }

    pub fn try_text(&self, args: &[&str]) -> Option<String> {
        self.text(args).ok()
    }

    pub fn resolve_commit(&self, rev: &str) -> Result<String, GitError> {
        self.text(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
    }

    pub fn resolve_tree(&self, rev: &str) -> Result<String, GitError> {
        self.text(&["rev-parse", "--verify", &format!("{rev}^{{tree}}")])
    }

    pub fn ref_target(&self, name: &str) -> Result<Option<String>, GitError> {
        match self.text(&["rev-parse", "--verify", "--quiet", name]) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.detail.is_empty() => Ok(None),
            Err(error)
                if error.detail.contains("Needed a single revision")
                    || error.detail.contains("unknown revision") =>
            {
                Ok(None)
            }
            Err(error) if error.detail.contains("not a valid object name") => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn blob_at(&self, commit: &str, path: &str) -> Result<Option<Vec<u8>>, GitError> {
        let object = format!("{commit}:{path}");
        if self.output(&["cat-file", "-e", &object]).is_err() {
            return Ok(None);
        }
        self.output(&["cat-file", "blob", &object]).map(Some)
    }

    pub fn object_format(&self) -> Result<&'static str, GitError> {
        match self.text(&["rev-parse", "--show-object-format"])?.as_str() {
            "sha1" => Ok("sha1"),
            "sha256" => Ok("sha256"),
            value => Err(GitError {
                operation: "git object format".to_owned(),
                detail: format!("unsupported object format {value}"),
            }),
        }
    }

    pub fn author_identity(&self) -> Result<String, GitError> {
        let identity = self.text(&["var", "GIT_AUTHOR_IDENT"])?;
        let Some(end) = identity.find('>') else {
            return Err(GitError {
                operation: "git author identity".to_owned(),
                detail: "identity is malformed".to_owned(),
            });
        };
        if identity[..end].rfind('<').is_none() {
            return Err(GitError {
                operation: "git author identity".to_owned(),
                detail: "identity is malformed".to_owned(),
            });
        }
        Ok(identity[..=end].trim().to_owned())
    }

    pub fn list_paths(&self, rev: &str) -> Result<Vec<String>, GitError> {
        let bytes = self.output(&["ls-tree", "-r", "--name-only", "-z", rev])?;
        Ok(bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect())
    }

    pub fn cas_ref(
        &self,
        name: &str,
        new_value: &str,
        expected: Option<&str>,
    ) -> Result<bool, GitError> {
        let zero = match self.object_format()? {
            "sha1" => "0000000000000000000000000000000000000000",
            _ => "0000000000000000000000000000000000000000000000000000000000000000",
        };
        let old = expected.unwrap_or(zero);
        match self.output(&["update-ref", name, new_value, old]) {
            Ok(_) => Ok(true),
            Err(error)
                if error.detail.contains("cannot lock ref")
                    || error.detail.contains("is at")
                    || error.detail.contains("reference already exists") =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub fn delete_ref_cas(&self, name: &str, expected: &str) -> Result<bool, GitError> {
        match self.output(&["update-ref", "-d", name, expected]) {
            Ok(_) => Ok(true),
            Err(error)
                if error.detail.contains("cannot lock ref") || error.detail.contains("is at") =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub fn create_commit(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        parent: Option<&str>,
        message: &str,
    ) -> Result<String, GitError> {
        let tree = self.private_tree(files)?;
        let mut args = vec!["commit-tree", &tree];
        if let Some(parent) = parent {
            args.push("-p");
            args.push(parent);
        }
        self.output_with_env(&args, &[], Some(message.as_bytes()))
            .map(|output| String::from_utf8_lossy(&output).trim().to_owned())
    }

    /// Create a descendant commit that preserves the parent's tree.
    pub fn create_descendant_commit(
        &self,
        parent: &str,
        message: &str,
    ) -> Result<String, GitError> {
        let tree = self.resolve_tree(parent)?;
        self.commit_tree(&tree, Some(parent), message)
    }

    pub fn path_in_index(&self, path: &str) -> Result<bool, GitError> {
        match self.output(&["ls-files", "--error-unmatch", "--", path]) {
            Ok(_) => Ok(true),
            Err(error) if error.detail.contains("did not match any files") => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn remove_paths_commit(
        &self,
        paths: &[String],
        message: &str,
    ) -> Result<(String, Vec<String>), GitError> {
        let parent = self.resolve_commit("HEAD")?;
        let index = PrivateIndex::new(&self.path)?;
        index.run(&["read-tree", "HEAD"])?;
        let mut actual = Vec::new();
        for path in paths {
            if !self.path_in_index(path)? {
                continue;
            }
            index.run(&["update-index", "--force-remove", "--", path])?;
            actual.push(path.clone());
        }
        if actual.is_empty() {
            return Err(GitError {
                operation: "git remove paths".to_owned(),
                detail: "no tracked paths".to_owned(),
            });
        }
        let tree = index.text(&["write-tree"])?;
        let commit = self.commit_tree(&tree, Some(&parent), message)?;
        let branch_ref = self.text(&["symbolic-ref", "-q", "HEAD"])?;
        if !self.cas_ref(&branch_ref, &commit, Some(&parent))? {
            return Err(GitError {
                operation: "git remove paths".to_owned(),
                detail: "checkout HEAD moved while removing the Intent".to_owned(),
            });
        }
        for path in &actual {
            let _ = self.output(&["update-index", "--force-remove", "--", path]);
        }
        Ok((commit, actual))
    }

    fn private_tree(&self, files: &BTreeMap<String, Vec<u8>>) -> Result<String, GitError> {
        let index = PrivateIndex::new(&self.path)?;
        index.run(&["read-tree", "--empty"])?;
        for (path, bytes) in files {
            let hash = self.output_with_env(&["hash-object", "-w", "--stdin"], &[], Some(bytes))?;
            let hash = String::from_utf8_lossy(&hash).trim().to_owned();
            index.run(&[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{hash},{path}"),
            ])?;
        }
        index.text(&["write-tree"])
    }

    fn commit_tree(
        &self,
        tree: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<String, GitError> {
        let mut args = vec!["commit-tree", tree];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        self.output_with_env(&args, &[], Some(message.as_bytes()))
            .map(|output| String::from_utf8_lossy(&output).trim().to_owned())
    }
}

struct PrivateIndex {
    repo: GitRepo,
    path: PathBuf,
}

impl PrivateIndex {
    fn new(repo: &Path) -> Result<Self, GitError> {
        let id = NEXT_INDEX.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("kogen-index-{}-{id}", std::process::id()));
        Ok(Self {
            repo: GitRepo::new(repo),
            path,
        })
    }

    fn env(&self) -> [(&'static str, &std::ffi::OsStr); 1] {
        [("GIT_INDEX_FILE", self.path.as_os_str())]
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        self.repo.output_with_env(args, &self.env(), None)
    }

    fn text(&self, args: &[&str]) -> Result<String, GitError> {
        self.run(args)
            .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
    }
}

impl Drop for PrivateIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[allow(dead_code)]
fn _git_env_key() -> OsString {
    OsString::from("GIT_INDEX_FILE")
}

#[cfg(test)]
mod supervised_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn supervised_git_effects_kill_hanging_helpers_and_cap_stdout() {
        let started = std::time::Instant::now();
        let mut hanging = Command::new("/bin/sh");
        hanging.args(["-c", "sleep 10"]);
        let error = run_git_command_with_limits(hanging, None, Duration::from_millis(40), 1024)
            .expect_err("the hanging helper must hit the process deadline");
        assert!(error.detail.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));

        let mut noisy = Command::new("/usr/bin/head");
        noisy.args(["-c", "4096", "/dev/zero"]);
        let error = run_git_command_with_limits(noisy, None, Duration::from_secs(2), 1024)
            .expect_err("oversized output must be rejected");
        assert!(error.detail.contains("capture limit"));
    }
}

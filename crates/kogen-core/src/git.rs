//! Small, explicit Git operations shared by approval and later ref owners.

pub mod landing;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_INDEX: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct GitRepo {
    path: PathBuf,
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
        Self { path: path.into() }
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
            .args(args)
            .current_dir(&self.path)
            .envs(env.iter().copied());
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| GitError {
                operation: format!("git {}", args.first().copied().unwrap_or("")),
                detail: error.to_string(),
            })?;
        if let Some(bytes) = input {
            child
                .stdin
                .take()
                .expect("stdin was piped")
                .write_all(bytes)
                .map_err(|error| GitError {
                    operation: format!("git {} input", args.first().copied().unwrap_or("")),
                    detail: error.to_string(),
                })?;
        }
        let output = child.wait_with_output().map_err(|error| GitError {
            operation: format!("git {}", args.first().copied().unwrap_or("")),
            detail: error.to_string(),
        })?;
        if !output.status.success() {
            return Err(GitError {
                operation: format!("git {}", args.first().copied().unwrap_or("")),
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(output.stdout)
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

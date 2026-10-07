use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_IGNORE_GIT_DIR: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeEntry {
    pub path: PathBuf,
    pub mode: u32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceTree {
    pub root: PathBuf,
    pub entries: BTreeMap<PathBuf, TreeEntry>,
    excluded_paths: Vec<PathBuf>,
}

impl WorkspaceTree {
    pub fn capture_excluding(
        root: &Path,
        excluded_paths: &[PathBuf],
    ) -> Result<Self, WorkspaceError> {
        let root = fs::canonicalize(root).map_err(|source| WorkspaceError::Io {
            operation: "resolve workspace",
            path: root.to_path_buf(),
            source,
        })?;
        if !root.is_dir() {
            return Err(WorkspaceError::NotDirectory(root));
        }
        let mut entries = BTreeMap::new();
        visit(&root, &root, &mut entries, excluded_paths)?;
        apply_gitignore(&root, &mut entries)?;
        Ok(Self {
            root,
            entries,
            excluded_paths: excluded_paths.to_vec(),
        })
    }

    pub fn changed_paths(&self, other: &Self) -> Vec<String> {
        let paths = self
            .entries
            .keys()
            .chain(other.entries.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        paths
            .into_iter()
            .filter_map(|path| {
                (self.entries.get(&path) != other.entries.get(&path))
                    .then(|| path.to_string_lossy().into_owned())
            })
            .collect()
    }

    /// Replaces workspace files from this snapshot and leaves the root `.git`
    /// entry intact. It is used after a check or test changes its input tree.
    pub fn restore(&self) -> Result<(), WorkspaceError> {
        clear_workspace(&self.root, &self.excluded_paths)?;
        for entry in self.entries.values() {
            let destination = self.root.join(&entry.path);
            create_parents(&self.root, &entry.path)?;
            write_entry(&destination, entry)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum WorkspaceError {
    NotDirectory(PathBuf),
    UnsupportedFile(PathBuf),
    Git(String),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "workspace is not a directory: {}",
                    path.display()
                )
            }
            Self::UnsupportedFile(path) => write!(
                formatter,
                "workspace contains a non-file entry: {}",
                path.display()
            ),
            Self::Git(detail) => write!(formatter, "read Git ignore state: {detail}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for WorkspaceError {}

fn apply_gitignore(
    root: &Path,
    entries: &mut BTreeMap<PathBuf, TreeEntry>,
) -> Result<(), WorkspaceError> {
    let git_dir = root.join(".git");
    match fs::symlink_metadata(&git_dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(WorkspaceError::Git(
                "workspace .git entry is not a real directory".to_owned(),
            ));
        }
        Err(error) => return Err(io_error("inspect workspace Git directory", &git_dir, error)),
    }

    let tracked = match crate::git::workspace_base_paths(root) {
        Some(paths) => paths,
        None => crate::git::GitRepo::workspace(root)
            .output(&["ls-files", "--cached", "-z"])
            .map_err(|error| WorkspaceError::Git(error.to_string()))?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(path_from_bytes)
            .collect::<std::collections::BTreeSet<_>>(),
    };
    let mut input = Vec::new();
    for path in entries.keys() {
        input.extend_from_slice(path_bytes(path.as_os_str()));
        input.push(0);
    }
    if input.is_empty() {
        return Ok(());
    }

    let private_git_dir = PrivateIgnoreGitDir::new(root)?;
    let git_dir = private_git_dir.path.as_os_str();
    let work_tree = root.as_os_str();
    let environment = [("GIT_DIR", git_dir), ("GIT_WORK_TREE", work_tree)];
    let ignored = match crate::git::GitRepo::workspace(root).output_with_env(
        &["check-ignore", "--no-index", "-z", "--stdin"],
        &environment,
        Some(&input),
    ) {
        Ok(paths) => paths,
        Err(error) if error.detail.is_empty() => Vec::new(),
        Err(error) => return Err(WorkspaceError::Git(error.to_string())),
    }
    .split(|byte| *byte == 0)
    .filter(|path| !path.is_empty())
    .map(path_from_bytes)
    .collect::<std::collections::BTreeSet<_>>();

    entries.retain(|path, _| tracked.contains(path) || !ignored.contains(path));
    Ok(())
}

struct PrivateIgnoreGitDir {
    path: PathBuf,
}

impl PrivateIgnoreGitDir {
    fn new(root: &Path) -> Result<Self, WorkspaceError> {
        for _ in 0..32 {
            let id = NEXT_IGNORE_GIT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("kogen-ignore-gitdir-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(
                            |error| io_error("protect private Git directory", &path, error),
                        )?;
                    }
                    let init = crate::git::GitRepo::workspace(root).output(&[
                        "init",
                        "--bare",
                        "--quiet",
                        "--template=",
                        path.to_str().ok_or_else(|| {
                            WorkspaceError::Git("temporary Git directory is not UTF-8".to_owned())
                        })?,
                    ]);
                    if let Err(error) = init {
                        let _ = fs::remove_dir_all(&path);
                        return Err(WorkspaceError::Git(error.to_string()));
                    }
                    let exclude = path.join("info/exclude");
                    match fs::remove_file(&exclude) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(io_error("disable private Git excludes", &exclude, error));
                        }
                    }
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io_error("create private Git directory", &path, error)),
            }
        }
        Err(WorkspaceError::Git(
            "could not allocate an isolated Git directory".to_owned(),
        ))
    }
}

impl Drop for PrivateIgnoreGitDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn path_bytes(path: &std::ffi::OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    path.as_bytes()
}

#[cfg(not(unix))]
fn path_bytes(path: &std::ffi::OsStr) -> &[u8] {
    path.to_str().unwrap_or_default().as_bytes()
}

#[cfg(unix)]
fn path_from_bytes(path: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
fn path_from_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

fn visit(
    root: &Path,
    directory: &Path,
    entries: &mut BTreeMap<PathBuf, TreeEntry>,
    excluded_paths: &[PathBuf],
) -> Result<(), WorkspaceError> {
    let mut children = fs::read_dir(directory)
        .map_err(|source| io_error("read workspace directory", directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error("read workspace entry", directory, source))?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let name = child.file_name();
        if name == ".git" {
            continue;
        }
        let path = child.path();
        let relative = path
            .strip_prefix(root)
            .expect("walked entries are descendants of the workspace")
            .to_path_buf();
        if excluded_paths
            .iter()
            .any(|excluded| relative.starts_with(excluded))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("inspect workspace entry", &path, source))?;
        if metadata.file_type().is_dir() {
            visit(root, &path, entries, excluded_paths)?;
            continue;
        }
        let (mode, bytes) = if metadata.file_type().is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|source| io_error("read workspace symlink", &path, source))?;
            (0o120000, os_bytes(target.as_os_str()))
        } else if metadata.is_file() {
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    0o100644
                } else {
                    0o100755
                }
            };
            #[cfg(not(unix))]
            let mode = 0o100644;
            let bytes =
                fs::read(&path).map_err(|source| io_error("read workspace file", &path, source))?;
            (mode, bytes)
        } else {
            return Err(WorkspaceError::UnsupportedFile(path));
        };
        let relative = path
            .strip_prefix(root)
            .expect("walked entries are descendants of the workspace")
            .to_path_buf();
        entries.insert(
            relative.clone(),
            TreeEntry {
                path: relative,
                mode,
                bytes,
            },
        );
    }
    Ok(())
}

fn clear_workspace(root: &Path, excluded_paths: &[PathBuf]) -> Result<(), WorkspaceError> {
    clear_directory(root, root, excluded_paths)
}

fn clear_directory(
    root: &Path,
    directory: &Path,
    excluded_paths: &[PathBuf],
) -> Result<(), WorkspaceError> {
    let children = fs::read_dir(directory)
        .map_err(|source| io_error("read workspace directory", directory, source))?;
    for child in children {
        let child = child.map_err(|source| io_error("read workspace entry", directory, source))?;
        if directory == root && child.file_name() == ".git" {
            continue;
        }
        let path = child.path();
        let relative = path
            .strip_prefix(root)
            .expect("walked entries are descendants of the workspace");
        if excluded_paths.iter().any(|excluded| relative == excluded) {
            continue;
        }
        if excluded_paths
            .iter()
            .any(|excluded| excluded.starts_with(relative))
        {
            if fs::symlink_metadata(&path)
                .map_err(|source| io_error("inspect workspace entry", &path, source))?
                .is_dir()
            {
                clear_directory(root, &path, excluded_paths)?;
            }
            continue;
        }
        remove_entry(&path)?;
    }
    Ok(())
}

fn create_parents(root: &Path, relative: &Path) -> Result<(), WorkspaceError> {
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(WorkspaceError::UnsupportedFile(relative.to_path_buf()));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                remove_entry(&current)?;
                fs::create_dir(&current)
                    .map_err(|source| io_error("create workspace directory", &current, source))?;
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .map_err(|source| io_error("create workspace directory", &current, source))?;
            }
            Err(source) => {
                return Err(io_error("inspect workspace directory", &current, source));
            }
        }
    }
    Ok(())
}

fn write_entry(destination: &Path, entry: &TreeEntry) -> Result<(), WorkspaceError> {
    if entry.mode == 0o120000 {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            use std::os::unix::fs::symlink;
            let target = PathBuf::from(std::ffi::OsString::from_vec(entry.bytes.clone()));
            return symlink(target, destination)
                .map_err(|source| io_error("restore workspace symlink", destination, source));
        }
        #[cfg(not(unix))]
        {
            return Err(WorkspaceError::UnsupportedFile(destination.to_path_buf()));
        }
    }
    fs::write(destination, &entry.bytes)
        .map_err(|source| io_error("restore workspace file", destination, source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if entry.mode == 0o100755 { 0o755 } else { 0o644 };
        fs::set_permissions(destination, fs::Permissions::from_mode(mode))
            .map_err(|source| io_error("restore workspace permissions", destination, source))?;
    }
    Ok(())
}

fn remove_entry(path: &Path) -> Result<(), WorkspaceError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error("inspect workspace entry", path, source)),
    };
    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
            .map_err(|source| io_error("remove workspace directory", path, source))
    } else {
        fs::remove_file(path).map_err(|source| io_error("remove workspace entry", path, source))
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> WorkspaceError {
    WorkspaceError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(unix)]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

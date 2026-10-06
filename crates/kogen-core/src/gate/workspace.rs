use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
}

impl WorkspaceTree {
    pub fn capture(root: &Path) -> Result<Self, WorkspaceError> {
        let root = fs::canonicalize(root).map_err(|source| WorkspaceError::Io {
            operation: "resolve workspace",
            path: root.to_path_buf(),
            source,
        })?;
        if !root.is_dir() {
            return Err(WorkspaceError::NotDirectory(root));
        }
        let mut entries = BTreeMap::new();
        visit(&root, &root, &mut entries)?;
        Ok(Self { root, entries })
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
        clear_workspace(&self.root)?;
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
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for WorkspaceError {}

fn visit(
    root: &Path,
    directory: &Path,
    entries: &mut BTreeMap<PathBuf, TreeEntry>,
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
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("inspect workspace entry", &path, source))?;
        if metadata.file_type().is_dir() {
            visit(root, &path, entries)?;
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

fn clear_workspace(root: &Path) -> Result<(), WorkspaceError> {
    let children =
        fs::read_dir(root).map_err(|source| io_error("read workspace directory", root, source))?;
    for child in children {
        let child = child.map_err(|source| io_error("read workspace entry", root, source))?;
        if child.file_name() == ".git" {
            continue;
        }
        remove_entry(&child.path())?;
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

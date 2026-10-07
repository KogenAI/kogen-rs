use crate::intent::intent_sha256;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub const ABSENT_SHA256: &str = "23518d434b5b519ad017aaaa4ca8e63cc5402a8051dec5242c918913969cfeec";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtectedEntry {
    pub sha256: String,
    /// Approved bytes for a present file; `None` means the path must stay absent.
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedFinding {
    pub path: String,
    pub expected_sha256: String,
    pub actual_sha256: String,
}

#[derive(Clone, Debug, Default)]
pub struct ProtectedWorkspace {
    manifest: BTreeMap<String, ProtectedEntry>,
    removed_sources: Vec<String>,
}

impl ProtectedWorkspace {
    pub fn new(
        manifest: BTreeMap<String, ProtectedEntry>,
        removed_sources: Vec<String>,
    ) -> Result<Self, ProtectionError> {
        for (path, entry) in &manifest {
            validate_relative_path(path)?;
            match &entry.bytes {
                Some(bytes) if intent_sha256(bytes) != entry.sha256 => {
                    return Err(ProtectionError::InvalidManifest(path.clone()));
                }
                None if entry.sha256 != ABSENT_SHA256 => {
                    return Err(ProtectionError::InvalidManifest(path.clone()));
                }
                _ => {}
            }
        }
        for path in &removed_sources {
            validate_relative_path(path)?;
        }
        Ok(Self {
            manifest,
            removed_sources,
        })
    }

    /// Checks exact approved bytes and confirms that source acceptance copies
    /// were removed from the build workspace.
    pub fn guard(&self, root: &Path) -> Result<Vec<ProtectedFinding>, ProtectionError> {
        let mut findings = Vec::new();
        for (path, expected) in &self.manifest {
            let actual = path_hash(root, path)?;
            if actual != expected.sha256 {
                findings.push(ProtectedFinding {
                    path: path.clone(),
                    expected_sha256: expected.sha256.clone(),
                    actual_sha256: actual,
                });
            }
        }
        for path in &self.removed_sources {
            if path_hash(root, path)? != ABSENT_SHA256 {
                findings.push(ProtectedFinding {
                    path: path.clone(),
                    expected_sha256: ABSENT_SHA256.to_owned(),
                    actual_sha256: "present".to_owned(),
                });
            }
        }
        Ok(findings)
    }

    /// Restores mismatched manifest paths and removes acceptance source copies.
    /// Returns each path that required a write or removal.
    pub fn restore_after_batch(&self, root: &Path) -> Result<Vec<String>, ProtectionError> {
        let mut restored = Vec::new();
        for (path, expected) in &self.manifest {
            if path_hash(root, path)? == expected.sha256 {
                continue;
            }
            match &expected.bytes {
                Some(bytes) => write_path(root, path, bytes)?,
                None => remove_path(root, path)?,
            }
            restored.push(path.clone());
        }
        for path in &self.removed_sources {
            if path_hash(root, path)? == ABSENT_SHA256 {
                continue;
            }
            remove_path(root, path)?;
            restored.push(path.clone());
        }
        Ok(restored)
    }
}

#[derive(Debug)]
pub enum ProtectionError {
    InvalidPath(String),
    InvalidManifest(String),
    UnsafeRoot(PathBuf),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl std::fmt::Display for ProtectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(formatter, "unsafe protected path: {path}"),
            Self::InvalidManifest(path) => {
                write!(formatter, "invalid protected manifest entry: {path}")
            }
            Self::UnsafeRoot(path) => write!(
                formatter,
                "protected workspace root is not a real directory: {}",
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

impl std::error::Error for ProtectionError {}

/// Installs the approved acceptance bytes at the configured candidate path
/// and removes the source copy from the workspace.
pub fn install_approved_acceptance(
    root: &Path,
    source_path: &str,
    candidate_path: &str,
    bytes: &[u8],
) -> Result<(), ProtectionError> {
    validate_relative_path(source_path)?;
    validate_relative_path(candidate_path)?;
    if source_path == candidate_path
        || source_path.starts_with(&format!("{candidate_path}/"))
        || candidate_path.starts_with(&format!("{source_path}/"))
    {
        return Err(ProtectionError::InvalidPath(candidate_path.to_owned()));
    }
    crate::safe_fs::validate_write(root, Path::new(source_path))
        .and_then(|()| crate::safe_fs::validate_write(root, Path::new(candidate_path)))
        .map_err(|source| {
            io_error(
                "validate approved acceptance installation",
                &safe_join(root, candidate_path).unwrap_or_else(|_| root.join(candidate_path)),
                source,
            )
        })?;
    write_path(root, candidate_path, bytes)?;
    remove_path(root, source_path)
}

fn path_hash(root: &Path, relative: &str) -> Result<String, ProtectionError> {
    let path = safe_join(root, relative)?;
    if parent_blocks(root, relative)? {
        return Ok(ABSENT_SHA256.to_owned());
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = fs::read_link(&path)
                .map_err(|source| io_error("read protected symlink", &path, source))?;
            Ok(intent_sha256(&os_bytes(target.as_os_str())))
        }
        Ok(metadata) if metadata.is_file() => {
            let bytes =
                fs::read(&path).map_err(|source| io_error("read protected file", &path, source))?;
            Ok(intent_sha256(&bytes))
        }
        Ok(_) => Ok(intent_sha256(b"kogen:non-file")),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(ABSENT_SHA256.to_owned())
        }
        Err(source) => Err(io_error("inspect protected path", &path, source)),
    }
}

fn parent_blocks(root: &Path, relative: &str) -> Result<bool, ProtectionError> {
    let components = relative.split('/').collect::<Vec<_>>();
    let mut current = root.to_path_buf();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Ok(true);
            }
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(source) if source.kind() == io::ErrorKind::NotADirectory => return Ok(true),
            Err(source) => return Err(io_error("inspect protected parent", &current, source)),
        }
    }
    Ok(false)
}

fn write_path(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), ProtectionError> {
    let path = safe_join(root, relative)?;
    let relative_path = Path::new(relative);
    let parents = relative_path.parent().unwrap_or_else(|| Path::new(""));
    crate::safe_fs::ensure_dir_replacing_non_dirs(root, parents)
        .and_then(|()| crate::safe_fs::validate_write(root, relative_path))
        .and_then(|()| crate::safe_fs::write_file(root, Path::new(relative), bytes))
        .map_err(|source| io_error("restore protected file", &path, source))
}

fn remove_path(root: &Path, relative: &str) -> Result<(), ProtectionError> {
    let path = safe_join(root, relative)?;
    let mut parent = root.to_path_buf();
    let components = relative.split('/').collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        parent.push(component);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                remove_entry(&parent)?;
                return Ok(());
            }
            Ok(_) => {}
            Err(source)
                if matches!(
                    source.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(());
            }
            Err(source) => return Err(io_error("inspect protected parent", &parent, source)),
        }
    }
    remove_entry(&path)
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, ProtectionError> {
    validate_relative_path(relative)?;
    let metadata = fs::symlink_metadata(root)
        .map_err(|source| io_error("inspect workspace root", root, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProtectionError::UnsafeRoot(root.to_path_buf()));
    }
    let root = fs::canonicalize(root)
        .map_err(|source| io_error("resolve workspace root", root, source))?;
    Ok(root.join(relative))
}

fn validate_relative_path(path: &str) -> Result<(), ProtectionError> {
    if path.is_empty()
        || path.contains(['\0', '\r', '\n'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part == ".git")
        || Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ProtectionError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

fn remove_entry(path: &Path) -> Result<(), ProtectionError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotADirectory => return Ok(()),
        Err(source) => return Err(io_error("inspect protected path", path, source)),
    };
    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
            .map_err(|source| io_error("remove protected directory", path, source))
    } else {
        fs::remove_file(path).map_err(|source| io_error("remove protected file", path, source))
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ProtectionError {
    ProtectionError::Io {
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

#[cfg(test)]
#[path = "protection_tests.rs"]
mod tests;

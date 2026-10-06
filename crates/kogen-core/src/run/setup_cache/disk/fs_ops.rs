use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(super) fn outputs_exist(checkout: &Path, outputs: &[String]) -> bool {
    let Ok(checkout) = fs::canonicalize(checkout) else {
        return false;
    };
    outputs.iter().all(|output| {
        let Some(path) = relative_join(&checkout, output) else {
            return false;
        };
        parent_chain_is_safe(&checkout, output) && fs::symlink_metadata(path).is_ok()
    })
}

pub(super) fn workspace_fingerprint(
    checkout: &Path,
    outputs: &[String],
) -> io::Result<BTreeMap<PathBuf, (u32, String)>> {
    let root = fs::canonicalize(checkout)?;
    let mut values = BTreeMap::new();
    fingerprint_dir(&root, &root, outputs, &mut values)?;
    Ok(values)
}

fn fingerprint_dir(
    root: &Path,
    directory: &Path,
    outputs: &[String],
    values: &mut BTreeMap<PathBuf, (u32, String)>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| invalid_path(path.display().to_string()))?
            .to_path_buf();
        if relative == Path::new(".git") || is_output(&relative, outputs) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() {
            fingerprint_dir(root, &path, outputs, values)?;
        } else if metadata.file_type().is_symlink() {
            let bytes = path_bytes(&fs::read_link(&path)?);
            values.insert(relative, (0o120000, sha256(&bytes)));
        } else if metadata.is_file() {
            values.insert(relative, (file_mode(&metadata), sha256(&fs::read(&path)?)));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported workspace entry",
            ));
        }
    }
    Ok(())
}

fn is_output(path: &Path, outputs: &[String]) -> bool {
    outputs.iter().any(|output| {
        let output = Path::new(output);
        path == output || path.starts_with(output)
    })
}

pub(super) fn copy_entry(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, destination)
        }
        #[cfg(not(unix))]
        {
            let _ = target;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "symlink copy is unsupported",
            ))
        }
    } else if metadata.is_dir() {
        fs::create_dir(destination)?;
        let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions())
    } else if metadata.is_file() {
        fs::copy(source, destination)?;
        fs::set_permissions(destination, metadata.permissions())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported cache output",
        ))
    }
}

pub(super) fn create_parents(root: &Path, relative: &str) -> io::Result<()> {
    let relative = Path::new(relative);
    let components = safe_components(relative)?;
    let mut current = root.to_path_buf();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "output parent is a symlink",
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                remove_path(&current)?;
                fs::create_dir(&current)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn parent_chain_is_safe(root: &Path, relative: &str) -> bool {
    let Ok(components) = safe_components(Path::new(relative)) else {
        return false;
    };
    let mut current = root.to_path_buf();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return false,
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return false,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }
    true
}

pub(super) fn relative_join(root: &Path, relative: &str) -> Option<PathBuf> {
    let components = safe_components(Path::new(relative)).ok()?;
    let mut path = root.to_path_buf();
    for component in components {
        path.push(component);
    }
    Some(path)
}

fn safe_components(path: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(value.to_os_string()),
            _ => Err(invalid_path(path.display().to_string())),
        })
        .collect::<io::Result<Vec<_>>>()?;
    if components.is_empty() {
        Err(invalid_path(path.display().to_string()))
    } else {
        Ok(components)
    }
}

pub(super) fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

pub(super) fn sync_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            sync_tree(&entry.path())?;
        }
        sync_directory(path)
    } else {
        fs::File::open(path)?.sync_all()
    }
}

pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn file_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            0o100644
        } else {
            0o100755
        }
    }
    #[cfg(not(unix))]
    {
        0o100644
    }
}

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().as_bytes().to_vec()
    }
}

pub(super) fn invalid_path(path: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("invalid relative path {}", path.into()),
    )
}

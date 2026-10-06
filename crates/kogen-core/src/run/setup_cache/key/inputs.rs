use super::{SetupInput, SetupKeyError, digest, key_io};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(super) fn input(root: &Path, relative: &str) -> Result<SetupInput, SetupKeyError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(SetupKeyError::InputPath(relative_path.to_path_buf()));
    }
    let path = root.join(relative_path);
    check_parent_chain(root, relative_path)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SetupInput {
                path: relative.to_owned(),
                mode: 0,
                sha256: digest(b"kogen:absent"),
            });
        }
        Err(error) => return Err(key_io(&path, error)),
    };
    let (mode, bytes) = if metadata.file_type().is_symlink() {
        check_symlink_target(root, &path)?;
        (
            0o120000,
            path_bytes(&fs::read_link(&path).map_err(|error| key_io(&path, error))?),
        )
    } else if metadata.is_file() {
        (
            file_mode(&metadata),
            fs::read(&path).map_err(|error| key_io(&path, error))?,
        )
    } else {
        return Err(SetupKeyError::InputNotFile(path));
    };
    Ok(SetupInput {
        path: relative.to_owned(),
        mode,
        sha256: digest(&bytes),
    })
}

fn check_parent_chain(root: &Path, relative: &Path) -> Result<(), SetupKeyError> {
    let mut current = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(name) = component else {
            return Err(SetupKeyError::InputPath(relative.to_path_buf()));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let resolved =
                    fs::canonicalize(&current).map_err(|error| key_io(&current, error))?;
                if !resolved.starts_with(root) {
                    return Err(SetupKeyError::InputOutsideCheckout(current));
                }
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(SetupKeyError::InputNotFile(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(key_io(&current, error)),
        }
    }
    Ok(())
}

fn check_symlink_target(root: &Path, path: &Path) -> Result<(), SetupKeyError> {
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(_) => {
            let link = fs::read_link(path).map_err(|error| key_io(path, error))?;
            let parent = path.parent().unwrap_or(root);
            let parent = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            normalize(&if link.is_absolute() {
                link
            } else {
                parent.join(link)
            })
        }
    };
    if target.starts_with(root) {
        Ok(())
    } else {
        Err(SetupKeyError::InputOutsideCheckout(path.to_path_buf()))
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            part => output.push(part.as_os_str()),
        }
    }
    output
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

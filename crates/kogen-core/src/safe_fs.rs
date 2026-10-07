//! Descriptor-relative file operations for controller writes into mutable trees.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path};

#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};

pub(crate) fn write_file(root: &Path, relative: &Path, bytes: &[u8]) -> io::Result<()> {
    let (parent, name) = parent_and_name(root, relative, true)?;
    #[cfg(unix)]
    let mut file = open_regular(
        &parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
        0o600,
    )?;
    #[cfg(not(unix))]
    let mut file = {
        reject_symlink_components(root, relative)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        options.open(root.join(relative))?
    };
    file.write_all(bytes)
}

pub(crate) fn create_file(root: &Path, relative: &Path, bytes: &[u8]) -> io::Result<()> {
    let (parent, name) = parent_and_name(root, relative, true)?;
    #[cfg(unix)]
    let mut file = open_regular(
        &parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
        0o600,
    )?;
    #[cfg(not(unix))]
    let mut file = {
        reject_symlink_components(root, relative)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        options.open(root.join(relative))?
    };
    file.write_all(bytes)
}

pub(crate) fn append_file(root: &Path, relative: &Path) -> io::Result<File> {
    let (parent, name) = parent_and_name(root, relative, false)?;
    #[cfg(unix)]
    {
        open_regular(
            &parent,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::APPEND,
            0o600,
        )
    }
    #[cfg(not(unix))]
    {
        reject_symlink_components(root, relative)?;
        let mut options = fs::OpenOptions::new();
        options.append(true).create(true);
        options.open(root.join(relative))
    }
}

pub(crate) fn read_file(root: &Path, relative: &Path) -> io::Result<Vec<u8>> {
    let mut file = open_read_file(root, relative)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn open_read_file(root: &Path, relative: &Path) -> io::Result<File> {
    let (parent, name) = parent_and_name(root, relative, false)?;
    #[cfg(unix)]
    {
        open_regular(&parent, name, OFlags::RDONLY, 0)
    }
    #[cfg(not(unix))]
    {
        reject_symlink_components(root, relative)?;
        File::open(root.join(relative))
    }
}

/// Validate a destination without creating any parent or writing any bytes.
pub(crate) fn validate_write(root: &Path, relative: &Path) -> io::Result<()> {
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a normal relative path",
        ));
    }
    #[cfg(unix)]
    {
        let mut parent = open_root(root)?;
        for component in &components[..components.len() - 1] {
            let Component::Normal(name) = component else {
                unreachable!();
            };
            match open_dir(&parent, name) {
                Ok(directory) => parent = directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        let Component::Normal(name) = components[components.len() - 1] else {
            unreachable!();
        };
        reject_final_symlink(&parent, name)
    }
    #[cfg(not(unix))]
    {
        reject_symlink_components(root, relative)
    }
}

pub(crate) fn ensure_dir(root: &Path, relative: &Path) -> io::Result<()> {
    let components = relative.components().collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a normal relative path",
        ));
    }
    #[cfg(unix)]
    {
        let mut directory = open_root(root)?;
        for component in components {
            let Component::Normal(name) = component else {
                unreachable!();
            };
            directory = open_or_create_dir(&directory, name)?;
        }
        directory.sync_all()
    }
    #[cfg(not(unix))]
    {
        let mut path = root.to_path_buf();
        for component in components {
            let Component::Normal(name) = component else {
                unreachable!();
            };
            path.push(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unsafe directory",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&path)?,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// Safely creates a directory tree below its nearest existing real ancestor.
pub(crate) fn ensure_directory_path(path: &Path) -> io::Result<()> {
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path contains a dot component",
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory ancestor is not a real directory",
                    ));
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory path has no ancestor",
                    )
                })?;
                suffix.push(name.to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory path has no ancestor",
                    )
                })?;
            }
            Err(error) => return Err(error),
        }
    }
    let root = fs::canonicalize(ancestor)?;
    let mut relative = std::path::PathBuf::new();
    for component in suffix.into_iter().rev() {
        relative.push(component);
    }
    ensure_dir(&root, &relative)
}

/// Creates parent directories and replaces any symlink or non-directory that
/// blocks one of them. Every removal and reopen is anchored to the verified
/// root descriptor, so a changed path component cannot redirect the repair.
pub(crate) fn ensure_dir_replacing_non_dirs(root: &Path, relative: &Path) -> io::Result<()> {
    let components = relative.components().collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a normal relative path",
        ));
    }
    #[cfg(unix)]
    {
        let mut directory = open_root(root)?;
        for component in components {
            let Component::Normal(name) = component else {
                unreachable!();
            };
            directory = match open_dir(&directory, name) {
                Ok(next) => next,
                Err(original) => {
                    match rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(stat)
                            if FileType::from_raw_mode(stat.st_mode) == FileType::Directory =>
                        {
                            return Err(original);
                        }
                        Ok(_) => rustix::fs::unlinkat(&directory, name, AtFlags::empty())
                            .map_err(io::Error::from)?,
                        Err(error) if io::Error::from(error).kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(io::Error::from(error)),
                    }
                    if let Err(error) =
                        rustix::fs::mkdirat(&directory, name, Mode::from_raw_mode(0o700))
                    {
                        let error = io::Error::from(error);
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    open_dir(&directory, name)?
                }
            };
        }
        directory.sync_all()
    }
    #[cfg(not(unix))]
    {
        ensure_dir(root, relative)
    }
}

pub(crate) fn atomic_replace(root: &Path, relative: &Path, bytes: &[u8]) -> io::Result<()> {
    let (parent, name) = parent_and_name(root, relative, false)?;
    #[cfg(unix)]
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

        let mut temp = None;
        let mut file = None;
        for _ in 0..32 {
            let candidate = format!(
                ".kogen-{}-{}.tmp",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            );
            match open_regular(
                &parent,
                Path::new(&candidate).as_os_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                0o600,
            ) {
                Ok(created) => {
                    temp = Some(candidate);
                    file = Some(created);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        let temp = temp.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate temporary file",
            )
        })?;
        let mut file = file.expect("temporary path and descriptor are paired");
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = unlink_at(&parent, &temp);
            return Err(error);
        }
        drop(file);
        reject_final_symlink(&parent, name)?;
        if let Err(error) = rustix::fs::renameat(&parent, temp.as_str(), &parent, name) {
            let error = io::Error::from(error);
            let _ = unlink_at(&parent, &temp);
            return Err(error);
        }
        parent.sync_all()
    }
    #[cfg(not(unix))]
    {
        reject_symlink_components(root, relative)?;
        let path = root.join(relative);
        let parent = path.parent().unwrap_or(root);
        let temporary = parent.join(format!(".kogen-{}.tmp", std::process::id()));
        write_file(parent, Path::new(temporary.file_name().unwrap()), bytes)?;
        fs::rename(temporary, path)?;
        File::open(parent)?.sync_all()
    }
}

pub(crate) fn remove_file(root: &Path, relative: &Path) -> io::Result<()> {
    let (parent, name) = parent_and_name(root, relative, false)?;
    #[cfg(unix)]
    {
        match rustix::fs::unlinkat(&parent, name, AtFlags::empty()) {
            Ok(()) => Ok(()),
            Err(error) if io::Error::from(error).kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io::Error::from(error)),
        }
    }
    #[cfg(not(unix))]
    {
        reject_symlink_components(root, relative)?;
        match fs::remove_file(root.join(relative)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn parent_and_name<'a>(
    root: &Path,
    relative: &'a Path,
    create: bool,
) -> io::Result<(File, &'a std::ffi::OsStr)> {
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a normal relative path",
        ));
    }
    let Component::Normal(name) = components[components.len() - 1] else {
        unreachable!();
    };
    #[cfg(unix)]
    {
        let mut parent = open_root(root)?;
        for component in &components[..components.len() - 1] {
            let Component::Normal(name) = component else {
                unreachable!();
            };
            parent = if create {
                open_or_create_dir(&parent, name)?
            } else {
                open_dir(&parent, name)?
            };
        }
        Ok((parent, name))
    }
    #[cfg(not(unix))]
    {
        let mut parent = root.to_path_buf();
        for component in &components[..components.len() - 1] {
            let Component::Normal(name) = component else {
                unreachable!();
            };
            parent.push(name);
            match fs::symlink_metadata(&parent) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unsafe path component",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                    fs::create_dir(&parent)?
                }
                Err(error) => return Err(error),
            }
        }
        Ok((File::open(parent)?, name))
    }
}

#[cfg(unix)]
fn open_root(root: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("root is not a real directory: {}", root.display()),
        ));
    }
    let canonical = fs::canonicalize(root)?;
    let mut current = File::from(
        rustix::fs::open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    for component in canonical.components() {
        if let Component::Normal(name) = component {
            current = open_dir(&current, name)?;
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn open_dir(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

#[cfg(unix)]
fn open_or_create_dir(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    match open_dir(parent, name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Err(error) = rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
                let error = io::Error::from(error);
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            open_dir(parent, name)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn open_regular(
    parent: &File,
    name: &std::ffi::OsStr,
    flags: OFlags,
    mode: u16,
) -> io::Result<File> {
    let file = rustix::fs::openat(
        parent,
        name,
        flags | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        // RawMode is u16 on macOS and u32 on Linux; `into` is a no-op on macOS.
        #[allow(clippy::useless_conversion)]
        Mode::from_raw_mode(mode.into()),
    )
    .map(File::from)
    .map_err(io::Error::from)?;
    let stat = rustix::fs::fstat(&file).map_err(io::Error::from)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "controller output is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn reject_final_symlink(parent: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    let stat = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) => {
            let error = io::Error::from(error);
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(error);
        }
    };
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Symlink => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "controller output path is a symlink",
        )),
        FileType::RegularFile => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "controller output path is not a regular file",
        )),
    }
}

#[cfg(unix)]
fn unlink_at(parent: &File, name: &str) -> io::Result<()> {
    rustix::fs::unlinkat(parent, name, AtFlags::empty()).map_err(io::Error::from)
}

#[cfg(not(unix))]
fn reject_symlink_components(root: &Path, relative: &Path) -> io::Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is not relative",
            ));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "symlink path component",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(unix)]
    #[test]
    fn write_and_append_reject_symlink_parent_and_final_components() {
        use std::os::unix::fs::symlink;

        let root = test_root();
        let outside = root.with_extension("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("linked-dir")).unwrap();
        assert!(write_file(&root, Path::new("linked-dir/file"), b"no").is_err());
        symlink(outside.join("target"), root.join("linked-file")).unwrap();
        assert!(write_file(&root, Path::new("linked-file"), b"no").is_err());
        assert!(append_file(&root, Path::new("linked-file")).is_err());
        assert!(!outside.join("target").exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    fn test_root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "kogen-safe-fs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }
}

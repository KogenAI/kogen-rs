use super::{OUTPUT_TAIL_BYTES, ProcessError, ProcessResult};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

static NEXT_LOG_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(in crate::run) fn ensure_private_dir(path: &Path) -> Result<PathBuf, ProcessError> {
    crate::safe_fs::ensure_directory_path(path)
        .map_err(|source| io_error("create private directory", source))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect private directory", source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ProcessError::InvalidDirectory(path.to_path_buf()));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error("secure private directory", source))?;
    fs::canonicalize(path).map_err(|source| io_error("resolve private directory", source))
}

pub(super) fn unique_log_path(directory: &Path, label: &str) -> Result<PathBuf, ProcessError> {
    for _ in 0..100 {
        let nonce = NEXT_LOG_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = directory.join(format!("{label}-{}-{nonce}.log", std::process::id()));
        let name = path
            .file_name()
            .expect("generated log path has a file name");
        match crate::safe_fs::create_file(directory, Path::new(name), &[]) {
            Ok(()) => return Ok(path),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error("create private log", source)),
        }
    }
    Err(io_error(
        "create unique log",
        io::Error::new(io::ErrorKind::AlreadyExists, "log name collisions"),
    ))
}

pub(super) fn private_file(path: &Path) -> Result<File, ProcessError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .expect("created private log path has a file name");
    crate::safe_fs::append_file(directory, Path::new(name))
        .map_err(|source| io_error("open private log", source))
}

pub(super) fn result_from_log(
    path: &Path,
    exit_status: Option<i32>,
    timed_out: bool,
    unavailable: bool,
    start: Instant,
) -> Result<ProcessResult, ProcessError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .expect("created private log path has a file name");
    let mut file = crate::safe_fs::open_read_file(directory, Path::new(name))
        .map_err(|source| io_error("read process log", source))?;
    let length = file
        .metadata()
        .map_err(|source| io_error("inspect process log", source))?
        .len();
    let tail_length = length.min(OUTPUT_TAIL_BYTES as u64) as usize;
    file.seek(SeekFrom::End(-(tail_length as i64)))
        .map_err(|source| io_error("seek process log", source))?;
    let mut output_tail = Vec::with_capacity(tail_length);
    file.read_to_end(&mut output_tail)
        .map_err(|source| io_error("read process tail", source))?;
    Ok(ProcessResult {
        exit_status,
        timed_out,
        unavailable,
        output_tail,
        log_path: path.to_path_buf(),
        duration_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        sandbox: None,
    })
}

pub(super) fn io_error(operation: &'static str, source: io::Error) -> ProcessError {
    ProcessError::Io { operation, source }
}

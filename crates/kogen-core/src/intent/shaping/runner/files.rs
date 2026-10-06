//! Private shaping scratch and artifact IO helpers.

use crate::error::{CoreError, ErrorClass};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn create_run_dir(home: &Path, slug: &str) -> Result<PathBuf, CoreError> {
    let parent = home.join(".kogen/runs/shaping").join(slug);
    fs::create_dir_all(&parent).map_err(|error| io_error("shape_scratch_unavailable", error))?;
    secure_dir(&parent)?;
    for serial in 0..100 {
        let id = format!(
            "{}-{}-{serial}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let path = parent.join(id);
        match fs::create_dir(&path) {
            Ok(()) => {
                secure_dir(&path)?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error("shape_scratch_unavailable", error)),
        }
    }
    Err(shape_error(
        ErrorClass::Environment,
        "shape_scratch_unavailable",
        "could not allocate a unique run directory",
        crate::ExitCode::Environment,
    ))
}

pub(super) fn secure_dir(path: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_error("shape_scratch_unavailable", error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(shape_error(
                ErrorClass::Environment,
                "shape_scratch_unavailable",
                "run directory is not a real directory",
                crate::ExitCode::Environment,
            ));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error("shape_scratch_unavailable", error))?;
    }
    Ok(())
}

pub(super) fn secure_file(path: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| io_error("shape_scratch_unavailable", error))?;
    }
    Ok(())
}

pub(super) fn remove_stale(path: &Path) -> Result<(), CoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(|error| io_error("shape_output_unavailable", error))
        }
        Ok(_) => Err(shape_error(
            ErrorClass::Environment,
            "shape_output_unavailable",
            format!("{} is not a regular file", path.display()),
            crate::ExitCode::Environment,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("shape_output_unavailable", error)),
    }
}

pub(super) fn write_json(path: &Path, value: &Value) -> Result<(), CoreError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io_error("shape_output_unavailable", error))?;
    fs::write(path, [bytes.as_slice(), b"\n"].concat())
        .map_err(|error| io_error("shape_output_unavailable", error))
}

pub(super) fn request_is_empty(request: &[u8]) -> bool {
    std::str::from_utf8(request).map_or_else(
        |_| request.iter().all(u8::is_ascii_whitespace),
        |text| text.chars().all(char::is_whitespace),
    )
}

pub(super) fn project_error(error: crate::project::ProjectError) -> CoreError {
    shape_error(
        ErrorClass::Environment,
        "project_unavailable",
        error.to_string(),
        crate::ExitCode::Environment,
    )
}

pub(super) fn io_error(reason: &str, error: impl std::fmt::Display) -> CoreError {
    shape_error(
        ErrorClass::Environment,
        reason,
        error.to_string(),
        crate::ExitCode::Environment,
    )
}

pub(super) fn repair_limit_error(
    failure: super::super::validation::ValidationFailure,
    passes: usize,
    calls: usize,
) -> CoreError {
    CoreError::new(
        ErrorClass::Candidate,
        failure.reason,
        format!(
            "Shaper repair limit reached for {} after {passes} pass(es) and {calls} model call(s).\n{}",
            failure.reason, failure.detail
        ),
        crate::ExitCode::Negative,
    )
}

pub(super) fn shape_error(
    class: ErrorClass,
    reason: &str,
    detail: impl Into<String>,
    exit: crate::ExitCode,
) -> CoreError {
    CoreError::new(class, reason, detail, exit)
}

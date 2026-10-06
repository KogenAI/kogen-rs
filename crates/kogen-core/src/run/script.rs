use super::process::{
    ChildEnvironment, ProcessError, ProcessPort, ProcessRequest, ProcessResult, StdinSource,
};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const DEFAULT_SHELL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
pub enum ScriptError {
    Process(ProcessError),
    Io {
        operation: &'static str,
        source: io::Error,
    },
    UnsupportedPlatform,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Process(error) => write!(formatter, "{error}"),
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::UnsupportedPlatform => write!(formatter, "private scripts require a Unix host"),
        }
    }
}

impl std::error::Error for ScriptError {}

/// Writes a script to a mode-0600 file under the private run directory and
/// invokes `sh <path>`. Script bytes never become an argv element.
pub fn run_private_script(
    runner: &dyn ProcessPort,
    script: &[u8],
    cwd: impl AsRef<Path>,
    run_dir: impl AsRef<Path>,
    env: ChildEnvironment,
) -> Result<ProcessResult, ScriptError> {
    #[cfg(not(unix))]
    {
        let _ = (runner, script, cwd, run_dir, env);
        return Err(ScriptError::UnsupportedPlatform);
    }
    #[cfg(unix)]
    {
        let run_dir = run_dir.as_ref();
        let tmp_dir = ensure_private_dir(&run_dir.join("tmp"))?;
        let script_path = create_private_file(&tmp_dir, "shell", ".sh", script)
            .map_err(|source| script_io("write private script", source))?;
        let mut request = ProcessRequest::new("sh", cwd.as_ref(), run_dir);
        request.args.push(script_path.as_os_str().to_owned());
        request.env = env;
        request.timeout = DEFAULT_SHELL_TIMEOUT;
        request.stdin = StdinSource::Null;
        request.log_name = "shell".to_owned();
        let result = runner.run(request).map_err(ScriptError::Process);
        let cleanup = fs::remove_file(script_path);
        if let Err(source) = cleanup
            && source.kind() != io::ErrorKind::NotFound
        {
            return Err(script_io("remove private script", source));
        }
        result
    }
}

#[cfg(unix)]
pub(crate) fn create_private_file(
    directory: &Path,
    prefix: &str,
    suffix: &str,
    contents: &[u8],
) -> io::Result<PathBuf> {
    static NEXT_SCRIPT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..100 {
        let id = NEXT_SCRIPT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = directory.join(format!("{prefix}-{}-{id}{suffix}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(contents)?;
                file.flush()?;
                return Ok(path);
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(source),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "private file name collisions",
    ))
}

#[cfg(unix)]
fn ensure_private_dir(path: &Path) -> Result<PathBuf, ScriptError> {
    fs::create_dir_all(path)
        .map_err(|source| script_io("create private script directory", source))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| script_io("inspect private script directory", source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(script_io(
            "validate private script directory",
            io::Error::new(io::ErrorKind::InvalidInput, "not a directory"),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| script_io("secure private script directory", source))?;
    fs::canonicalize(path).map_err(|source| script_io("resolve private script directory", source))
}

#[cfg(unix)]
fn script_io(operation: &'static str, source: io::Error) -> ScriptError {
    ScriptError::Io { operation, source }
}

#[cfg(test)]
#[path = "script_tests.rs"]
mod tests;

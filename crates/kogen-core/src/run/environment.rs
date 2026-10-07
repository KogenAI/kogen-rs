use super::process::{ChildEnvironment, ProcessError, ProcessPort, ProcessRequest};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub type EnvironmentMap = BTreeMap<OsString, OsString>;

#[derive(Clone, Debug)]
pub struct EnvironmentRequest {
    pub base: EnvironmentMap,
    pub run_dir: PathBuf,
    pub project_root: PathBuf,
    pub workspace: PathBuf,
    pub project: BTreeMap<String, String>,
    /// Executable directories owned by Kogen and excluded from the child's PATH.
    pub kogen_runtime_paths: Vec<PathBuf>,
    /// Stack homes exposed by the selected adapter, restricted to MIX_HOME/HEX_HOME.
    pub stack_home_keys: BTreeSet<String>,
}

impl EnvironmentRequest {
    #[must_use]
    pub fn new(
        base: EnvironmentMap,
        run_dir: impl Into<PathBuf>,
        project_root: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            base,
            run_dir: run_dir.into(),
            project_root: project_root.into(),
            workspace: workspace.into(),
            project: BTreeMap::new(),
            kogen_runtime_paths: Vec::new(),
            stack_home_keys: BTreeSet::new(),
        }
    }
}

#[derive(Debug)]
pub enum EnvironmentError {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Process(ProcessError),
    MiseFailed {
        exit_status: Option<i32>,
        timed_out: bool,
        log_path: PathBuf,
    },
    MiseOutput {
        detail: String,
        log_path: PathBuf,
    },
}

impl fmt::Display for EnvironmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::Process(error) => write!(formatter, "{error}"),
            Self::MiseFailed {
                exit_status,
                timed_out,
                log_path,
            } => write!(
                formatter,
                "mise env failed (status={exit_status:?}, timed_out={timed_out}); log: {}",
                log_path.display()
            ),
            Self::MiseOutput { detail, log_path } => write!(
                formatter,
                "mise env returned invalid JSON ({detail}); log: {}",
                log_path.display()
            ),
        }
    }
}

impl std::error::Error for EnvironmentError {}

/// Builds the exact environment passed to project children. The host map is
/// filtered first, mise values are merged next, and project values win last.
pub fn build_child_environment(
    runner: &dyn ProcessPort,
    request: EnvironmentRequest,
) -> Result<ChildEnvironment, EnvironmentError> {
    let mut child = filtered_base(&request);
    let tmp_dir = request.run_dir.join("tmp");
    ensure_private_dir(&tmp_dir)?;
    child.insert(OsString::from("TMPDIR"), tmp_dir.as_os_str().to_owned());

    if let Some(mise) = find_executable("mise", &child) {
        merge_mise_environment(runner, &request, &mise, &mut child)?;
    }

    for (key, value) in request.project {
        child.insert(OsString::from(key), OsString::from(value));
    }
    Ok(child)
}

#[must_use]
pub fn host_environment() -> EnvironmentMap {
    std::env::vars_os().collect()
}

fn filtered_base(request: &EnvironmentRequest) -> ChildEnvironment {
    let stack_homes = ["MIX_HOME", "HEX_HOME"];
    let runtime = normalized_paths(&runtime_paths(request));
    let mut env = ChildEnvironment::new();
    for (key, value) in &request.base {
        let Some(name) = key.to_str() else { continue };
        if !base_key_allowed(name, &request.stack_home_keys, &stack_homes) {
            continue;
        }
        if name == "PATH" {
            let entries = std::env::split_paths(value)
                .filter(|entry| !runtime.contains(&normalize_path(entry)))
                .collect::<Vec<_>>();
            if let Ok(path) = std::env::join_paths(entries) {
                env.insert(key.clone(), path);
            }
        } else {
            env.insert(key.clone(), value.clone());
        }
    }
    env
}

fn base_key_allowed(name: &str, stack_keys: &BTreeSet<String>, stack_homes: &[&str]) -> bool {
    matches!(
        name,
        "PATH" | "HOME" | "LANG" | "LC_ALL" | "TERM" | "USER" | "SHELL"
    ) || name.to_ascii_lowercase().ends_with("_proxy")
        || name.starts_with("GIT_")
        || name.starts_with("MISE_")
        || stack_homes
            .iter()
            .any(|allowed| *allowed == name && stack_keys.contains(name))
}

fn merge_mise_environment(
    runner: &dyn ProcessPort,
    request: &EnvironmentRequest,
    mise: &Path,
    child: &mut ChildEnvironment,
) -> Result<(), EnvironmentError> {
    let state_dir = request.run_dir.join("mise-state");
    let cache_dir = request.run_dir.join("mise-cache");
    ensure_private_dir(&state_dir)?;
    ensure_private_dir(&cache_dir)?;
    let state_value = state_dir.into_os_string();
    let cache_value = cache_dir.into_os_string();
    let inherited_trusted = child.get(OsStr::new("MISE_TRUSTED_CONFIG_PATHS")).cloned();
    child.insert(OsString::from("MISE_STATE_DIR"), state_value.clone());
    child.insert(OsString::from("MISE_CACHE_DIR"), cache_value.clone());
    let trusted = trusted_paths(
        child,
        inherited_trusted.as_deref(),
        &request.project_root,
        &request.workspace,
    );
    child.insert(OsString::from("MISE_TRUSTED_CONFIG_PATHS"), trusted);

    let mise_dir = mise.parent().unwrap_or_else(|| Path::new("/usr/bin"));
    let mut request_process =
        ProcessRequest::new(mise.as_os_str(), &request.workspace, &request.run_dir);
    request_process.args = [
        OsString::from("env"),
        OsString::from("-C"),
        request.workspace.as_os_str().to_owned(),
        OsString::from("--json"),
        OsString::from("--quiet"),
    ]
    .into();
    request_process.env = child.clone();
    request_process.timeout = Duration::from_secs(30);
    request_process.log_name = "mise-env".to_owned();
    let result = runner
        .run(request_process)
        .map_err(EnvironmentError::Process)?;
    if result.unavailable || result.timed_out || result.exit_status != Some(0) {
        return Err(EnvironmentError::MiseFailed {
            exit_status: result.exit_status,
            timed_out: result.timed_out,
            log_path: result.log_path,
        });
    }
    let output =
        fs::read(&result.log_path).map_err(|source| env_io("read mise environment", source))?;
    if output.len() > 1024 * 1024 {
        return Err(EnvironmentError::MiseOutput {
            detail: "output exceeds 1 MiB".to_owned(),
            log_path: result.log_path,
        });
    }
    let values: BTreeMap<String, String> =
        serde_json::from_slice(&output).map_err(|error| EnvironmentError::MiseOutput {
            detail: error.to_string(),
            log_path: result.log_path.clone(),
        })?;
    for (key, value) in values {
        child.insert(OsString::from(key), OsString::from(value));
    }
    child.insert(OsString::from("MISE_STATE_DIR"), state_value);
    child.insert(OsString::from("MISE_CACHE_DIR"), cache_value);
    let trusted = trusted_paths(
        child,
        inherited_trusted.as_deref(),
        &request.project_root,
        &request.workspace,
    );
    child.insert(OsString::from("MISE_TRUSTED_CONFIG_PATHS"), trusted);

    let existing_path = child.get(OsStr::new("PATH")).cloned().unwrap_or_default();
    let mut path_entries = vec![mise_dir.to_path_buf()];
    let runtime = normalized_paths(&runtime_paths(request));
    path_entries.extend(
        std::env::split_paths(&existing_path)
            .filter(|entry| !runtime.contains(&normalize_path(entry))),
    );
    let joined = std::env::join_paths(path_entries).map_err(|_| EnvironmentError::MiseOutput {
        detail: "mise PATH contains an invalid entry".to_owned(),
        log_path: result.log_path,
    })?;
    child.insert(OsString::from("PATH"), joined);
    Ok(())
}

fn trusted_paths(
    child: &ChildEnvironment,
    inherited: Option<&OsStr>,
    project_root: &Path,
    workspace: &Path,
) -> OsString {
    let mut entries = inherited
        .into_iter()
        .flat_map(std::env::split_paths)
        .collect::<Vec<_>>();
    entries.extend(
        child
            .get(OsStr::new("MISE_TRUSTED_CONFIG_PATHS"))
            .map(std::env::split_paths)
            .into_iter()
            .flatten(),
    );
    entries.sort();
    entries.dedup();
    for path in [project_root, workspace] {
        if !entries.iter().any(|entry| entry == path) {
            entries.push(path.to_path_buf());
        }
    }
    std::env::join_paths(entries).unwrap_or_default()
}

fn find_executable(program: &str, base: &EnvironmentMap) -> Option<PathBuf> {
    find_executable_if(program, base, |_| true)
}

pub(crate) fn find_executable_if(
    program: &str,
    base: &EnvironmentMap,
    is_available: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let path = base.get(OsStr::new("PATH"))?;
    for directory in std::env::split_paths(path) {
        let candidate = directory.join(program);
        if candidate.is_file() && is_executable(&candidate) && is_available(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn normalized_paths(paths: &[PathBuf]) -> BTreeSet<PathBuf> {
    paths.iter().map(|path| normalize_path(path)).collect()
}

fn runtime_paths(request: &EnvironmentRequest) -> Vec<PathBuf> {
    let mut paths = request.kogen_runtime_paths.clone();
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(directory) = current_exe.parent()
    {
        paths.push(directory.to_path_buf());
    }
    paths
}

fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn ensure_private_dir(path: &Path) -> Result<(), EnvironmentError> {
    fs::create_dir_all(path)
        .map_err(|source| env_io("create child environment directory", source))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| env_io("inspect child environment directory", source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(env_io(
            "validate child environment directory",
            io::Error::new(io::ErrorKind::InvalidInput, "not a directory"),
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| env_io("secure child environment directory", source))?;
    Ok(())
}

fn env_io(operation: &'static str, source: io::Error) -> EnvironmentError {
    EnvironmentError::Io { operation, source }
}

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

#[cfg(unix)]
use super::watchdog::{start_parent_watcher, stop_group, stop_watcher};
#[cfg(unix)]
pub(super) use logging::ensure_private_dir;
#[cfg(unix)]
use logging::{io_error, private_file, result_from_log, unique_log_path};

pub const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(120);
pub const OUTPUT_TAIL_BYTES: usize = 16 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(unix)]
mod logging;

pub type ChildEnvironment = BTreeMap<OsString, OsString>;

#[derive(Clone, Debug)]
pub enum StdinSource {
    Null,
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct ProcessRequest {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub run_dir: PathBuf,
    pub env: ChildEnvironment,
    pub timeout: Duration,
    pub stdin: StdinSource,
    /// A short filename label used only for the private log name.
    pub log_name: String,
}

impl ProcessRequest {
    #[must_use]
    pub fn new(
        program: impl Into<OsString>,
        cwd: impl Into<PathBuf>,
        run_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            run_dir: run_dir.into(),
            env: ChildEnvironment::new(),
            timeout: DEFAULT_PROCESS_TIMEOUT,
            stdin: StdinSource::Null,
            log_name: "process".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessResult {
    pub exit_status: Option<i32>,
    pub timed_out: bool,
    pub unavailable: bool,
    pub output_tail: Vec<u8>,
    pub log_path: PathBuf,
    pub duration_ms: u64,
    pub sandbox: Option<SandboxObservation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxStatus {
    Off,
    Confined,
    Unconfined,
}

impl SandboxStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Confined => "confined",
            Self::Unconfined => "unconfined",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxObservation {
    pub status: SandboxStatus,
    pub warning_reason: Option<String>,
}

pub trait ProcessPort: Send + Sync {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessSupervisor;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapturedCommandOutput {
    pub exit_status: Option<i32>,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr_tail: Vec<u8>,
    pub stdout_truncated: bool,
}

/// Run a controller command in a supervised process group while retaining only
/// bounded stdout and stderr. Git commands use this path instead of output().
/// Run a bounded controller effect without a separate parent-death watcher.
/// The process group is still terminated on exit or timeout.
pub(crate) fn run_bounded_command(
    mut command: Command,
    run_dir: &std::path::Path,
    input: Option<&[u8]>,
    timeout: Duration,
    stdout_limit: usize,
) -> Result<CapturedCommandOutput, ProcessError> {
    if timeout.is_zero() {
        return Err(ProcessError::InvalidTimeout);
    }
    ensure_private_dir(run_dir)?;
    let start = Instant::now();
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(|source| ProcessError::Io {
        operation: "spawn supervised controller command",
        source,
    })?;
    let group = child.id();
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdin = input.map(|_| child.stdin.take().expect("stdin was piped"));

    let output = std::thread::scope(|scope| {
        let stdout_reader = scope.spawn(move || read_bounded(stdout, stdout_limit, false));
        let stderr_reader = scope.spawn(move || read_bounded(stderr, OUTPUT_TAIL_BYTES, true));
        let stdin_writer = stdin.map(|mut stdin| {
            let input = input.expect("stdin exists only when input is present");
            scope.spawn(move || stdin.write_all(input))
        });
        let wait = wait_until_deadline(&mut child, start + timeout, Duration::from_millis(2));
        let (status, timed_out) = match wait {
            Ok(result) => result,
            Err(error) => {
                stop_group(group, true);
                let _ = child.wait();
                return Err(error);
            }
        };
        stop_group(group, timed_out);
        let status = match status {
            Some(status) => status,
            None => child
                .wait()
                .map_err(|source| io_error("wait for command", source))?,
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| io_error("read command stdout", io::Error::other("reader panicked")))?
            .map_err(|source| io_error("read command stdout", source))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| io_error("read command stderr", io::Error::other("reader panicked")))?
            .map_err(|source| io_error("read command stderr", source))?;
        if let Some(writer) = stdin_writer {
            writer
                .join()
                .map_err(|_| io_error("write command stdin", io::Error::other("writer panicked")))?
                .map_err(|source| io_error("write command stdin", source))?;
        }
        Ok((status, timed_out, stdout, stderr))
    });
    let (status, timed_out, (stdout, stdout_truncated), (stderr_tail, _)) = output?;
    Ok(CapturedCommandOutput {
        exit_status: status_code(status),
        timed_out,
        stdout,
        stderr_tail,
        stdout_truncated,
    })
}

fn read_bounded<R: Read>(
    mut reader: R,
    limit: usize,
    keep_tail: bool,
) -> io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::with_capacity(limit.min(OUTPUT_TAIL_BYTES));
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok((retained, truncated));
        }
        let chunk = &buffer[..count];
        if keep_tail {
            if chunk.len() >= limit {
                retained.clear();
                retained.extend_from_slice(&chunk[chunk.len() - limit..]);
                truncated = true;
            } else {
                let excess = retained
                    .len()
                    .saturating_add(chunk.len())
                    .saturating_sub(limit);
                if excess > 0 {
                    retained.drain(..excess);
                    truncated = true;
                }
                retained.extend_from_slice(chunk);
            }
        } else {
            let room = limit.saturating_sub(retained.len());
            retained.extend_from_slice(&chunk[..chunk.len().min(room)]);
            if count > room {
                truncated = true;
            }
        }
    }
}

impl ProcessPort for ProcessSupervisor {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        let original = request.clone();
        let result = self.run_request(request);
        if result.is_err() {
            super::diagnostics::record_process(&original, serde_json::Value::Null, &result)?;
        }
        result
    }
}

impl ProcessSupervisor {
    fn run_request(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        validate(&request)?;
        #[cfg(unix)]
        {
            run_unix(request)
        }
        #[cfg(not(unix))]
        {
            let _ = request;
            Err(ProcessError::UnsupportedPlatform)
        }
    }
}

#[derive(Debug)]
pub enum ProcessError {
    EmptyProgram,
    ArgumentTooLong {
        index: usize,
        bytes: usize,
    },
    InvalidTimeout,
    InvalidLogName,
    InvalidDirectory(PathBuf),
    SandboxSetup(String),
    SandboxIntegrityRequired,
    SandboxIntegrityRead(String),
    SandboxIntegrityChanged,
    Io {
        operation: &'static str,
        source: io::Error,
    },
    UnsupportedPlatform,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProgram => write!(formatter, "program must not be empty"),
            Self::ArgumentTooLong { index, bytes } => {
                write!(
                    formatter,
                    "argv element {index} is {bytes} bytes; maximum is 4096"
                )
            }
            Self::InvalidTimeout => write!(formatter, "timeout must be greater than zero"),
            Self::InvalidLogName => write!(
                formatter,
                "log name must contain only ASCII letters, digits, '-' or '_'"
            ),
            Self::InvalidDirectory(path) => write!(
                formatter,
                "run directory is not a private directory: {}",
                path.display()
            ),
            Self::SandboxSetup(detail) => write!(formatter, "sandbox setup failed: {detail}"),
            Self::SandboxIntegrityRequired => {
                write!(
                    formatter,
                    "unconfined execution requires an integrity snapshot port"
                )
            }
            Self::SandboxIntegrityRead(detail) => {
                write!(
                    formatter,
                    "could not verify checkout/origin integrity: {detail}"
                )
            }
            Self::SandboxIntegrityChanged => {
                write!(
                    formatter,
                    "unconfined execution changed checkout/origin state"
                )
            }
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::UnsupportedPlatform => {
                write!(formatter, "process groups are supported only on Unix hosts")
            }
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn validate(request: &ProcessRequest) -> Result<(), ProcessError> {
    if request.program.is_empty() {
        return Err(ProcessError::EmptyProgram);
    }
    if request.timeout.is_zero() {
        return Err(ProcessError::InvalidTimeout);
    }
    if request.log_name.is_empty()
        || !request
            .log_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(ProcessError::InvalidLogName);
    }
    for (index, argument) in std::iter::once(request.program.as_os_str())
        .chain(request.args.iter().map(OsString::as_os_str))
        .enumerate()
    {
        let bytes = argv_bytes(argument);
        if bytes > 4096 {
            return Err(ProcessError::ArgumentTooLong { index, bytes });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn argv_bytes(argument: &OsStr) -> usize {
    argument.as_bytes().len()
}

#[cfg(not(unix))]
fn argv_bytes(argument: &OsStr) -> usize {
    argument.to_string_lossy().len()
}

#[cfg(unix)]
fn run_unix(request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
    let run_dir = ensure_private_dir(&request.run_dir)?;
    let logs_dir = ensure_private_dir(&run_dir.join("logs"))?;
    let log_path = unique_log_path(&logs_dir, &request.log_name)?;
    let mut log = private_file(&log_path)?;
    let start = Instant::now();

    let mut command = Command::new(&request.program);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .env_clear()
        .envs(&request.env)
        .stdin(stdin_for(&request.stdin)?)
        .stdout(Stdio::from(
            log.try_clone()
                .map_err(|source| io_error("clone log", source))?,
        ))
        .stderr(Stdio::from(
            log.try_clone()
                .map_err(|source| io_error("clone log", source))?,
        ))
        .process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            let status = if source.kind() == io::ErrorKind::PermissionDenied {
                126
            } else {
                127
            };
            writeln!(log, "{}", source)
                .map_err(|source| io_error("write spawn error to log", source))?;
            log.flush()
                .map_err(|source| io_error("flush log", source))?;
            return result_from_log(&log_path, Some(status), false, true, start);
        }
        Err(source) => return Err(io_error("spawn child", source)),
    };

    let target_group = child.id();
    let watcher = match start_parent_watcher(&run_dir, target_group) {
        Ok(watcher) => watcher,
        Err(error) => {
            stop_group(target_group, true);
            let _ = child.wait();
            return Err(error);
        }
    };

    let deadline = start + request.timeout;
    let (status, timed_out) = match wait_until_deadline(&mut child, deadline, POLL_INTERVAL) {
        Ok(result) => result,
        Err(error) => {
            stop_group(target_group, true);
            let _ = child.wait();
            stop_watcher(watcher);
            return Err(error);
        }
    };
    stop_group(target_group, timed_out);
    let status = match status {
        Some(status) => Ok(status),
        None => child.wait(),
    };
    stop_watcher(watcher);
    let status = status.map_err(|source| io_error("wait for child", source))?;
    log.flush()
        .map_err(|source| io_error("flush log", source))?;
    result_from_log(&log_path, status_code(status), timed_out, false, start)
}

#[cfg(unix)]
fn wait_until_deadline(
    child: &mut Child,
    deadline: Instant,
    poll_interval: Duration,
) -> Result<(Option<ExitStatus>, bool), ProcessError> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|source| io_error("poll child", source))?
        {
            return Ok((Some(status), false));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok((None, true));
        }
        thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
    }
}

#[cfg(unix)]
fn stdin_for(source: &StdinSource) -> Result<Stdio, ProcessError> {
    match source {
        StdinSource::Null => Ok(Stdio::null()),
        StdinSource::File(path) => File::open(path)
            .map(Stdio::from)
            .map_err(|source| io_error("open child stdin", source)),
    }
}

#[cfg(unix)]
fn status_code(status: ExitStatus) -> Option<i32> {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;

//! Command acceptance adapter and strict JSONL ledger handling.
//!
//! The adapter owns acceptance-report interpretation. Gate callers provide
//! the shared child-process port and the tree snapshot implementation used by
//! checks so mutation detection has the same tree semantics everywhere.

use crate::run::{ChildEnvironment, ProcessError, ProcessPort, ProcessRequest, ProcessResult};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod report;

pub use report::{
    LedgerReadError, LedgerRow, LedgerStatus, parse_ledger_report, read_ledger_report,
};

#[cfg(not(unix))]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[derive(Clone, Debug)]
pub struct CommandAcceptanceRequest {
    pub slug: String,
    /// The first entry is the executable; exact `{path}` arguments are replaced.
    pub command: Vec<OsString>,
    pub candidate_path: PathBuf,
    pub workdir: PathBuf,
    pub run_dir: PathBuf,
    pub report_path: PathBuf,
    pub env: ChildEnvironment,
    pub timeout: Duration,
    pub expected_items: BTreeSet<String>,
    /// Set by adapters with an explicit unavailable signal in their own protocol.
    pub adapter_unavailable: bool,
}

pub trait TreeSnapshotPort: Send + Sync {
    fn snapshot(&self, workdir: &Path) -> Result<String, String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcceptanceFailure {
    ToolMissing,
    AcceptanceCompileFailed,
    NoTaggedTests,
    LedgerInvalid { line: Option<usize>, detail: String },
    AcceptanceTimeout,
    TreeMutated,
    Suite,
}

#[derive(Clone, Debug)]
pub struct CommandAcceptanceResult {
    pub process: ProcessResult,
    pub rows: Vec<LedgerRow>,
    /// Keys are A<n> identifiers. An item passes only when it has rows and all pass.
    pub item_pass: BTreeMap<String, bool>,
    pub failures: Vec<AcceptanceFailure>,
}

#[derive(Debug)]
pub enum AcceptanceRunError {
    InvalidCommand,
    InvalidReportPath(PathBuf),
    TreeSnapshot { phase: &'static str, detail: String },
    Process(ProcessError),
    ReportSetup { path: PathBuf, source: io::Error },
}

impl fmt::Display for AcceptanceRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommand => write!(formatter, "command adapter requires a nonempty argv"),
            Self::InvalidReportPath(path) => write!(
                formatter,
                "ledger report must be under the run directory: {}",
                path.display()
            ),
            Self::TreeSnapshot { phase, detail } => {
                write!(formatter, "tree snapshot {phase} failed: {detail}")
            }
            Self::Process(error) => write!(formatter, "{error}"),
            Self::ReportSetup { path, source } => write!(
                formatter,
                "prepare ledger report {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for AcceptanceRunError {}

pub fn run_command_acceptance(
    runner: &dyn ProcessPort,
    tree: &dyn TreeSnapshotPort,
    mut request: CommandAcceptanceRequest,
) -> Result<CommandAcceptanceResult, AcceptanceRunError> {
    if request.command.is_empty() || request.command[0].is_empty() {
        return Err(AcceptanceRunError::InvalidCommand);
    }
    let (report_root, report_relative, report_path) =
        validated_report_path(&request.run_dir, &request.report_path)?;
    request.report_path = report_path;
    match fs::symlink_metadata(&request.report_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(AcceptanceRunError::InvalidReportPath(request.report_path));
            }
            crate::safe_fs::remove_file(&report_root, &report_relative).map_err(|source| {
                AcceptanceRunError::ReportSetup {
                    path: request.report_path.clone(),
                    source,
                }
            })?;
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(AcceptanceRunError::ReportSetup {
                path: request.report_path.clone(),
                source,
            });
        }
    }

    let before =
        tree.snapshot(&request.workdir)
            .map_err(|detail| AcceptanceRunError::TreeSnapshot {
                phase: "before",
                detail,
            })?;
    let program = replace_candidate_path(request.command.remove(0), &request.candidate_path);
    let args = request
        .command
        .into_iter()
        .map(|arg| replace_candidate_path(arg, &request.candidate_path))
        .collect();
    request.env.insert(
        OsString::from("KOGEN_LEDGER_REPORT"),
        request.report_path.as_os_str().to_owned(),
    );
    request.env.insert(
        OsString::from("KOGEN_INTENT_SLUG"),
        OsString::from(&request.slug),
    );
    let mut process_request = ProcessRequest::new(program, &request.workdir, &request.run_dir);
    process_request.args = args;
    process_request.env = request.env;
    process_request.timeout = request.timeout;
    process_request.log_name = "acceptance".to_owned();
    let process = runner.run(process_request);
    let after =
        tree.snapshot(&request.workdir)
            .map_err(|detail| AcceptanceRunError::TreeSnapshot {
                phase: "after",
                detail,
            })?;
    let process = process.map_err(AcceptanceRunError::Process)?;
    let report = crate::safe_fs::read_file(&report_root, &report_relative)
        .map_err(|source| LedgerReadError::Io(source.to_string()))
        .and_then(|bytes| parse_ledger_report(&bytes));
    Ok(assess_command_result(
        request.slug,
        request.expected_items,
        process,
        report,
        request.adapter_unavailable,
        before != after,
    ))
}

fn validated_report_path(
    run_dir: &Path,
    report_path: &Path,
) -> Result<(PathBuf, PathBuf, PathBuf), AcceptanceRunError> {
    if !report_path.is_absolute() {
        return Err(AcceptanceRunError::InvalidReportPath(
            report_path.to_path_buf(),
        ));
    }
    let root_metadata =
        fs::symlink_metadata(run_dir).map_err(|source| AcceptanceRunError::ReportSetup {
            path: run_dir.to_path_buf(),
            source,
        })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(AcceptanceRunError::InvalidReportPath(run_dir.to_path_buf()));
    }
    let run_root = fs::canonicalize(run_dir).map_err(|source| AcceptanceRunError::ReportSetup {
        path: run_dir.to_path_buf(),
        source,
    })?;
    // Keep the caller's path spelling for the containment check. On macOS,
    // temp_dir() commonly uses /var while canonical paths use /private/var.
    let report_relative = report_path
        .strip_prefix(run_dir)
        .map_err(|_| AcceptanceRunError::InvalidReportPath(report_path.to_path_buf()))?
        .to_path_buf();
    let parent_relative = report_relative.parent().unwrap_or_else(|| Path::new(""));
    crate::safe_fs::ensure_dir(&run_root, parent_relative).map_err(|source| {
        AcceptanceRunError::ReportSetup {
            path: run_root.join(parent_relative),
            source,
        }
    })?;
    crate::safe_fs::validate_write(&run_root, &report_relative)
        .map_err(|_| AcceptanceRunError::InvalidReportPath(report_path.to_path_buf()))?;
    if report_relative.file_name().is_none() {
        return Err(AcceptanceRunError::InvalidReportPath(
            report_path.to_path_buf(),
        ));
    }
    Ok((
        run_root.clone(),
        report_relative.clone(),
        run_root.join(report_relative),
    ))
}

fn assess_command_result(
    slug: String,
    expected_items: BTreeSet<String>,
    process: ProcessResult,
    report: Result<Vec<LedgerRow>, LedgerReadError>,
    adapter_unavailable: bool,
    tree_mutated: bool,
) -> CommandAcceptanceResult {
    let runner_unavailable = adapter_unavailable
        || process.unavailable
        || matches!(process.exit_status, Some(126 | 127));
    let mut failures = Vec::new();
    if process.timed_out {
        failures.push(AcceptanceFailure::AcceptanceTimeout);
    }
    if tree_mutated {
        failures.push(AcceptanceFailure::TreeMutated);
    }

    let rows = match report {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) | Err(LedgerReadError::Io(_)) if runner_unavailable => {
            failures.push(AcceptanceFailure::ToolMissing);
            Vec::new()
        }
        Err(LedgerReadError::MalformedLine { line, detail }) if runner_unavailable => {
            let _ = (line, detail);
            failures.push(AcceptanceFailure::ToolMissing);
            Vec::new()
        }
        Err(LedgerReadError::InvalidUtf8 | LedgerReadError::UnsafeReport(_))
            if runner_unavailable =>
        {
            failures.push(AcceptanceFailure::ToolMissing);
            Vec::new()
        }
        Ok(_) | Err(LedgerReadError::Io(_)) => {
            if process.exit_status.is_some_and(|status| status != 0) {
                failures.push(AcceptanceFailure::AcceptanceCompileFailed);
            } else {
                failures.push(AcceptanceFailure::NoTaggedTests);
            }
            Vec::new()
        }
        Err(error) => {
            let (line, detail) = match error {
                LedgerReadError::MalformedLine { line, detail } => (Some(line), detail),
                other => (None, other.to_string()),
            };
            failures.push(AcceptanceFailure::LedgerInvalid { line, detail });
            Vec::new()
        }
    };

    let mut item_pass = BTreeMap::new();
    let prefix = format!("{slug}/");
    let mut suite_failure = false;
    for row in &rows {
        if let Some(item) = row.tag.strip_prefix(&prefix)
            && !expected_items.contains(item)
        {
            suite_failure = true;
        }
    }
    for item in &expected_items {
        let item_rows = rows
            .iter()
            .filter(|row| row.tag == format!("{slug}/{item}"))
            .collect::<Vec<_>>();
        item_pass.insert(
            item.clone(),
            !item_rows.is_empty()
                && item_rows
                    .iter()
                    .all(|row| row.status == LedgerStatus::Passed),
        );
    }
    if suite_failure
        || (process.exit_status.is_some_and(|status| status != 0)
            && !expected_items.is_empty()
            && item_pass.values().all(|passed| *passed))
    {
        failures.push(AcceptanceFailure::Suite);
    }
    CommandAcceptanceResult {
        process,
        rows,
        item_pass,
        failures,
    }
}

fn replace_candidate_path(argument: OsString, candidate_path: &Path) -> OsString {
    #[cfg(unix)]
    {
        let source = argument.as_os_str().as_bytes();
        let needle = b"{path}";
        if !source.windows(needle.len()).any(|part| part == needle) {
            return argument;
        }
        let replacement = candidate_path.as_os_str().as_bytes();
        let mut result = Vec::with_capacity(source.len() + replacement.len());
        let mut start = 0;
        while let Some(offset) = source[start..]
            .windows(needle.len())
            .position(|part| part == needle)
        {
            let found = start + offset;
            result.extend_from_slice(&source[start..found]);
            result.extend_from_slice(replacement);
            start = found + needle.len();
        }
        result.extend_from_slice(&source[start..]);
        OsString::from_vec(result)
    }
    #[cfg(not(unix))]
    {
        if argument == OsStr::new("{path}") {
            candidate_path.as_os_str().to_owned()
        } else {
            argument
        }
    }
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;

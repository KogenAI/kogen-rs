use super::workspace::WorkspaceTree;
use crate::project::ProjectResolution;
use crate::run::{ChildEnvironment, ProcessError, ProcessPort, ProcessRequest, ProcessResult};
use serde_yaml::Value;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckCommand {
    pub name: String,
    pub argv: Vec<OsString>,
    pub timeout: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckConfigError {
    pub field: String,
    pub detail: String,
}

impl fmt::Display for CheckConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.detail)
    }
}

impl std::error::Error for CheckConfigError {}

/// Reads validated `fix` or `checks` commands from the project config.
pub fn configured_commands(
    project: &ProjectResolution,
    field: &str,
) -> Result<Vec<CheckCommand>, CheckConfigError> {
    if !matches!(field, "checks" | "fix") {
        return Err(config_error(
            field,
            "only `fix` and `checks` are executable gate lists",
        ));
    }
    let Some(config) = project.config.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(root) = config.raw.as_mapping() else {
        return Err(config_error(field, "project config is not a map"));
    };
    let Some(rows) = root
        .get(Value::String(field.to_owned()))
        .and_then(Value::as_sequence)
    else {
        return Ok(Vec::new());
    };
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let row = row
                .as_mapping()
                .ok_or_else(|| config_error(field, &format!("entry {} is not a map", index + 1)))?;
            let get = |name: &str| row.get(Value::String(name.to_owned()));
            let name = get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| config_error(field, &format!("entry {} has no name", index + 1)))?;
            let argv = get("argv")
                .and_then(Value::as_sequence)
                .and_then(|args| args.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                .filter(|args| !args.is_empty())
                .ok_or_else(|| {
                    config_error(field, &format!("entry {} has invalid argv", index + 1))
                })?;
            let timeout_ms = get("timeout_ms")
                .and_then(Value::as_u64)
                .filter(|timeout| *timeout > 0)
                .ok_or_else(|| {
                    config_error(field, &format!("entry {} has invalid timeout", index + 1))
                })?;
            Ok(CheckCommand {
                name: name.to_owned(),
                argv: argv.iter().map(OsString::from).collect(),
                timeout: Duration::from_millis(timeout_ms),
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckStatus {
    Green,
    Red,
    Unavailable,
    Timeout,
    Mutating,
}

impl CheckStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Red => "red",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Mutating => "mutating",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct CheckFinding {
    pub path: String,
    pub rule: String,
    pub symbol: String,
    pub message: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl CheckFinding {
    fn identity(&self) -> (&str, &str, &str) {
        (&self.path, &self.rule, &self.symbol)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckBaseline {
    pub name: String,
    pub status: CheckStatus,
    pub exit_status: Option<i32>,
    pub findings: Vec<CheckFinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckResult {
    pub name: String,
    pub program: String,
    pub status: CheckStatus,
    pub exit_status: Option<i32>,
    pub findings: Vec<CheckFinding>,
    pub changed_paths: Vec<String>,
    pub log_path: PathBuf,
    pub duration_ms: u64,
    pub timeout: Duration,
    pub excused: bool,
}

impl CheckResult {
    #[must_use]
    pub fn blocks_gate(&self) -> bool {
        self.status != CheckStatus::Green && !self.excused
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixResult {
    pub name: String,
    pub exit_status: Option<i32>,
    pub timed_out: bool,
    pub unavailable: bool,
    pub log_path: PathBuf,
    pub duration_ms: u64,
}

impl FixResult {
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.timed_out && !self.unavailable && self.exit_status == Some(0)
    }
}

#[derive(Debug)]
pub enum CheckRunError {
    EmptyCommand(String),
    Process(ProcessError),
    Snapshot(String),
    Restore(String),
}

impl fmt::Display for CheckRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand(name) => write!(formatter, "check {name} has an empty argv"),
            Self::Process(error) => write!(formatter, "{error}"),
            Self::Snapshot(error) => write!(formatter, "snapshot workspace: {error}"),
            Self::Restore(error) => write!(formatter, "restore workspace after check: {error}"),
        }
    }
}

impl std::error::Error for CheckRunError {}

/// Runs one configured check, compares full Git tree identity before and after,
/// and restores every write before returning a mutating result.
pub fn run_check(
    runner: &dyn ProcessPort,
    command: &CheckCommand,
    workdir: &Path,
    run_dir: &Path,
    env: &ChildEnvironment,
) -> Result<CheckResult, CheckRunError> {
    run_check_excluding(runner, command, workdir, run_dir, env, &[])
}

pub(crate) fn run_check_excluding(
    runner: &dyn ProcessPort,
    command: &CheckCommand,
    workdir: &Path,
    run_dir: &Path,
    env: &ChildEnvironment,
    excluded_paths: &[PathBuf],
) -> Result<CheckResult, CheckRunError> {
    let Some((program, args)) = command.argv.split_first() else {
        return Err(CheckRunError::EmptyCommand(command.name.clone()));
    };
    let before = WorkspaceTree::capture_excluding(workdir, excluded_paths)
        .map_err(|error| CheckRunError::Snapshot(error.to_string()))?;
    let mut request = ProcessRequest::new(program.clone(), workdir, run_dir);
    request.args = args.to_vec();
    request.env = env.clone();
    request.timeout = command.timeout;
    request.log_name = log_name(&command.name);
    let process = runner.run(request);
    let after = WorkspaceTree::capture_excluding(workdir, excluded_paths)
        .map_err(|error| CheckRunError::Snapshot(error.to_string()))?;
    let changed_paths = before.changed_paths(&after);
    if !changed_paths.is_empty() {
        before
            .restore()
            .map_err(|error| CheckRunError::Restore(error.to_string()))?;
    }
    let process = process.map_err(CheckRunError::Process)?;
    Ok(check_result(command, process, changed_paths, workdir))
}

pub(crate) fn run_fix(
    runner: &dyn ProcessPort,
    command: &CheckCommand,
    workdir: &Path,
    run_dir: &Path,
    env: &ChildEnvironment,
) -> Result<FixResult, CheckRunError> {
    let Some((program, args)) = command.argv.split_first() else {
        return Err(CheckRunError::EmptyCommand(command.name.clone()));
    };
    let mut request = ProcessRequest::new(program.clone(), workdir, run_dir);
    request.args = args.to_vec();
    request.env = env.clone();
    request.timeout = command.timeout;
    request.log_name = format!("fix-{}", log_name(&command.name));
    let result = runner.run(request).map_err(CheckRunError::Process)?;
    Ok(FixResult {
        name: command.name.clone(),
        exit_status: result.exit_status,
        timed_out: result.timed_out,
        unavailable: result.unavailable
            || result
                .exit_status
                .is_none_or(|code| code == 126 || code == 127),
        log_path: result.log_path,
        duration_ms: result.duration_ms,
    })
}

pub(crate) fn check_result(
    command: &CheckCommand,
    process: ProcessResult,
    changed_paths: Vec<String>,
    workdir: &Path,
) -> CheckResult {
    let output = std::fs::read(&process.log_path).unwrap_or_else(|_| process.output_tail.clone());
    let findings = parse_check_findings(&output, workdir);
    let unavailable = process.unavailable
        || process
            .exit_status
            .is_none_or(|code| code == 126 || code == 127);
    let status = if !changed_paths.is_empty() {
        CheckStatus::Mutating
    } else if process.timed_out {
        CheckStatus::Timeout
    } else if unavailable {
        CheckStatus::Unavailable
    } else if process.exit_status != Some(0) {
        CheckStatus::Red
    } else {
        CheckStatus::Green
    };
    CheckResult {
        name: command.name.clone(),
        program: command
            .argv
            .first()
            .map(|program| program.to_string_lossy().into_owned())
            .unwrap_or_default(),
        status,
        exit_status: process.exit_status,
        findings,
        changed_paths,
        log_path: process.log_path,
        duration_ms: process.duration_ms,
        timeout: command.timeout,
        excused: false,
    }
}

/// A check can be excused only when its approved baseline was red-like and its
/// current result has the same status and no new finding identities.
#[must_use]
pub fn is_excused(baseline: &CheckBaseline, current: &CheckResult) -> bool {
    if baseline.status == CheckStatus::Green || baseline.status != current.status {
        return false;
    }
    let baseline_ids = identities(&baseline.findings);
    let current_ids = identities(&current.findings);
    if !baseline_ids.is_empty() && !current_ids.is_empty() {
        current_ids.is_subset(&baseline_ids)
    } else {
        baseline.exit_status == current.exit_status
    }
}

fn identities(findings: &[CheckFinding]) -> BTreeSet<(&str, &str, &str)> {
    findings.iter().map(CheckFinding::identity).collect()
}

/// Use the same finding identities for approval baselines and candidates.
pub(crate) fn parse_check_findings(output: &[u8], workdir: &Path) -> Vec<CheckFinding> {
    let mut findings = parse_gnu_findings(output);
    if workdir.join("mix.exs").is_file() {
        findings.extend(super::adapters::exunit::parse_findings(output, workdir));
    }
    findings
}

fn parse_gnu_findings(output: &[u8]) -> Vec<CheckFinding> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(parse_gnu_finding)
        .collect()
}

fn parse_gnu_finding(line: &str) -> Option<CheckFinding> {
    let mut parts = line.splitn(3, ':');
    let path = parts.next()?;
    let line_number = parts.next()?.parse::<u32>().ok()?;
    let tail = parts.next()?.trim_start();
    let (column, tail) = match tail.split_once(':') {
        Some((column, rest)) if column.parse::<u32>().is_ok() => {
            (column.parse().ok(), rest.trim_start())
        }
        _ => (None, tail),
    };
    let (_, tail) = ["error: ", "warning: ", "note: "]
        .into_iter()
        .find_map(|severity| tail.strip_prefix(severity).map(|rest| (severity, rest)))?;
    let tail = tail.strip_prefix('[')?;
    let (rule, details) = tail.split_once("] ")?;
    let (symbol, message) = if is_test_rule(rule) {
        details
            .split_once(": ")
            .map_or(("", details), |(symbol, message)| (symbol, message))
    } else {
        ("", details)
    };
    Some(CheckFinding {
        path: path.to_owned(),
        rule: rule.to_owned(),
        symbol: symbol.to_owned(),
        message: message.to_owned(),
        line: Some(line_number),
        column,
    })
}

pub(crate) fn is_test_rule(rule: &str) -> bool {
    matches!(
        rule.split('/').next().unwrap_or_default(),
        "test" | "kt" | "exunit" | "minitest" | "rails" | "acceptance"
    )
}

fn config_error(field: &str, detail: &str) -> CheckConfigError {
    CheckConfigError {
        field: field.to_owned(),
        detail: detail.to_owned(),
    }
}

fn log_name(name: &str) -> String {
    let safe = name
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                char::from(byte)
            } else {
                '-'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        "check".to_owned()
    } else {
        safe.chars().take(72).collect()
    }
}

#[cfg(test)]
#[path = "checks_tests.rs"]
mod tests;

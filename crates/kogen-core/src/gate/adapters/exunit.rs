//! ExUnit command, ledger formatter, setup seeds, and finding parsers.

mod findings;

use crate::gate::CheckCommand;
use crate::gate::ledger::{
    AcceptanceFailure, AcceptanceRunError, CommandAcceptanceRequest, CommandAcceptanceResult,
    TreeSnapshotPort, run_command_acceptance,
};
use crate::run::ProcessPort;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const ACCEPTANCE_EXTENSION: &str = "_test.exs";
pub const CANDIDATE_DIRECTORY: &str = "test/acceptance";
pub const SETUP_SEEDS: &[&str] = &["deps", "_build"];

#[derive(Debug)]
pub enum ExUnitAdapterError {
    InvalidRunDirectory(PathBuf),
    FormatterExists(PathBuf),
    FormatterWrite { path: PathBuf, source: io::Error },
    NonUtf8FormatterPath(PathBuf),
    Acceptance(AcceptanceRunError),
}

impl fmt::Display for ExUnitAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRunDirectory(path) => {
                write!(
                    formatter,
                    "run directory is not a real directory: {}",
                    path.display()
                )
            }
            Self::FormatterExists(path) => {
                write!(
                    formatter,
                    "ledger formatter already exists: {}",
                    path.display()
                )
            }
            Self::FormatterWrite { path, source } => {
                write!(
                    formatter,
                    "write ledger formatter {}: {source}",
                    path.display()
                )
            }
            Self::NonUtf8FormatterPath(path) => {
                write!(
                    formatter,
                    "ledger formatter path is not UTF-8: {}",
                    path.display()
                )
            }
            Self::Acceptance(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ExUnitAdapterError {}

pub fn source_path(slug: &str) -> PathBuf {
    PathBuf::from(format!(".kogen/acceptance/{slug}{ACCEPTANCE_EXTENSION}"))
}

pub fn candidate_path(slug: &str) -> PathBuf {
    PathBuf::from(format!(
        "{CANDIDATE_DIRECTORY}/{slug}{ACCEPTANCE_EXTENSION}"
    ))
}

pub fn setup_seeds() -> &'static [&'static str] {
    SETUP_SEEDS
}

pub fn formatter_source() -> &'static str {
    include_str!("exunit/ledger_formatter.ex")
}

/// Writes the formatter beside the run ledger, never into the project tree.
pub fn write_formatter(run_dir: &Path) -> Result<PathBuf, ExUnitAdapterError> {
    let metadata =
        fs::symlink_metadata(run_dir).map_err(|source| ExUnitAdapterError::FormatterWrite {
            path: run_dir.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ExUnitAdapterError::InvalidRunDirectory(
            run_dir.to_path_buf(),
        ));
    }
    let formatter_path = run_dir.join("ledger_formatter.ex");
    match fs::symlink_metadata(&formatter_path) {
        Ok(metadata)
            if !metadata.file_type().is_symlink()
                && metadata.is_file()
                && fs::read(&formatter_path).ok()
                    == Some(formatter_source().as_bytes().to_vec()) =>
        {
            return Ok(formatter_path);
        }
        Ok(_) => return Err(ExUnitAdapterError::FormatterExists(formatter_path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ExUnitAdapterError::FormatterWrite {
                path: formatter_path,
                source,
            });
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file =
        options
            .open(&formatter_path)
            .map_err(|source| ExUnitAdapterError::FormatterWrite {
                path: formatter_path.clone(),
                source,
            })?;
    file.write_all(formatter_source().as_bytes())
        .map_err(|source| ExUnitAdapterError::FormatterWrite {
            path: formatter_path.clone(),
            source,
        })?;
    Ok(formatter_path)
}

/// Builds the ExUnit argv. The shared command adapter substitutes {path}.
pub fn runner_command(
    formatter_path: &Path,
    use_mise: bool,
) -> Result<Vec<OsString>, ExUnitAdapterError> {
    let formatter_path = formatter_path
        .to_str()
        .ok_or_else(|| ExUnitAdapterError::NonUtf8FormatterPath(formatter_path.to_path_buf()))?;
    let mut command = Vec::new();
    if use_mise {
        command.extend(words(["mise", "exec", "--"]));
    }
    command.extend(words([
        "elixir",
        "-e",
        &format!(
            "Code.require_file({}); Code.ensure_loaded!(KogenLedgerFormatter)",
            elixir_string(formatter_path)
        ),
        "-S",
        "mix",
        "test",
        "--formatter",
        "KogenLedgerFormatter",
        "--formatter",
        "ExUnit.CLIFormatter",
        "{path}",
    ]));
    Ok(command)
}

/// Selects the project format check or the default formatter for Elixir files.
pub fn formatter(checks: &[CheckCommand], file: &Path) -> Option<Vec<OsString>> {
    if !is_elixir_file(file) {
        return None;
    }
    if let Some(check) = checks.iter().find(|check| {
        check.argv.iter().any(|argument| argument == "format")
            && check
                .argv
                .iter()
                .any(|argument| argument == "--check-formatted")
    }) {
        return Some(
            check
                .argv
                .iter()
                .filter(|argument| argument.as_os_str() != "--check-formatted")
                .cloned()
                .collect(),
        );
    }
    let mut command = words(["mix", "format"]);
    command.push(file.as_os_str().to_owned());
    Some(command)
}

/// A missing runtime mentioned near the start of the log is an adapter signal.
pub fn unavailable(log: &[u8]) -> bool {
    String::from_utf8_lossy(log)
        .lines()
        .take(20)
        .any(missing_runtime_line)
}

/// Runs ExUnit through the shared ledger and applies its log-based unavailable signal.
pub fn run_acceptance(
    runner: &dyn ProcessPort,
    tree: &dyn TreeSnapshotPort,
    mut request: CommandAcceptanceRequest,
    use_mise: bool,
) -> Result<CommandAcceptanceResult, ExUnitAdapterError> {
    let formatter_path = write_formatter(&request.run_dir)?;
    request.command = runner_command(&formatter_path, use_mise)?;
    let mut result =
        run_command_acceptance(runner, tree, request).map_err(ExUnitAdapterError::Acceptance)?;
    let log =
        fs::read(&result.process.log_path).unwrap_or_else(|_| result.process.output_tail.clone());
    if unavailable(&log) {
        result.process.unavailable = true;
        if result.rows.is_empty() {
            result.failures.retain(|failure| {
                !matches!(
                    failure,
                    AcceptanceFailure::AcceptanceCompileFailed
                        | AcceptanceFailure::NoTaggedTests
                        | AcceptanceFailure::LedgerInvalid { .. }
                )
            });
            if !result.failures.contains(&AcceptanceFailure::ToolMissing) {
                result.failures.push(AcceptanceFailure::ToolMissing);
            }
        }
    }
    Ok(result)
}

pub fn parse_findings(output: &[u8], workdir: &Path) -> Vec<crate::gate::CheckFinding> {
    findings::parse(output, workdir)
}

fn is_elixir_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("ex" | "exs")
    )
}

fn missing_runtime_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    let missing = [
        "not found",
        "no such file",
        "could not find",
        "cannot find",
        "can't find",
        "not recognized",
    ];
    if !missing.iter().any(|marker| line.contains(marker)) {
        return false;
    }
    ["erl", "elixir", "mix"].iter().any(|name| {
        line.match_indices(name).any(|(index, _)| {
            let before = line.as_bytes().get(index.wrapping_sub(1)).copied();
            let after = line.as_bytes().get(index + name.len()).copied();
            before.is_none_or(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
                && after.is_none_or(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
        })
    })
}

fn elixir_string(value: &str) -> String {
    let mut literal = String::from("\"");
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            '#' if characters.peek() == Some(&'{') => literal.push_str("\\#"),
            character => literal.push(character),
        }
    }
    literal.push('"');
    literal
}

fn words<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

#[cfg(test)]
#[path = "exunit_tests.rs"]
mod tests;

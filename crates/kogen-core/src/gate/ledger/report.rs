use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerRow {
    pub tag: String,
    pub test: String,
    pub status: LedgerStatus,
}

impl LedgerRow {
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerStatus {
    Passed,
    Failed,
    Skipped,
    Excluded,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LedgerReadError {
    Io(String),
    InvalidUtf8,
    MalformedLine { line: usize, detail: String },
    UnsafeReport(PathBuf),
}

impl fmt::Display for LedgerReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(detail) => write!(formatter, "could not read ledger: {detail}"),
            Self::InvalidUtf8 => write!(formatter, "ledger is not UTF-8"),
            Self::MalformedLine { line, detail } => {
                write!(formatter, "malformed ledger row on line {line}: {detail}")
            }
            Self::UnsafeReport(path) => write!(
                formatter,
                "ledger report is not a regular file: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for LedgerReadError {}

pub fn read_ledger_report(path: &Path) -> Result<Vec<LedgerRow>, LedgerReadError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| LedgerReadError::Io(source.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LedgerReadError::UnsafeReport(path.to_path_buf()));
    }
    let bytes = fs::read(path).map_err(|source| LedgerReadError::Io(source.to_string()))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| LedgerReadError::InvalidUtf8)?;
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let row = serde_json::from_str(line).map_err(|error| LedgerReadError::MalformedLine {
            line: index + 1,
            detail: error.to_string(),
        })?;
        rows.push(row);
    }
    Ok(rows)
}

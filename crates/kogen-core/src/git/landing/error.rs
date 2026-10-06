use crate::gate::TreeSnapshotError;
use crate::git::GitError;
use crate::run::RunPersistenceError;
use std::fmt;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LandingErrorKind {
    Git,
    Io,
    Snapshot,
    Persistence,
    InvalidRequest,
    Controller,
}

#[derive(Debug)]
pub struct LandingError {
    pub kind: LandingErrorKind,
    pub operation: &'static str,
    pub detail: String,
}

impl LandingError {
    pub(super) fn invalid(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind: LandingErrorKind::InvalidRequest,
            operation,
            detail: detail.into(),
        }
    }

    pub(super) fn io(operation: &'static str, path: &Path, error: impl fmt::Display) -> Self {
        Self {
            kind: LandingErrorKind::Io,
            operation,
            detail: format!("{}: {error}", path.display()),
        }
    }

    pub(super) fn controller(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind: LandingErrorKind::Controller,
            operation,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for LandingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl std::error::Error for LandingError {}

impl From<GitError> for LandingError {
    fn from(error: GitError) -> Self {
        Self {
            kind: LandingErrorKind::Git,
            operation: "git landing",
            detail: error.to_string(),
        }
    }
}

impl From<TreeSnapshotError> for LandingError {
    fn from(error: TreeSnapshotError) -> Self {
        Self {
            kind: LandingErrorKind::Snapshot,
            operation: "snapshot landing tree",
            detail: error.to_string(),
        }
    }
}

impl From<RunPersistenceError> for LandingError {
    fn from(error: RunPersistenceError) -> Self {
        Self {
            kind: LandingErrorKind::Persistence,
            operation: "persist landing",
            detail: error.to_string(),
        }
    }
}

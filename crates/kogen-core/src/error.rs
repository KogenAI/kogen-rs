//! Typed errors and the public CLI rendering boundary.

use std::fmt::Write as _;

use crate::ExitCode;

/// Public error classes accepted by spec/01-cli.md §1.5.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    Intent,
    Check,
    Environment,
    Provider,
    Candidate,
    Controller,
    Shape,
}

impl ErrorClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Check => "check",
            Self::Environment => "environment",
            Self::Provider => "provider",
            Self::Candidate => "candidate",
            Self::Controller => "controller",
            Self::Shape => "shape",
        }
    }
}

/// An error returned by a typed core command handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreError {
    pub class: ErrorClass,
    pub reason: String,
    pub detail: String,
    pub exit_code: ExitCode,
}

impl CoreError {
    #[must_use]
    pub fn new(
        class: ErrorClass,
        reason: impl Into<String>,
        detail: impl Into<String>,
        exit_code: ExitCode,
    ) -> Self {
        Self {
            class,
            reason: reason.into(),
            detail: detail.into(),
            exit_code,
        }
    }

    /// Render the exact stdout error line and indent continuation lines.
    #[must_use]
    pub fn render_stdout(&self) -> String {
        let mut rendered = format!(
            "{}/{}: {}",
            self.class.as_str(),
            self.reason,
            self.detail.lines().next().unwrap_or_default()
        );
        for line in self.detail.lines().skip(1) {
            let _ = write!(rendered, "\n  {line}");
        }
        rendered.push('\n');
        rendered
    }

    #[must_use]
    pub fn into_cli_output(self) -> CliOutput {
        CliOutput {
            stdout: self.render_stdout(),
            stderr: String::new(),
            exit_code: self.exit_code,
        }
    }
}

/// A handler's normal result. The CLI owns process I/O and exit mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: ExitCode,
}

impl CliOutput {
    #[must_use]
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code: ExitCode::Done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CoreError, ErrorClass};
    use crate::ExitCode;

    #[test]
    fn error_continuations_are_indented_on_stdout() {
        let error = CoreError::new(
            ErrorClass::Environment,
            "project_config_invalid",
            "project file\nline 4: invalid value",
            ExitCode::Environment,
        );

        assert_eq!(
            error.render_stdout(),
            "environment/project_config_invalid: project file\n  line 4: invalid value\n"
        );
    }
}

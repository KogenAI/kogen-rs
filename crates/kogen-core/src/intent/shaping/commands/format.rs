//! Configured shape formatters and missing-command detection.

use super::super::validation::ValidationFailure;
use super::ShapeCommands;
use crate::project::ProjectConfig;
use serde_yaml::Value;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

impl ShapeCommands {
    pub(in crate::intent::shaping) fn formatter(
        &self,
        checkout: &Path,
        config: Option<&ProjectConfig>,
        paths: [&str; 2],
    ) -> Result<bool, ValidationFailure> {
        let Some(formatter) = config
            .and_then(|config| config.raw.get("format"))
            .and_then(Value::as_sequence)
        else {
            return Ok(false);
        };
        for path in paths {
            let argv = formatter
                .iter()
                .filter_map(Value::as_str)
                .map(|value| value.replace("{path}", path))
                .map(OsString::from)
                .collect::<Vec<_>>();
            match self.run_os_argv(&argv, checkout, Duration::from_secs(120), "shape-format") {
                Ok(result) if formatter_missing(&result) => return Ok(true),
                Ok(result) if !result.timed_out && result.exit_status == Some(0) => {}
                Ok(result) => {
                    return Err(ValidationFailure {
                        reason: "formatter_failed",
                        detail: format!(
                            "formatter failed with exit {}",
                            result.exit_status.unwrap_or(-1)
                        ),
                    });
                }
                Err(error) if error.contains("No such file") || error.contains("not found") => {
                    return Ok(true);
                }
                Err(error) => {
                    return Err(ValidationFailure {
                        reason: "formatter_failed",
                        detail: error,
                    });
                }
            }
        }
        Ok(false)
    }
}

fn formatter_missing(result: &crate::run::ProcessResult) -> bool {
    if result.unavailable || matches!(result.exit_status, Some(126 | 127)) {
        return true;
    }
    fs_log(&result.log_path)
        .is_some_and(|log| log.contains("execvp() of") && log.contains("No such file or directory"))
}

fn fs_log(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

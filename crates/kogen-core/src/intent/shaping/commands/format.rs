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
        for path in paths {
            let Some(argv) = formatter_command(config, &self.acceptance.adapter, path) else {
                continue;
            };
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

fn formatter_command(
    config: Option<&ProjectConfig>,
    adapter: &str,
    path: &str,
) -> Option<Vec<OsString>> {
    if let Some(formatter) = config
        .and_then(|config| config.raw.get("format"))
        .and_then(Value::as_sequence)
    {
        return Some(
            formatter
                .iter()
                .filter_map(Value::as_str)
                .map(|value| OsString::from(value.replace("{path}", path)))
                .collect(),
        );
    }
    if adapter != "exunit" {
        return None;
    }
    let checks = config
        .and_then(|config| config.raw.get("checks"))
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .map(|row| crate::gate::CheckCommand {
            name: row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("format")
                .to_owned(),
            argv: row
                .get("argv")
                .and_then(Value::as_sequence)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(OsString::from)
                .collect(),
            timeout: Duration::from_secs(120),
        })
        .collect::<Vec<_>>();
    let mut command = crate::gate::adapters::exunit::formatter(&checks, Path::new(path))?;
    // A derived project-wide format check may not include the generated source
    // under .kogen. Always name that file, while keeping formatter options.
    if !command.iter().any(|argument| argument == path) {
        command.push(OsString::from(path));
    }
    Some(command)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exunit_formats_hidden_acceptance_source_but_never_intent_markdown() {
        let source = ".kogen/acceptance/greet_test.exs";
        assert_eq!(
            formatter_command(None, "exunit", source),
            Some(vec!["mix".into(), "format".into(), source.into()])
        );
        assert_eq!(
            formatter_command(None, "exunit", ".kogen/intents/greet/intent.md"),
            None
        );
        assert_eq!(formatter_command(None, "command", source), None);
    }
}

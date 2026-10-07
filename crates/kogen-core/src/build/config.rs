use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use crate::gate::{CheckCommand, configured_commands};
use crate::project::ProjectResolution;
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub(super) struct BuildOptions {
    pub recipe: String,
    pub planner_model: String,
    pub planner_effort: String,
    pub builder_model: String,
    pub builder_effort: String,
    pub fallback_on: bool,
    pub wall_ms: u64,
    pub tool_tokens: u64,
    pub model_generation_tokens: Option<u64>,
    pub sandbox: bool,
    pub environment: BTreeMap<String, String>,
    pub setup: Vec<CheckCommand>,
    pub setup_outputs: Vec<String>,
    pub fixes: Vec<CheckCommand>,
    pub checks: Vec<CheckCommand>,
    pub adapter: String,
    pub acceptance_extension: String,
    pub acceptance_candidate_dir: String,
    pub acceptance_run: Vec<OsString>,
    pub acceptance_timeout: Duration,
}

impl BuildOptions {
    pub fn load(project: &ProjectResolution) -> Result<Self, CoreError> {
        let machine = machine_build_config()?;
        let planner = role(project, &machine, "planner", "gpt-6.1-sol", "high");
        let builder = role(project, &machine, "builder", "gpt-6-luna", "max");
        let raw = project.config.as_ref().map(|config| &config.raw);
        let recipe = mapping_value(mapping_value(raw, "build"), "recipe")
            .and_then(Value::as_str)
            .unwrap_or("ladder")
            .to_owned();
        let fallback_on = bool_value(project, &machine, "model_fallback").unwrap_or(true);
        let wall_ms = integer_value(project, &machine, "budget_ms")
            .or_else(|| integer_value(project, &machine, "wall_minutes").map(|m| m * 60_000))
            .unwrap_or(3_600_000);
        let tool_tokens = integer_value(project, &machine, "tool_result_tokens").unwrap_or(2_000);
        let model_generation_tokens = integer_value(project, &machine, "model_generation_tokens");
        let sandbox = mapping_value(raw, "sandbox")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let environment = mapping_value(raw, "env")
            .and_then(Value::as_mapping)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(name, value)| {
                        Some((name.as_str()?.to_owned(), value.as_str()?.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let setup = check_list(project, "setup")?;
        let setup_outputs = string_list(raw, "setup_outputs");
        let fixes = configured_commands(project, "fix")
            .map_err(|error| config_error("build_config_invalid", error))?;
        let checks = configured_commands(project, "checks")
            .map_err(|error| config_error("build_config_invalid", error))?;
        let (adapter, extension, candidate_dir, acceptance_run, acceptance_timeout) =
            acceptance(project)?;
        Ok(Self {
            recipe,
            planner_model: planner.0,
            planner_effort: planner.1,
            builder_model: builder.0,
            builder_effort: builder.1,
            fallback_on,
            wall_ms,
            tool_tokens,
            model_generation_tokens,
            sandbox,
            environment,
            setup,
            setup_outputs,
            fixes,
            checks,
            adapter,
            acceptance_extension: extension,
            acceptance_candidate_dir: candidate_dir,
            acceptance_run,
            acceptance_timeout,
        })
    }
}

fn role(
    project: &ProjectResolution,
    machine: &Option<Value>,
    name: &str,
    default_model: &str,
    default_effort: &str,
) -> (String, String) {
    let mut model = default_model.to_owned();
    let mut effort = default_effort.to_owned();
    for config in [machine.as_ref(), project.config.as_ref().map(|c| &c.raw)] {
        let Some(value) = config
            .and_then(|raw| mapping_value(Some(raw), "roles"))
            .and_then(Value::as_mapping)
            .and_then(|roles| roles.get(Value::String(name.to_owned())))
        else {
            continue;
        };
        if let Some(next) = mapping_value(Some(value), "model").and_then(Value::as_str) {
            model = next.to_owned();
        }
        if let Some(next) = mapping_value(Some(value), "effort").and_then(Value::as_str) {
            effort = next.to_owned();
        }
    }
    (model, effort)
}

fn machine_build_config() -> Result<Option<Value>, CoreError> {
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let path = PathBuf::from(home).join(".kogen/config.yaml");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(config_error("config_unavailable", error)),
    };
    let value = crate::project::yaml::parse(&bytes).map_err(|error| {
        config_error(
            "config_invalid",
            format!("{}: {}", path.display(), error.message),
        )
    })?;
    Ok(mapping_value(Some(&value), "build").cloned())
}

fn integer_value(project: &ProjectResolution, machine: &Option<Value>, name: &str) -> Option<u64> {
    mapping_value(project.config.as_ref().map(|c| &c.raw), "build")
        .and_then(|build| mapping_value(Some(build), name))
        .and_then(Value::as_u64)
        .or_else(|| mapping_value(machine.as_ref(), name).and_then(Value::as_u64))
}

fn bool_value(project: &ProjectResolution, machine: &Option<Value>, name: &str) -> Option<bool> {
    mapping_value(project.config.as_ref().map(|c| &c.raw), "build")
        .and_then(|build| mapping_value(Some(build), name))
        .and_then(Value::as_bool)
        .or_else(|| mapping_value(machine.as_ref(), name).and_then(Value::as_bool))
}

fn mapping_value<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    value?.as_mapping()?.get(Value::String(key.to_owned()))
}

fn check_list(project: &ProjectResolution, field: &str) -> Result<Vec<CheckCommand>, CoreError> {
    let Some(root) = project.config.as_ref().map(|config| &config.raw) else {
        return Ok(Vec::new());
    };
    let Some(rows) = mapping_value(Some(root), field).and_then(Value::as_sequence) else {
        return Ok(Vec::new());
    };
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let name = mapping_value(Some(row), "name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    config_error(
                        "build_config_invalid",
                        format!("{field}[{}] has no name", index + 1),
                    )
                })?;
            let argv = mapping_value(Some(row), "argv")
                .and_then(Value::as_sequence)
                .and_then(|values| values.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                .ok_or_else(|| {
                    config_error(
                        "build_config_invalid",
                        format!("{field}[{}] has invalid argv", index + 1),
                    )
                })?;
            let timeout = mapping_value(Some(row), "timeout_ms")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    config_error(
                        "build_config_invalid",
                        format!("{field}[{}] has invalid timeout", index + 1),
                    )
                })?;
            Ok(CheckCommand {
                name: name.to_owned(),
                argv: argv.into_iter().map(OsString::from).collect(),
                timeout: Duration::from_millis(timeout),
            })
        })
        .collect()
}

fn acceptance(
    project: &ProjectResolution,
) -> Result<(String, String, String, Vec<OsString>, Duration), CoreError> {
    let value = mapping_value(
        project.config.as_ref().map(|config| &config.raw),
        "acceptance",
    );
    let adapter = mapping_value(value, "adapter")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if crate::gate::adapters::rails::detected(&project.checkout) {
                "rails".to_owned()
            } else {
                "exunit".to_owned()
            }
        });
    let extension = mapping_value(value, "ext")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| match adapter.as_str() {
            "command" => ".test".to_owned(),
            "rails" => crate::gate::adapters::rails::ACCEPTANCE_EXTENSION.to_owned(),
            _ => crate::gate::adapters::exunit::ACCEPTANCE_EXTENSION.to_owned(),
        });
    let candidate_dir = mapping_value(value, "candidate_dir")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| match adapter.as_str() {
            "command" => "test/acceptance".to_owned(),
            "rails" => crate::gate::adapters::rails::CANDIDATE_DIRECTORY.to_owned(),
            _ => crate::gate::adapters::exunit::CANDIDATE_DIRECTORY.to_owned(),
        });
    let command = mapping_value(value, "run")
        .and_then(Value::as_sequence)
        .and_then(|items| items.iter().map(Value::as_str).collect::<Option<Vec<_>>>());
    let command: Vec<OsString> = match (adapter.as_str(), command) {
        ("command", Some(command)) => command.into_iter().map(OsString::from).collect(),
        ("rails", _) => crate::gate::adapters::rails::runner_command(),
        _ => Vec::new(),
    };
    let timeout = mapping_value(value, "timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(600_000);
    if adapter == "command" && command.is_empty() {
        return Err(config_error(
            "build_config_invalid",
            "acceptance.run is missing",
        ));
    }
    Ok((
        adapter,
        extension,
        candidate_dir,
        command,
        Duration::from_millis(timeout),
    ))
}

fn string_list(value: Option<&Value>, key: &str) -> Vec<String> {
    mapping_value(value, key)
        .and_then(Value::as_sequence)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn config_error(reason: &str, detail: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        detail.to_string(),
        ExitCode::Environment,
    )
}

//! Shaping-specific project settings and effective gate inputs.

use crate::error::CoreError;
use crate::project::ProjectResolution;
use crate::provider::{RunAccount, resolve_run_account};
use serde_yaml::Value;
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn selected_account(
    home: &Path,
    project: &ProjectResolution,
) -> Result<RunAccount, CoreError> {
    resolve_run_account(
        home,
        &project.checkout,
        None,
        std::env::var("KOGEN_BENCH_PROVIDER").ok().as_deref(),
        std::env::var("KOGEN_BENCH_ACCOUNT").ok().as_deref(),
        std::env::var_os("KOGEN_AUTH_PATH").is_some(),
    )
}

pub(super) fn role_config(
    config: Option<&crate::project::ProjectConfig>,
    home: &Path,
    role: &str,
    default: (&str, &str),
) -> Result<(String, String), CoreError> {
    let mut effective = (default.0.to_owned(), default.1.to_owned());
    let machine_path = home.join(".kogen/config.yaml");
    let machine = match std::fs::read(&machine_path) {
        Ok(bytes) => Some(
            crate::project::yaml::parse(&bytes)
                .map_err(|error| super::files::io_error("config_invalid", error.message))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(super::files::io_error("config_unavailable", error)),
    };
    let demotion = config
        .and_then(|config| config.raw["build"]["auditor_demotion"].as_bool())
        .or_else(|| {
            machine
                .as_ref()
                .and_then(|raw| raw["build"]["auditor_demotion"].as_bool())
        });
    if demotion == Some(true) {
        return Err(super::files::io_error(
            "config_invalid",
            "build.auditor_demotion has no admitted calibration",
        ));
    }
    for raw in [machine.as_ref(), config.map(|config| &config.raw)]
        .into_iter()
        .flatten()
    {
        if let Some(model) = raw["build"]["roles"][role]["model"].as_str() {
            effective.0 = model.to_owned();
        }
        if let Some(effort) = raw["build"]["roles"][role]["effort"].as_str() {
            effective.1 = effort.to_owned();
        }
    }
    let grok = default.0.starts_with("grok-");
    if grok != effective.0.starts_with("grok-") {
        return Err(super::files::io_error(
            "config_invalid",
            format!("{role} model does not belong to selected provider"),
        ));
    }
    Ok(effective)
}

pub(super) fn domains(config: Option<&crate::project::ProjectConfig>) -> Vec<String> {
    let mut domains: Vec<String> = config
        .and_then(|config| config.raw["domains"].as_mapping())
        .map(|mapping| {
            mapping
                .keys()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    domains.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    domains
}

pub(super) fn gate_paths(config: Option<&crate::project::ProjectConfig>) -> Vec<String> {
    let mut paths = BTreeSet::from([".kogen/project.yaml".to_owned()]);
    for section in ["checks", "fix"] {
        for row in config
            .and_then(|config| config.raw[section].as_sequence())
            .into_iter()
            .flatten()
        {
            collect_argv_paths(row.get("argv"), &mut paths);
        }
    }
    if let Some(run) = config.and_then(|config| config.raw["acceptance"]["run"].as_sequence()) {
        for arg in run.iter().filter_map(Value::as_str).skip(1) {
            collect_path(arg, &mut paths);
        }
    }
    paths.into_iter().collect()
}

fn collect_argv_paths(argv: Option<&Value>, paths: &mut BTreeSet<String>) {
    let Some(argv) = argv.and_then(Value::as_sequence) else {
        return;
    };
    for arg in argv.iter().filter_map(Value::as_str).skip(1) {
        collect_path(arg, paths);
    }
}

fn collect_path(arg: &str, paths: &mut BTreeSet<String>) {
    let path = arg.split_whitespace().next().unwrap_or(arg);
    if !path.starts_with('{')
        && !Path::new(path).is_absolute()
        && (path.contains('/') || path.ends_with(".sh"))
    {
        paths.insert(path.to_owned());
    }
}

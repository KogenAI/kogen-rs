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
    role: &str,
    default: (&str, &str),
) -> (String, String) {
    let values = config
        .and_then(|config| config.raw["build"]["roles"][role].as_mapping())
        .and_then(|mapping| {
            Some((
                mapping.get("model")?.as_str()?,
                mapping.get("effort")?.as_str()?,
            ))
        });
    values.map_or_else(
        || (default.0.to_owned(), default.1.to_owned()),
        |(model, effort)| (model.to_owned(), effort.to_owned()),
    )
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

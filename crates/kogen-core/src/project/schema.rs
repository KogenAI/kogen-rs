mod build;

use serde_yaml::{Mapping, Value};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub(super) struct ValidatedConfig {
    pub name: String,
    pub base: Option<String>,
    pub raw: Value,
}

pub(super) fn validate(raw: Value) -> Result<ValidatedConfig, Vec<String>> {
    let Some(root) = raw.as_mapping() else {
        return Err(vec!["project config must be a YAML map".to_owned()]);
    };
    let mut issues = Vec::new();
    let allowed = [
        "name",
        "checks",
        "acceptance_checks",
        "setup",
        "setup_outputs",
        "setup_inputs",
        "fix",
        "format",
        "protected_paths",
        "gate_paths",
        "domains",
        "env",
        "sandbox",
        "base",
        "acceptance",
        "shaping",
        "build",
        "account",
    ];
    unknown_keys(root, &allowed, "project", &mut issues);
    for required in ["name", "checks"] {
        if get(root, required).is_none() {
            issues.push(format!("missing required key `{required}`"));
        }
    }
    if let Some(value) = get(root, "name") {
        require_string(value, "name", &mut issues);
    }
    if let Some(value) = get(root, "base") {
        require_string(value, "base", &mut issues);
    }
    for field in ["checks", "acceptance_checks", "setup", "fix"] {
        if let Some(value) = get(root, field) {
            validate_check_list(value, field, &mut issues);
        }
    }
    for field in ["setup_outputs", "setup_inputs"] {
        if let Some(value) = get(root, field) {
            validate_setup_paths(value, field, field == "setup_outputs", &mut issues);
        }
    }
    for field in ["protected_paths", "gate_paths"] {
        if let Some(value) = get(root, field) {
            validate_strings(value, field, false, &mut issues);
        }
    }
    if let Some(value) = get(root, "format") {
        validate_strings(value, "format", true, &mut issues);
    }
    if let Some(value) = get(root, "domains") {
        validate_domains(value, &mut issues);
    }
    if let Some(value) = get(root, "env") {
        validate_env(value, &mut issues);
    }
    if let Some(value) = get(root, "sandbox")
        && !matches!(value, Value::Bool(_))
    {
        issues.push("sandbox must be true or false".to_owned());
    }
    if let Some(value) = get(root, "account") {
        require_string(value, "account", &mut issues);
    }
    if let Some(value) = get(root, "acceptance") {
        validate_acceptance(value, &mut issues);
    }
    if let Some(value) = get(root, "shaping") {
        validate_shaping(value, &mut issues);
    }
    if let Some(value) = get(root, "build") {
        build::validate(value, &mut issues);
    }
    if issues.is_empty() {
        let name = get(root, "name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let base = get(root, "base").and_then(Value::as_str).map(str::to_owned);
        Ok(ValidatedConfig { name, base, raw })
    } else {
        Err(issues)
    }
}

fn validate_check_list(value: &Value, field: &str, issues: &mut Vec<String>) {
    let Some(rows) = value.as_sequence() else {
        issues.push(format!("{field} must be a list"));
        return;
    };
    let mut names = BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        let path = format!("{field}[{}]", index + 1);
        let Some(map) = row.as_mapping() else {
            issues.push(format!("{path} must be a map"));
            continue;
        };
        unknown_keys(map, &["name", "argv", "timeout_ms"], &path, issues);
        let name = get(map, "name");
        if let Some(name) = name {
            if let Some(name) = name.as_str() {
                if !names.insert(name.to_owned()) {
                    issues.push(format!("{field} has duplicate name \"{name}\""));
                }
            } else {
                issues.push(format!("{path}.name must be a string"));
            }
        } else {
            issues.push(format!("{path} is missing required key `name`"));
        }
        match get(map, "argv") {
            Some(Value::Sequence(argv)) => {
                if argv.is_empty()
                    || argv
                        .iter()
                        .any(|arg| arg.as_str().is_none_or(str::is_empty))
                {
                    issues.push(format!("{path}.argv must be a non-empty list of strings"));
                }
            }
            Some(_) => issues.push(format!("{path}.argv must be a list")),
            None => issues.push(format!("{path} is missing required key `argv`")),
        }
        match get(map, "timeout_ms") {
            Some(timeout) if timeout.as_u64().is_some_and(|n| n > 0) => {}
            Some(_) => issues.push(format!("{path}.timeout_ms must be a positive integer")),
            None => issues.push(format!("{path} is missing required key `timeout_ms`")),
        }
    }
}

fn validate_strings(value: &Value, path: &str, nonempty: bool, issues: &mut Vec<String>) {
    let Some(rows) = value.as_sequence() else {
        issues.push(format!("{path} must be a list"));
        return;
    };
    if rows.iter().any(|entry| {
        entry.as_str().is_none() || (nonempty && entry.as_str().is_some_and(str::is_empty))
    }) {
        issues.push(format!("{path} must contain only strings"));
    }
}

fn validate_setup_paths(value: &Value, field: &str, outputs: bool, issues: &mut Vec<String>) {
    let Some(paths) = value.as_sequence() else {
        issues.push(format!("{field} must be a list"));
        return;
    };
    let mut valid = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, value) in paths.iter().enumerate() {
        let Some(path) = value.as_str() else {
            issues.push(format!("{field}[{}] must be a string", index + 1));
            continue;
        };
        let segments = path.split('/').collect::<Vec<_>>();
        let invalid = path.is_empty()
            || path.contains(['\0', '\r', '\n'])
            || path.starts_with('/')
            || segments
                .iter()
                .any(|segment| segment.is_empty() || matches!(*segment, "." | ".." | ".git"));
        if invalid {
            issues.push(format!(
                "{field}[{}] must be a relative path without empty, dot, parent, or .git segments",
                index + 1
            ));
            continue;
        }
        if outputs && !seen.insert(path.to_owned()) {
            issues.push(format!("{field} has duplicate path \"{path}\""));
        }
        valid.push(path);
    }
    if outputs {
        for (index, path) in valid.iter().enumerate() {
            if valid.iter().skip(index + 1).any(|other| {
                path.starts_with(&format!("{other}/")) || other.starts_with(&format!("{path}/"))
            }) {
                issues.push(format!("{field} paths overlap at \"{path}\""));
            }
        }
    }
}

fn validate_domains(value: &Value, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push("domains must be a map".to_owned());
        return;
    };
    for (key, value) in map {
        let name = key.as_str().unwrap_or("?");
        let path = format!("domains.{name}");
        if let Some(paths) = value.as_sequence() {
            if paths.iter().any(|path| path.as_str().is_none()) {
                issues.push(format!("{path} must contain only strings"));
            }
        } else {
            issues.push(format!("{path} must be a list"));
        }
    }
}

fn validate_env(value: &Value, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push("env must be a map".to_owned());
        return;
    };
    for (key, value) in map {
        let Some(name) = key.as_str() else {
            issues.push("env keys must be strings".to_owned());
            continue;
        };
        if name.starts_with("KOGEN_") {
            issues.push(format!("env key \"{name}\" is reserved"));
        }
        if value.as_str().is_none() {
            issues.push(format!("env.{name} must be a string"));
        }
    }
}

fn validate_acceptance(value: &Value, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push("acceptance must be a map".to_owned());
        return;
    };
    unknown_keys(
        map,
        &["adapter", "ext", "candidate_dir", "run", "timeout_ms"],
        "acceptance",
        issues,
    );
    for field in ["adapter", "ext", "candidate_dir"] {
        if let Some(value) = get(map, field) {
            require_string(value, &format!("acceptance.{field}"), issues);
        }
    }
    if let Some(value) = get(map, "run") {
        validate_strings(value, "acceptance.run", true, issues);
    }
    if let Some(value) = get(map, "timeout_ms")
        && !value.as_u64().is_some_and(|n| n > 0)
    {
        issues.push("acceptance.timeout_ms must be a positive integer".to_owned());
    }
}

fn validate_shaping(value: &Value, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push("shaping must be a map".to_owned());
        return;
    };
    unknown_keys(map, &["proof"], "shaping", issues);
    if let Some(proof) = get(map, "proof") {
        match proof.as_str() {
            Some("none" | "witness") => {}
            Some(_) => issues.push("shaping.proof must be none or witness".to_owned()),
            None => issues.push("shaping.proof must be a string".to_owned()),
        }
    }
}

pub(super) fn unknown_keys(
    map: &Mapping,
    allowed: &[&str],
    prefix: &str,
    issues: &mut Vec<String>,
) {
    for key in map.keys() {
        let Some(key) = key.as_str() else {
            issues.push(format!("{prefix} keys must be strings"));
            continue;
        };
        if !allowed.contains(&key) {
            let subject = if prefix == "project" {
                "project"
            } else {
                prefix
            };
            issues.push(format!("{subject} has unknown key \"{key}\""));
        }
    }
}

pub(super) fn get<'a>(map: &'a Mapping, key: &str) -> Option<&'a Value> {
    map.get(Value::String(key.to_owned()))
}

pub(super) fn require_string(value: &Value, field: &str, issues: &mut Vec<String>) {
    if value.as_str().is_none() {
        issues.push(format!("{field} must be a string"));
    }
}

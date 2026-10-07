use super::model::{BaselineCache, BaselineRow, Finding};
mod acceptance;
mod cache;
mod identity;

use crate::gate::is_test_rule;
use crate::git::GitRepo;
use crate::project::ProjectResolution;
use crate::run::setup_cache::{SetupCacheKey, SetupCacheRequest, run_setup as run_cached_setup};
use crate::run::{
    EnvironmentRequest, ProcessPort, ProcessRequest, ProcessResult, ProcessSupervisor,
    build_child_environment, host_environment,
};
pub(super) use acceptance::{check_error, stage_and_check};
use cache::{read_cache, write_cache};
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) struct CheckOutcome {
    pub rows: Vec<BaselineRow>,
    pub run_dir: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
}

pub(super) fn run_setup_and_baseline(
    project: &ProjectResolution,
    base_sha: &str,
    run_dir: PathBuf,
) -> Result<CheckOutcome, CheckError> {
    // Never label a dirty or stale checkout as the resolved base.
    let checkout_repo = GitRepo::new(&project.checkout);
    let exact = checkout_repo.resolve_commit("HEAD").ok().as_deref() == Some(base_sha)
        && checkout_repo
            .text(&["status", "--porcelain=v1", "--untracked-files=no"])
            .is_ok_and(|status| status.is_empty());
    if !exact {
        let workspace = run_dir.join("baseline-base");
        let repository = crate::git::landing::LandingRepository::clone_fresh(
            &project.origin,
            &workspace,
            base_sha,
        )
        .map_err(|error| CheckError::Internal(error.to_string()))?;
        let mut checked = project.clone();
        checked.checkout = workspace.clone();
        let result = run_setup_and_baseline(&checked, base_sha, run_dir);
        drop(repository);
        crate::git::forget_workspace(&workspace);
        let _ = fs::remove_dir_all(workspace);
        return result;
    }
    let runner = ProcessSupervisor;
    fs::create_dir_all(&run_dir).map_err(|error| CheckError::Internal(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700))
            .map_err(|error| CheckError::Internal(error.to_string()))?;
    }
    let mut request = EnvironmentRequest::new(
        host_environment(),
        &run_dir,
        &project.checkout,
        &project.checkout,
    );
    request.project = configured_env(project);
    let env = build_child_environment(&runner, request)
        .map_err(|error| CheckError::Internal(error.to_string()))?;
    let origin = GitRepo::new(&project.origin);
    let base_tree = origin
        .resolve_tree(base_sha)
        .map_err(|error| CheckError::Internal(error.to_string()))?;
    let setup_key =
        SetupCacheKey::from_project(project.config.as_ref(), &project.checkout, &base_tree, &env)
            .map_err(|error| CheckError::Internal(error.to_string()))?;
    let checks = checks_value(project)?;
    let identity =
        identity::baseline_key(&setup_key, &base_tree, &checks, &project.checkout, &env)?;
    // Unknown check toolchains are a miss, rather than a reusable empty identity.
    let key = identity
        .clone()
        .unwrap_or_else(|| format!("uncached-{}", rand::random::<u64>()));
    let setup_key = setup_key.digest();
    let cache_path = project
        .state_root
        .join("approval-cache")
        .join(format!("{key}.json"));
    if identity.is_some()
        && let Some(cache) = read_cache(&cache_path, &key)
    {
        return Ok(CheckOutcome {
            rows: cache.rows,
            run_dir,
            env,
        });
    }
    let outputs = config_list(project, "setup_outputs")
        .unwrap_or_default()
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let setup_enabled =
        !config_list(project, "setup").unwrap_or_default().is_empty() && !outputs.is_empty();
    run_cached_setup(
        SetupCacheRequest {
            checkout: &project.checkout,
            cache_root: &project.state_root.join("setup-cache"),
            key: &setup_key,
            outputs,
            enabled: setup_enabled,
        },
        || run_setup(project, &runner, &run_dir, &env),
    )?;
    let rows = run_checks(project, &runner, &run_dir, &env)?;
    if identity.is_some() {
        write_cache(
            &cache_path,
            &BaselineCache {
                key,
                rows: rows.clone(),
            },
        )?;
    }
    Ok(CheckOutcome { rows, run_dir, env })
}

#[derive(Debug)]
pub(super) enum CheckError {
    Internal(String),
    SetupFailed {
        name: String,
        status: Option<i32>,
        timed_out: bool,
        tail: Vec<String>,
    },
    ToolMissing(String),
    AcceptanceFailed {
        name: String,
        timed_out: bool,
        tail: Vec<String>,
    },
}

fn run_setup(
    project: &ProjectResolution,
    runner: &dyn ProcessPort,
    run_dir: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Result<u64, CheckError> {
    let started = Instant::now();
    let Some(setups) = config_list(project, "setup") else {
        return Ok(0);
    };
    for (index, setup) in setups.iter().enumerate() {
        let name = field(setup, "name").unwrap_or_else(|| format!("setup-{}", index + 1));
        let args = argv(setup);
        let Some((program, rest)) = args.split_first() else {
            return Err(CheckError::Internal(format!("Setup {name} has no argv")));
        };
        let result = run_process(
            runner,
            ProcessInvocation {
                cwd: &project.checkout,
                run_dir,
                env,
                program,
                args: rest,
                timeout: timeout(setup),
                log_name: &format!("setup-{}", safe_log_name(&name)),
            },
        )?;
        if result.unavailable || result.timed_out || result.exit_status != Some(0) {
            return Err(CheckError::SetupFailed {
                name,
                status: result.exit_status,
                timed_out: result.timed_out,
                tail: tail_lines(&result.output_tail, 20),
            });
        }
    }
    Ok(started.elapsed().as_millis().min(u64::MAX as u128) as u64)
}

fn run_checks(
    project: &ProjectResolution,
    runner: &dyn ProcessPort,
    run_dir: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Result<Vec<BaselineRow>, CheckError> {
    let Some(checks) = config_list(project, "checks") else {
        return Ok(Vec::new());
    };
    let repo = GitRepo::new(&project.checkout);
    let mut rows = Vec::with_capacity(checks.len());
    for (index, check) in checks.iter().enumerate() {
        let name = field(check, "name").unwrap_or_else(|| format!("check-{}", index + 1));
        let argv = argv(check);
        let Some((program, args)) = argv.split_first() else {
            return Err(CheckError::Internal(format!("check {name} has no argv")));
        };
        let before = repo
            .output(&["status", "--porcelain=v1", "--untracked-files=all"])
            .map_err(|error| CheckError::Internal(error.to_string()))?;
        let result = run_process(
            runner,
            ProcessInvocation {
                cwd: &project.checkout,
                run_dir,
                env,
                program,
                args,
                timeout: timeout(check),
                log_name: &format!("check-{}", safe_log_name(&name)),
            },
        )?;
        let after = repo
            .output(&["status", "--porcelain=v1", "--untracked-files=all"])
            .map_err(|error| CheckError::Internal(error.to_string()))?;
        rows.push(baseline_row(name, result, before != after));
    }
    Ok(rows)
}

fn baseline_row(name: String, result: ProcessResult, changed: bool) -> BaselineRow {
    let failed = result.exit_status.is_some_and(|status| status != 0);
    let status = if changed {
        "mutating"
    } else if result.timed_out {
        "timeout"
    } else if result.unavailable || matches!(result.exit_status, Some(126 | 127)) {
        "unavailable"
    } else if failed {
        "red"
    } else {
        "green"
    };
    BaselineRow {
        name,
        status: status.to_owned(),
        exit_status: result.exit_status,
        findings: parse_findings(&result.output_tail),
    }
}

fn parse_findings(output: &[u8]) -> Vec<Finding> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(parse_finding)
        .collect()
}

fn parse_finding(line: &str) -> Option<Finding> {
    let (path, rest) = line.split_once(':')?;
    let (line_number, rest) = rest.split_once(':')?;
    let line_number = line_number.parse::<u32>().ok()?;
    let rest = if let Some((column, after_column)) = rest.split_once(':')
        && column.parse::<u32>().is_ok()
    {
        after_column.trim_start()
    } else {
        rest.trim_start()
    };
    let rest = rest
        .strip_prefix("error: ")
        .or_else(|| rest.strip_prefix("warning: "))
        .or_else(|| rest.strip_prefix("note: "))?;
    let rest = rest.strip_prefix('[')?;
    let (rule, rest) = rest.split_once("] ")?;
    let (symbol, message) = if is_test_rule(rule) {
        rest.split_once(": ").unwrap_or(("", rest))
    } else {
        ("", rest)
    };
    Some(Finding {
        path: path.to_owned(),
        rule: rule.to_owned(),
        symbol: symbol.to_owned(),
        message: message.to_owned(),
        line: Some(line_number),
    })
}

struct ProcessInvocation<'a> {
    cwd: &'a Path,
    run_dir: &'a Path,
    env: &'a BTreeMap<OsString, OsString>,
    program: &'a str,
    args: &'a [String],
    timeout: Duration,
    log_name: &'a str,
}

fn run_process(
    runner: &dyn ProcessPort,
    invocation: ProcessInvocation<'_>,
) -> Result<ProcessResult, CheckError> {
    let mut request = ProcessRequest::new(invocation.program, invocation.cwd, invocation.run_dir);
    request.args = invocation.args.iter().map(OsString::from).collect();
    request.env = invocation.env.clone();
    request.timeout = invocation.timeout;
    request.log_name = safe_log_name(invocation.log_name);
    runner
        .run(request)
        .map_err(|error| CheckError::Internal(error.to_string()))
}

fn configured_env(project: &ProjectResolution) -> BTreeMap<String, String> {
    config_map(project, "env")
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    Some((key.as_str()?.to_owned(), value.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn checks_value(project: &ProjectResolution) -> Result<serde_json::Value, CheckError> {
    config_value(project, "checks")
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| CheckError::Internal(error.to_string()))
        .map(|value| value.unwrap_or_else(|| serde_json::json!([])))
}

fn config_value<'a>(project: &'a ProjectResolution, field: &str) -> Option<&'a Value> {
    let root = project.config.as_ref()?.raw.as_mapping()?;
    root.get(Value::String(field.to_owned()))
}

fn config_list<'a>(project: &'a ProjectResolution, field: &str) -> Option<Vec<&'a Value>> {
    config_value(project, field)?
        .as_sequence()
        .map(|rows| rows.iter().collect())
}

fn config_map<'a>(project: &'a ProjectResolution, field: &str) -> Option<&'a serde_yaml::Mapping> {
    config_value(project, field)?.as_mapping()
}

fn field(value: &Value, field: &str) -> Option<String> {
    value
        .as_mapping()?
        .get(Value::String(field.to_owned()))?
        .as_str()
        .map(str::to_owned)
}

fn argv(value: &Value) -> Vec<String> {
    let Some(args) = value
        .as_mapping()
        .and_then(|map| map.get(Value::String("argv".to_owned())))
        .and_then(Value::as_sequence)
    else {
        return Vec::new();
    };
    args.iter()
        .map(|arg| arg.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

fn timeout(value: &Value) -> Duration {
    let millis = value
        .as_mapping()
        .and_then(|map| map.get(Value::String("timeout_ms".to_owned())))
        .and_then(Value::as_u64)
        .unwrap_or(60_000);
    Duration::from_millis(millis)
}

fn tail_lines(bytes: &[u8], limit: usize) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(str::to_owned)
        .collect()
}

fn safe_log_name(name: &str) -> String {
    let sanitized = name
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
                char::from(byte)
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "approval".to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::parse_finding;

    #[test]
    fn approval_baselines_only_extract_symbols_for_test_failures() {
        let lint = parse_finding("lib/old.txt:2:1: error: [lint/todo] old.txt: TODO found")
            .expect("parse lint finding");
        assert_eq!(lint.symbol, "");
        assert_eq!(lint.message, "old.txt: TODO found");

        let test = parse_finding("test/unit/greet.t.sh:1:1: error: [kt/test] alpha: failed")
            .expect("parse kt test finding");
        assert_eq!(test.symbol, "alpha");
        assert_eq!(test.message, "failed");
    }
}

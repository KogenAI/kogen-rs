use super::model::{BaselineCache, BaselineRow, Finding};
mod acceptance;

use crate::git::GitRepo;
use crate::project::ProjectResolution;
use crate::run::{
    EnvironmentRequest, ProcessPort, ProcessRequest, ProcessResult, ProcessSupervisor,
    build_child_environment, host_environment,
};
pub(super) use acceptance::{check_error, stage_and_check};
use serde_yaml::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    run_setup(project, &runner, &run_dir, &env)?;

    let origin = GitRepo::new(&project.origin);
    let base_tree = origin
        .resolve_tree(base_sha)
        .map_err(|error| CheckError::Internal(error.to_string()))?;
    let key = cache_key(project, &base_tree, &env);
    let cache_path = project
        .state_root
        .join("approval-cache")
        .join(format!("{key}.json"));
    if let Some(cache) = read_cache(&cache_path, &key) {
        return Ok(CheckOutcome {
            rows: cache.rows,
            run_dir,
            env,
        });
    }
    let rows = run_checks(project, &runner, &run_dir, &env)?;
    write_cache(
        &cache_path,
        &BaselineCache {
            key,
            rows: rows.clone(),
        },
    )?;
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
) -> Result<(), CheckError> {
    let Some(setups) = config_list(project, "setup") else {
        return Ok(());
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
    Ok(())
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
    let (symbol, message) = rest.split_once(": ").unwrap_or(("", rest));
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

fn cache_key(
    project: &ProjectResolution,
    base_tree: &str,
    env: &BTreeMap<OsString, OsString>,
) -> String {
    let checks = config_value(project, "checks");
    let setup = config_value(project, "setup");
    let env = env
        .iter()
        .filter_map(|(key, value)| {
            let key = key.to_string_lossy();
            if matches!(key.as_ref(), "TMPDIR" | "MISE_STATE_DIR" | "MISE_CACHE_DIR") {
                return None;
            }
            Some((key.into_owned(), value.to_string_lossy().into_owned()))
        })
        .collect::<BTreeMap<_, _>>();
    let encoded = serde_json::to_vec(&(base_tree, setup, checks, env)).unwrap_or_default();
    let digest = Sha256::digest(encoded);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_cache(path: &Path, key: &str) -> Option<BaselineCache> {
    let bytes = fs::read(path).ok()?;
    let cache: BaselineCache = serde_json::from_slice(&bytes).ok()?;
    (cache.key == key).then_some(cache)
}

fn write_cache(path: &Path, cache: &BaselineCache) -> Result<(), CheckError> {
    let parent = path
        .parent()
        .ok_or_else(|| CheckError::Internal("approval cache has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|error| CheckError::Internal(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| CheckError::Internal(error.to_string()))?;
    }
    let bytes =
        serde_json::to_vec(cache).map_err(|error| CheckError::Internal(error.to_string()))?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, bytes).map_err(|error| CheckError::Internal(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .map_err(|error| CheckError::Internal(error.to_string()))?;
    }
    fs::rename(&temporary, path).map_err(|error| CheckError::Internal(error.to_string()))
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

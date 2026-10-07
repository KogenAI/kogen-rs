use super::super::support::{check_error_line, environment_error, remove_empty_parents};
use super::*;
use crate::ExitCode;
use crate::error::CoreError;
use crate::project::CheckoutLock;
use std::path::Path;

pub(crate) fn stage_and_check(
    project: &ProjectResolution,
    candidate: &Path,
    relative: &str,
    bytes: &[u8],
    checks: &CheckOutcome,
) -> Result<(), CoreError> {
    let _lock = CheckoutLock::acquire(&project.state_root, &project.checkout)?;
    if std::fs::symlink_metadata(candidate).is_ok() {
        return Err(environment_error(
            "acceptance_check_path_conflict",
            relative,
            ExitCode::Environment,
        ));
    }
    let parent = candidate.parent().ok_or_else(|| {
        environment_error(
            "acceptance_check_path_conflict",
            relative,
            ExitCode::Environment,
        )
    })?;
    let relative_path = candidate.strip_prefix(&project.checkout).map_err(|error| {
        environment_error(
            "acceptance_check_path_conflict",
            error.to_string(),
            ExitCode::Environment,
        )
    })?;
    let relative = Path::new(relative_path);
    let relative_parent = relative.parent().unwrap_or_else(|| Path::new(""));
    crate::safe_fs::ensure_dir(&project.checkout, relative_parent)
        .and_then(|()| crate::safe_fs::validate_write(&project.checkout, relative))
        .and_then(|()| crate::safe_fs::write_file(&project.checkout, relative, bytes))
        .map_err(|error| {
            environment_error(
                "acceptance_check_path_conflict",
                error.to_string(),
                ExitCode::Environment,
            )
        })?;
    let result = run_acceptance_checks(project, candidate, &checks.run_dir, &checks.env);
    let cleanup = crate::safe_fs::remove_file(&project.checkout, relative);
    remove_empty_parents(parent, &project.checkout);
    if let Err(error) = cleanup
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(environment_error(
            "acceptance_check_cleanup_failed",
            error.to_string(),
            ExitCode::Environment,
        ));
    }
    result.map_err(check_error)
}

fn run_acceptance_checks(
    project: &ProjectResolution,
    candidate_path: &Path,
    run_dir: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Result<(), CheckError> {
    let Some(checks) = config_list(project, "acceptance_checks") else {
        return Ok(());
    };
    let runner = ProcessSupervisor;
    for (index, check) in checks.iter().enumerate() {
        let name = field(check, "name").unwrap_or_else(|| format!("check-{}", index + 1));
        let argv = argv(check);
        let Some((program, args)) = argv.split_first() else {
            return Err(CheckError::Internal(format!("{name} has no argv")));
        };
        let replaced = args
            .iter()
            .map(|arg| arg.replace("{path}", &candidate_path.to_string_lossy()))
            .collect::<Vec<_>>();
        let result = run_process(
            &runner,
            ProcessInvocation {
                cwd: &project.checkout,
                run_dir,
                env,
                program,
                args: &replaced,
                timeout: timeout(check),
                log_name: &format!("acceptance-{}", safe_log_name(&name)),
            },
        )?;
        if result.unavailable || matches!(result.exit_status, Some(126 | 127)) {
            return Err(CheckError::ToolMissing(program.clone()));
        }
        if result.timed_out || result.exit_status != Some(0) {
            return Err(CheckError::AcceptanceFailed {
                name,
                timed_out: result.timed_out,
                tail: tail_lines(&result.output_tail, 20),
            });
        }
    }
    Ok(())
}

pub(crate) fn check_error(error: CheckError) -> CoreError {
    match error {
        CheckError::SetupFailed {
            name,
            status,
            timed_out,
            tail,
        } => {
            let mut detail = format!(
                "Setup {name} failed (status={}, timed_out={timed_out}).",
                status.unwrap_or_default()
            );
            for line in tail {
                detail.push('\n');
                detail.push_str(&line);
            }
            environment_error("setup_failed", detail, ExitCode::Environment)
        }
        CheckError::ToolMissing(program) => environment_error(
            "tool_missing",
            format!("{program} is not available"),
            ExitCode::Environment,
        ),
        CheckError::AcceptanceFailed {
            name,
            timed_out,
            tail,
        } => {
            let mut detail = format!(
                "acceptance check {name} {}",
                if timed_out { "timed out" } else { "failed" }
            );
            for line in tail {
                detail.push('\n');
                detail.push_str(&line);
            }
            check_error_line("acceptance_check_failed", detail, ExitCode::Negative)
        }
        CheckError::Internal(detail) => {
            environment_error("approval_check_failed", detail, ExitCode::Environment)
        }
    }
}

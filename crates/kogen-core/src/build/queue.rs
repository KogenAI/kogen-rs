use super::approval::ApprovedBuild;
use super::single_rung::BuildStatus;
use crate::ExitCode;
use crate::error::{CliOutput, CoreError, ErrorClass};
use crate::project::ProjectResolution;
use crate::queue::{
    DrainOutcome, QueueApproval, QueueEvent, QueuePidLock, QueuePidStart, QueueScheduler,
    QueueStopState,
};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn queue_start(project: &ProjectResolution, detach: bool) -> CliOutput {
    if detach {
        return detach_queue(project);
    }
    match drain(project) {
        Ok(output) => output,
        Err(error) => error.into_cli_output(),
    }
}

pub fn queue_stop(project: &ProjectResolution) -> CliOutput {
    if let Err(error) = project.ensure_state_root() {
        return environment_error("queue_lock_failed", error).into_cli_output();
    }
    match QueuePidLock::request_stop(&project.state_root) {
        Ok(QueueStopState::Requested(pid)) => CliOutput::success(format!(
            "queue: stopping after the current Build (pid {pid})\n"
        )),
        Ok(QueueStopState::NotRunning) => CliOutput::success("queue: not running\n"),
        Err(error) => error.into_cli_output(),
    }
}

fn drain(project: &ProjectResolution) -> Result<CliOutput, CoreError> {
    project
        .ensure_state_root()
        .map_err(|error| environment_error("queue_lock_failed", error))?;
    let owner = match QueuePidLock::acquire(&project.state_root)? {
        QueuePidStart::Acquired(owner) => owner,
        QueuePidStart::Running(pid) => {
            return Ok(CliOutput::success(format!(
                "queue: already running (pid {pid})\n"
            )));
        }
    };
    crate::recovery::reconcile(project)?;
    let board = crate::status::inspect(project)?.board;
    let mut scheduler = QueueScheduler::new();
    let mut approvals = BTreeMap::new();
    for slug in board.queue {
        let approval = ApprovedBuild::load(project, &slug);
        let queue_item = match &approval {
            Ok(approval) => QueueApproval::new(
                slug.clone(),
                approval.approval_time,
                approval.intent.frontmatter.priority,
                approval.approval_sha256.clone(),
            )
            .with_approval_commit(approval.commit.clone()),
            Err(_) => QueueApproval::new(slug.clone(), 0, 0, ""),
        };
        if let Ok(approval) = approval {
            approvals.insert(slug, approval);
        }
        scheduler.apply(QueueEvent::Enqueue(queue_item));
    }
    let mut stderr = String::new();
    let mut observation = scheduler.apply(QueueEvent::Start);
    let mut stop_reason = None;
    loop {
        if observation.current.is_empty() {
            break;
        }
        let slug = observation.current.clone();
        let Some(approval) = approvals.get(&slug) else {
            emit_line(&format!("building {slug}\n"));
            emit_line(&format!(
                "stopped {slug}: controller/approval_invalid; it stays queued\n"
            ));
            stop_reason = Some((slug, "controller".to_owned()));
            observation = scheduler.apply(QueueEvent::Outcome(DrainOutcome::StoppedController));
            break;
        };
        if approval.target_branch != project.base {
            emit_line(&format!(
                "skipped {slug}: environment/approval_branch_mismatch\n"
            ));
            observation = scheduler.apply(QueueEvent::Outcome(DrainOutcome::Skipped));
            continue;
        }
        emit_line(&format!("building {slug}\n"));
        match super::single_rung::run(project, approval) {
            Ok(build) => {
                stderr.push_str(&build.stderr);
                match build.status {
                    BuildStatus::Landed => {
                        if build.advisory_items.is_empty() {
                            emit_line(&format!(
                                "~landed {slug} {} (Build {})\n",
                                short(&build.commit),
                                short(&build.run_id),
                            ));
                        } else {
                            emit_line(&format!(
                                "~landed {slug} {} (advisory: {}) (Build {})\n",
                                short(&build.commit),
                                build.advisory_items.join(", "),
                                short(&build.run_id),
                            ));
                        }
                        observation = scheduler.apply(QueueEvent::Outcome(DrainOutcome::Landed));
                    }
                    BuildStatus::Failed => {
                        emit_line(&format!(
                            "failed {slug}: {}; best candidate {} at refs/kogen/parked/{} (Build {})\n",
                            build.reason,
                            build.verdict,
                            build.run_id,
                            short(&build.run_id),
                        ));
                        observation = scheduler.apply(QueueEvent::Outcome(DrainOutcome::Failed));
                    }
                    BuildStatus::Parked => {
                        emit_line(&format!(
                            "parked {slug}: {}; best candidate {} at refs/kogen/parked/{} (Build {})\n",
                            build.reason,
                            build.verdict,
                            build.run_id,
                            short(&build.run_id),
                        ));
                        observation = scheduler.apply(QueueEvent::Outcome(DrainOutcome::Parked));
                    }
                    BuildStatus::Stopped { class, reason } => {
                        if build.has_run {
                            emit_line(&format!(
                                "stopped {slug}: {class}/{reason}; it stays queued (Build {})\n",
                                short(&build.run_id),
                            ));
                        } else {
                            emit_line(&format!(
                                "stopped {slug}: {class}/{reason}; it stays queued\n"
                            ));
                        }
                        stop_reason = Some((slug, class.clone()));
                        let outcome = match class.as_str() {
                            "provider" => DrainOutcome::StoppedProvider,
                            "controller" => DrainOutcome::StoppedController,
                            _ => DrainOutcome::StoppedEnvironment,
                        };
                        observation = scheduler.apply(QueueEvent::Outcome(outcome));
                        break;
                    }
                }
            }
            Err(error) => {
                let class = error.class.as_str().to_owned();
                emit_line(&format!(
                    "stopped {slug}: {class}/{}; it stays queued\n",
                    error.reason
                ));
                stop_reason = Some((slug, class.clone()));
                let outcome = match class.as_str() {
                    "provider" => DrainOutcome::StoppedProvider,
                    "controller" => DrainOutcome::StoppedController,
                    _ => DrainOutcome::StoppedEnvironment,
                };
                observation = scheduler.apply(QueueEvent::Outcome(outcome));
                break;
            }
        }
        if owner.stop_requested()? && !observation.current.is_empty() {
            scheduler.apply(QueueEvent::Halt);
            scheduler.apply(QueueEvent::Outcome(DrainOutcome::Skipped));
            observation = scheduler.observe();
            break;
        }
    }
    let final_line = if stop_reason.is_some() {
        let (slug, class) = stop_reason.unwrap_or_default();
        let article = if class == "environment" { "an" } else { "a" };
        format!(
            "queue: stopped because {slug} hit {article} {class} error; {} Build(s), {} landed, {} not\n",
            observation.built,
            observation.landed,
            observation.built.saturating_sub(observation.landed),
        )
    } else if observation.line == "stopped_on_request" {
        format!(
            "queue: stopped on request; {} Build(s), {} landed, {} not\n",
            observation.built,
            observation.landed,
            observation.built.saturating_sub(observation.landed),
        )
    } else if observation.built == 0 {
        "queue: nothing to build\n".to_owned()
    } else {
        format!(
            "queue: done; {} Build(s), {} landed, {} not\n",
            observation.built,
            observation.landed,
            observation.built.saturating_sub(observation.landed),
        )
    };
    emit_line(&final_line);
    let exit = match observation.exit {
        3 => ExitCode::Environment,
        4 => ExitCode::Provider,
        70 => ExitCode::Bug,
        1 => ExitCode::Negative,
        _ => ExitCode::Done,
    };
    owner.release();
    Ok(CliOutput {
        stdout: String::new(),
        stderr,
        exit_code: exit,
    })
}

fn emit_line(line: &str) {
    use std::io::Write as _;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let _ = output.write_all(line.as_bytes());
    let _ = output.flush();
}

fn detach_queue(project: &ProjectResolution) -> CliOutput {
    if let Err(error) = project.ensure_state_root() {
        return environment_error("detach_unavailable", error).into_cli_output();
    }
    if let Some(pid) = live_queue_pid(&project.state_root) {
        return CliOutput::success(format!("queue: already running (pid {pid})\n"));
    }
    let log_path = project.state_root.join("queue.log");
    let log = match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(file) => file,
        Err(error) => return environment_error("detach_unavailable", error).into_cli_output(),
    };
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => return environment_error("detach_unavailable", error).into_cli_output(),
    };
    let mut child = Command::new(executable);
    child
        .args(["queue", "start", "--project"])
        .arg(&project.checkout)
        .arg("--origin")
        .arg(&project.origin)
        .arg("--base")
        .arg(&project.base)
        .stdin(Stdio::null())
        .stdout(match log.try_clone() {
            Ok(stdout) => Stdio::from(stdout),
            Err(error) => return environment_error("detach_unavailable", error).into_cli_output(),
        })
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        child.process_group(0);
    }
    match child.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            if !wait_for_queue_owner(&mut child, &project.state_root, pid) {
                if let Some(owner) = live_queue_pid(&project.state_root) {
                    return CliOutput::success(format!("queue: already running (pid {owner})\n"));
                }
                return environment_error(
                    "detach_unavailable",
                    "the background queue did not acquire its lock",
                )
                .into_cli_output();
            }
            CliOutput::success(format!(
                "queue: started in the background (pid {pid})\nlog: {}\n",
                log_path.display(),
            ))
        }
        Err(_) => environment_error(
            "detach_unavailable",
            "--detach needs an installed kogen; run kogen queue start in the background instead",
        )
        .into_cli_output(),
    }
}

fn live_queue_pid(state_root: &std::path::Path) -> Option<u32> {
    let pid = std::fs::read_to_string(state_root.join("queue.pid"))
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .status()
        .ok()?
        .success()
        .then_some(pid)
}

fn wait_for_queue_owner(
    child: &mut std::process::Child,
    state_root: &std::path::Path,
    pid: u32,
) -> bool {
    let owner_path = state_root.join("queue.pid");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if std::fs::read_to_string(&owner_path)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            == Some(pid)
        {
            return true;
        }
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return false;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn short(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn environment_error(reason: &str, detail: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        detail.to_string(),
        ExitCode::Environment,
    )
}

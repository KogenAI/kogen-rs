//! Filesystem and Git effect driver for dead-owner reconciliation.

use super::replay::recovery_decision;
use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use crate::git::GitRepo;
use crate::project::ProjectResolution;
use crate::run::{RunEvent, RunSnapshot, RunStore};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CLAIM_REF: &str = "refs/kogen/claim";

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecoveryReport {
    pub reconciled: Vec<ReconciledRun>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReconciledRun {
    pub run_id: String,
    pub slug: String,
    pub status: String,
    pub reason: String,
}

/// Recover dead running Builds before status derivation or a queue drain step.
pub fn reconcile(project: &ProjectResolution) -> Result<RecoveryReport, CoreError> {
    let runs_path = project.state_root.join("runs");
    let entries = match fs::read_dir(&runs_path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecoveryReport::default());
        }
        Err(error) => return Err(recovery_error("run_directory_unavailable", error)),
    };
    let origin = GitRepo::new(&project.origin);
    let base = origin
        .resolve_commit(&project.base)
        .map_err(|error| recovery_error("base_read_failed", error))?;
    let reachable = reachable_commits(&origin, &base)?;
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    let mut report = RecoveryReport::default();
    for directory in paths {
        let snapshot_path = directory.join("run.json");
        let bytes = match fs::read(&snapshot_path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let Ok(mut snapshot) = serde_json::from_slice::<RunSnapshot>(&bytes) else {
            continue;
        };
        if snapshot.status != "running" || !valid_run_id(&snapshot.run_id) {
            continue;
        }
        let last_event = read_last_event(&directory).unwrap_or_default();
        let alive = owner_is_alive(snapshot.owner_pid, snapshot.owner_started_ms);
        let candidate = snapshot
            .landing
            .as_ref()
            .map(|landing| landing.candidate_commit.as_str())
            .unwrap_or_default();
        let on_base = !candidate.is_empty() && reachable.contains(candidate);
        let Some(decision) = recovery_decision("running", alive, on_base, &last_event) else {
            continue;
        };

        snapshot.status = decision.status.to_owned();
        let event = if decision.status == "landed" {
            RunEvent::new("reconciled", now_ms()).with("status", json!(decision.status))
        } else {
            RunEvent::new("finished", now_ms())
                .with("status", json!(decision.status))
                .with("reason", json!(decision.reason))
        };
        RunStore::new(&directory)
            .record(&event, &snapshot)
            .map_err(|error| recovery_error("run_reconcile_failed", error))?;
        release_claim_for_run(&origin, &snapshot.run_id)?;
        remove_incoming(&origin, &snapshot.run_id)?;
        remove_run_workspaces(&project.state_root, &snapshot.run_id)?;
        report.reconciled.push(ReconciledRun {
            run_id: snapshot.run_id,
            slug: snapshot.slug,
            status: decision.status.to_owned(),
            reason: decision.reason.to_owned(),
        });
    }
    Ok(report)
}

fn read_last_event(directory: &Path) -> Option<String> {
    let content = fs::read_to_string(directory.join("events.jsonl")).ok()?;
    content.lines().rev().find_map(|line| {
        serde_json::from_str::<Value>(line).ok().and_then(|event| {
            event
                .get("event")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
}

fn reachable_commits(
    origin: &GitRepo,
    base: &str,
) -> Result<std::collections::BTreeSet<String>, CoreError> {
    let output = origin
        .output(&["rev-list", base])
        .map_err(|error| recovery_error("base_read_failed", error))?;
    Ok(output
        .split(|byte| *byte == b'\n')
        .filter(|commit| !commit.is_empty())
        .map(|commit| String::from_utf8_lossy(commit).into_owned())
        .collect())
}

fn release_claim_for_run(origin: &GitRepo, run_id: &str) -> Result<(), CoreError> {
    let Some(commit) = origin
        .ref_target(CLAIM_REF)
        .map_err(|error| recovery_error("claim_read_failed", error))?
    else {
        return Ok(());
    };
    let owner = origin
        .blob_at(&commit, ".kogen/claim")
        .map_err(|error| recovery_error("claim_read_failed", error))?
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned());
    if owner.as_deref() == Some(run_id) {
        origin
            .delete_ref_cas(CLAIM_REF, &commit)
            .map_err(|error| recovery_error("claim_release_failed", error))?;
    }
    Ok(())
}

fn remove_incoming(origin: &GitRepo, run_id: &str) -> Result<(), CoreError> {
    let name = format!("refs/kogen/incoming/{run_id}");
    if let Some(commit) = origin
        .ref_target(&name)
        .map_err(|error| recovery_error("incoming_ref_read_failed", error))?
    {
        origin
            .delete_ref_cas(&name, &commit)
            .map_err(|error| recovery_error("incoming_ref_cleanup_failed", error))?;
    }
    Ok(())
}

fn remove_run_workspaces(state_root: &Path, run_id: &str) -> Result<(), CoreError> {
    let entries = match fs::read_dir(state_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(recovery_error("workspace_cleanup_failed", error)),
    };
    let prefix = format!("{run_id}-");
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&prefix) && entry.path().is_dir() {
            fs::remove_dir_all(entry.path())
                .map_err(|error| recovery_error("workspace_cleanup_failed", error))?;
        }
    }
    Ok(())
}

pub fn owner_is_alive(pid: u32, started_ms: i64) -> bool {
    if pid == 0 || started_ms <= 0 {
        return false;
    }
    let probe = Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .output();
    let Ok(probe) = probe else {
        return false;
    };
    if !probe.status.success() {
        let error = String::from_utf8_lossy(&probe.stderr).to_ascii_lowercase();
        if error.contains("operation not permitted") || error.contains("permission denied") {
            return process_start_matches(pid, started_ms).unwrap_or(true);
        }
        return false;
    }
    process_start_matches(pid, started_ms).unwrap_or(true)
}

fn process_start_matches(pid: u32, expected_ms: i64) -> Option<bool> {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !output.status.success() {
        return Some(false);
    }
    let started = String::from_utf8(output.stdout).ok()?;
    let seconds = date_seconds(started.trim())?;
    Some(seconds == expected_ms / 1000)
}

fn date_seconds(lstart: &str) -> Option<i64> {
    let mac = Command::new("date")
        .args(["-j", "-f", "%a %b %e %T %Y", lstart, "+%s"])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if mac.status.success() {
        return String::from_utf8_lossy(&mac.stdout).trim().parse().ok();
    }
    let linux = Command::new("date")
        .args(["-d", lstart, "+%s"])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    linux
        .status
        .success()
        .then(|| String::from_utf8_lossy(&linux.stdout).trim().parse().ok())
        .flatten()
}

fn valid_run_id(run_id: &str) -> bool {
    run_id.len() == 32
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn recovery_error(reason: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        error.to_string(),
        ExitCode::Environment,
    )
}

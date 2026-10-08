//! Git, approval, run-store, and queue-lock observations for the public status command.

use super::agents::{AgentStatus, observe_agents};
use super::model::{IntentFacts, StatusBoard, StatusRun, derive_board};
use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use crate::git::GitRepo;
use crate::intent::Intent;
use crate::project::{ProjectResolution, valid_slug};
use crate::recovery;
use crate::run::RunSnapshot;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct StatusReport {
    pub board: StatusBoard,
    pub agents: Vec<AgentStatus>,
    pub queue_pid: Option<u32>,
    pub now_ms: i64,
}

/// Reconcile dead owners, then derive a fresh project-wide status snapshot.
pub fn inspect(project: &ProjectResolution) -> Result<StatusReport, CoreError> {
    recovery::reconcile(project)?;
    let origin = GitRepo::new(&project.origin);
    let base = origin
        .resolve_commit(&project.base)
        .map_err(|error| status_error("base_read_failed", error))?;
    let checkout_intents = read_intents(&project.checkout)?;
    let approvals = read_approvals(&origin)?;
    let landings = read_landings(&origin, &base)?;
    let claim = read_claim_run(&origin)?;
    let runs = read_latest_runs(&project.state_root.join("runs"));
    let mut facts = Vec::new();
    for (slug, current_bytes) in checkout_intents {
        let approval = approvals.get(&slug);
        let landing = landings.get(&slug).and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate.intent.as_deref() == Some(current_bytes.as_slice()))
        });
        let latest_run = runs.get(&slug).cloned();
        let metadata = approval
            .and_then(|record| record.intent_bytes.as_deref())
            .unwrap_or(&current_bytes);
        let parsed = Intent::parse(&slug, metadata).ok();
        let facts_row = IntentFacts {
            slug: slug.clone(),
            priority: parsed
                .as_ref()
                .map_or(0, |intent| intent.frontmatter.priority),
            approval_time: approval.map_or(0, |record| record.approval_time),
            approval_commit: approval.map(|record| record.commit.clone()),
            approval: approval.and_then(|record| record.document.clone()),
            dependencies: parsed.map_or_else(Vec::new, |intent| intent.frontmatter.blocks_on),
            scheduling_error: None,
            landed_sha: landing.map(|candidate| candidate.commit.clone()),
            landed_time: landing.map_or(0, |candidate| candidate.commit_time),
            latest_run,
            claimed_run_id: claim.clone(),
        };
        facts.push(facts_row);
    }
    let now = now_ms();
    Ok(StatusReport {
        board: derive_board(facts),
        agents: observe_agents(&project.state_root.join("runs"), now),
        queue_pid: read_queue_pid(&project.state_root),
        now_ms: now,
    })
}

#[derive(Clone, Debug)]
struct ApprovalObservation {
    commit: String,
    approval_time: i64,
    document: Option<Value>,
    intent_bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct LandingObservation {
    commit: String,
    commit_time: i64,
    intent: Option<Vec<u8>>,
}

fn read_intents(checkout: &Path) -> Result<BTreeMap<String, Vec<u8>>, CoreError> {
    let root = checkout.join(".kogen/intents");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(status_error("intent_directory_unavailable", error)),
    };
    let mut intents = BTreeMap::new();
    for entry in entries.filter_map(Result::ok) {
        let kind = entry.file_type().ok();
        if !kind.is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(slug) = name.to_str().filter(|slug| valid_slug(slug)) else {
            continue;
        };
        let path = entry.path().join("intent.md");
        if let Ok(bytes) = fs::read(path) {
            intents.insert(slug.to_owned(), bytes);
        }
    }
    Ok(intents)
}

fn read_approvals(origin: &GitRepo) -> Result<BTreeMap<String, ApprovalObservation>, CoreError> {
    let refs = origin
        .output(&[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "refs/kogen/intents/",
        ])
        .map_err(|error| status_error("approval_refs_read_failed", error))?;
    let mut approvals = BTreeMap::new();
    for row in refs
        .split(|byte| *byte == b'\n')
        .filter(|row| !row.is_empty())
    {
        let Some((name, commit)) = split_nul_once(row) else {
            continue;
        };
        let name = String::from_utf8_lossy(name);
        let Some(slug) = name
            .strip_prefix("refs/kogen/intents/")
            .filter(|slug| valid_slug(slug))
        else {
            continue;
        };
        let commit = String::from_utf8_lossy(commit).trim().to_owned();
        if commit.is_empty() {
            continue;
        }
        let document = origin
            .blob_at(&commit, &format!(".kogen/intents/{slug}/approval.json"))
            .map_err(|error| status_error("approval_read_failed", error))
            .and_then(|bytes| {
                bytes
                    .map(|bytes| {
                        serde_json::from_slice::<Value>(&bytes)
                            .map_err(|error| status_error("approval_read_failed", error))
                    })
                    .transpose()
            })?;
        let intent_bytes = origin
            .blob_at(&commit, &format!(".kogen/intents/{slug}/intent.md"))
            .map_err(|error| status_error("approval_read_failed", error))?;
        let commit_time = origin
            .text(&["show", "-s", "--format=%ct", &commit])
            .map_err(|error| status_error("approval_read_failed", error))?
            .parse::<i64>()
            .unwrap_or_default();
        approvals.insert(
            slug.to_owned(),
            ApprovalObservation {
                commit,
                approval_time: commit_time,
                document,
                intent_bytes,
            },
        );
    }
    Ok(approvals)
}

fn read_landings(
    origin: &GitRepo,
    base: &str,
) -> Result<BTreeMap<String, Vec<LandingObservation>>, CoreError> {
    let output = origin
        .output(&[
            "log",
            base,
            "--format=%H%x00%(trailers:key=Kogen-Intent,valueonly)",
        ])
        .map_err(|error| status_error("landing_history_read_failed", error))?;
    let mut landings: BTreeMap<String, Vec<LandingObservation>> = BTreeMap::new();
    for row in output
        .split(|byte| *byte == b'\n')
        .filter(|row| !row.is_empty())
    {
        let Some((commit, trailer_values)) = split_nul_once(row) else {
            continue;
        };
        let commit = String::from_utf8_lossy(commit).trim().to_owned();
        for trailer in String::from_utf8_lossy(trailer_values)
            .lines()
            .map(str::trim)
        {
            if !valid_slug(trailer) {
                continue;
            }
            let path = format!(".kogen/intents/{trailer}/intent.md");
            let intent = origin
                .blob_at(&commit, &path)
                .map_err(|error| status_error("landing_tree_read_failed", error))?;
            let commit_time = origin
                .text(&["show", "-s", "--format=%ct", &commit])
                .map_err(|error| status_error("landing_history_read_failed", error))?
                .parse()
                .unwrap_or_default();
            landings
                .entry(trailer.to_owned())
                .or_default()
                .push(LandingObservation {
                    commit: commit.clone(),
                    commit_time,
                    intent,
                });
        }
    }
    Ok(landings)
}

fn read_latest_runs(root: &Path) -> BTreeMap<String, StatusRun> {
    let Ok(entries) = fs::read_dir(root) else {
        return BTreeMap::new();
    };
    let mut latest: BTreeMap<String, StatusRun> = BTreeMap::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(bytes) = fs::read(path.join("run.json")) else {
            continue;
        };
        let Ok(snapshot) = serde_json::from_slice::<RunSnapshot>(&bytes) else {
            continue;
        };
        let events = fs::read_to_string(path.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect::<Vec<_>>();
        // Cleanup and preservation events do not replace the terminal outcome.
        let last = events
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.get("event").and_then(Value::as_str),
                    Some("finished" | "reconciled")
                )
            })
            .or_else(|| events.last());
        let last_event = last
            .and_then(|event| event.get("event"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let reason = last
            .and_then(|event| event.get("reason"))
            .and_then(Value::as_str)
            .or_else(|| snapshot.fields.get("reason").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned();
        let owner_alive = snapshot.status == "running"
            && crate::recovery::owner_is_alive(snapshot.owner_pid, snapshot.owner_started_ms);
        let observation = StatusRun {
            run_id: snapshot.run_id.clone(),
            approval_commit: snapshot.approval_commit.clone(),
            status: snapshot.status.clone(),
            reason,
            last_event,
            owner_alive,
            same_approval: false,
            started_ms: snapshot.started_ms,
            snapshot: serde_json::to_value(snapshot).unwrap_or(Value::Null),
            events,
        };
        let slug = observation.slug_key();
        let replace = latest.get(&slug).is_none_or(|prior| {
            let prior_started = prior.started_ms;
            (observation.started_ms, observation.run_id.as_str())
                > (prior_started, prior.run_id.as_str())
        });
        if replace {
            latest.insert(slug, observation);
        }
    }
    latest
}

impl StatusRun {
    fn slug_key(&self) -> String {
        self.snapshot
            .get("slug")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
}

fn read_claim_run(origin: &GitRepo) -> Result<Option<String>, CoreError> {
    let Some(commit) = origin
        .ref_target("refs/kogen/claim")
        .map_err(|error| status_error("claim_read_failed", error))?
    else {
        return Ok(None);
    };
    let value = origin
        .blob_at(&commit, ".kogen/claim")
        .map_err(|error| status_error("claim_read_failed", error))?;
    Ok(value
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
        .filter(|id| !id.is_empty()))
}

fn read_queue_pid(root: &Path) -> Option<u32> {
    let pid = fs::read_to_string(root.join("queue.pid"))
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    let result = Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .output()
        .ok()?;
    result.status.success().then_some(pid)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn split_nul_once(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let index = bytes.iter().position(|byte| *byte == 0)?;
    Some((&bytes[..index], &bytes[index + 1..]))
}

fn status_error(reason: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        error.to_string(),
        ExitCode::Environment,
    )
}

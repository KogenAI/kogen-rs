//! Live agent observations stored beside their Build run journals.

use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentStatus {
    pub id: String,
    pub role: String,
    pub build: String,
    pub status: String,
    pub elapsed_ms: i64,
    pub activity: String,
    pub events: String,
}

pub(super) fn observe_agents(runs_root: &Path, now_ms: i64) -> Vec<AgentStatus> {
    let Ok(runs) = fs::read_dir(runs_root) else {
        return Vec::new();
    };
    let mut agents = Vec::new();
    for run_entry in runs.filter_map(Result::ok) {
        let run_directory = run_entry.path();
        let Ok(snapshot_bytes) = fs::read(run_directory.join("run.json")) else {
            continue;
        };
        let Ok(snapshot) = serde_json::from_slice::<Value>(&snapshot_bytes) else {
            continue;
        };
        let Some(build) = snapshot.get("run_id").and_then(Value::as_str) else {
            continue;
        };
        let run_status = snapshot
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if run_status != "running" {
            continue;
        }
        let agent_root = run_directory.join("agents");
        let Ok(entries) = fs::read_dir(&agent_root) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_agent_id(&id) {
                continue;
            }
            let events_path = entry.path().join("events.jsonl");
            let Ok(text) = fs::read_to_string(&events_path) else {
                continue;
            };
            let events = text
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect::<Vec<_>>();
            let Some(last) = events.last() else {
                continue;
            };
            let Some(status) = live_status(last) else {
                continue;
            };
            let started = events
                .iter()
                .find_map(|event| {
                    let name = event_name(event);
                    (name == "started" || name == "agent_started")
                        .then(|| timestamp(event))
                        .flatten()
                })
                .or_else(|| events.iter().find_map(timestamp))
                .unwrap_or(now_ms);
            let role = events
                .iter()
                .rev()
                .find_map(|event| field_string(event, "role"))
                .unwrap_or("agent")
                .to_owned();
            let activity = field_string(last, "activity")
                .or_else(|| field_string(last, "message"))
                .unwrap_or_else(|| event_name(last))
                .trim()
                .replace(['\n', '\r'], " ");
            agents.push(AgentStatus {
                id,
                role,
                build: build.to_owned(),
                status: status.to_owned(),
                elapsed_ms: now_ms.saturating_sub(started).max(0),
                activity: if activity.is_empty() {
                    "working".to_owned()
                } else {
                    activity
                },
                events: events_path.display().to_string(),
            });
        }
    }
    agents.sort_by(|left, right| left.build.cmp(&right.build).then(left.id.cmp(&right.id)));
    agents
}

fn live_status(event: &Value) -> Option<&'static str> {
    let status = field_string(event, "status").unwrap_or_default();
    let name = event_name(event);
    if matches!(status, "waiting") || matches!(name, "waiting" | "agent_waiting") {
        return Some("waiting");
    }
    if matches!(status, "running" | "active")
        || matches!(
            name,
            "started" | "agent_started" | "activity" | "progress" | "tool_started"
        )
    {
        return Some("running");
    }
    if matches!(
        status,
        "complete" | "completed" | "done" | "failed" | "cancelled" | "stopped"
    ) || matches!(
        name,
        "finished" | "agent_finished" | "failed" | "cancelled" | "stopped"
    ) {
        return None;
    }
    Some("running")
}

fn timestamp(event: &Value) -> Option<i64> {
    ["ts", "started_ms", "timestamp"]
        .iter()
        .find_map(|key| event.get(*key).and_then(Value::as_i64))
}

fn event_name(event: &Value) -> &str {
    event
        .get("event")
        .or_else(|| event.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn field_string<'a>(event: &'a Value, key: &str) -> Option<&'a str> {
    event.get(key).and_then(Value::as_str)
}

fn valid_agent_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

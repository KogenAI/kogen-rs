//! §2.10 Build-report assembly from the run snapshot and journal.

use kogen_core::project::ProjectResolution;
use kogen_core::status::StatusRun;
use kogen_core::status::{IntentStatus, StatusReport};
use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) fn build_report(
    intent: &IntentStatus,
    status: &StatusReport,
    project: &ProjectResolution,
) -> Value {
    let run = intent.facts.latest_run.as_ref();
    let start = run.and_then(|run| event(run, "started"));
    let finished = run.and_then(|run| event(run, "finished"));
    let verification = run.and_then(|run| last_event(run, "verification"));
    let model_stages = run.map_or_else(Vec::new, |run| events(run, "model_stage"));
    let rungs = run.map_or_else(Vec::new, build_rungs);
    let run_path = run.map(|run| project.state_root.join("runs").join(&run.run_id));
    let approval = intent.facts.approval.clone().unwrap_or(Value::Null);
    let approved_by = approval.get("by").cloned().unwrap_or(Value::Null);
    let base = approval
        .get("base_sha")
        .cloned()
        .or_else(|| {
            start
                .as_ref()
                .and_then(|event| event.get("base_sha").cloned())
        })
        .unwrap_or(Value::Null);
    let cache_hit_rate = cache_hit_rate(&model_stages);
    let used_ms = run.map_or(0, |run| {
        let end = finished
            .as_ref()
            .and_then(|event| event.get("ts"))
            .and_then(Value::as_i64)
            .unwrap_or(status.now_ms);
        (end - run.started_ms).max(0)
    });
    let paused_ms = run.map_or(0, |run| {
        events(run, "provider_wait")
            .iter()
            .map(|event| event.get("wait_ms").and_then(Value::as_i64).unwrap_or(0))
            .sum()
    });
    let budget_ms = start
        .as_ref()
        .and_then(|event| event.get("budget_ms"))
        .cloned()
        .unwrap_or(Value::Null);
    let candidate = run
        .and_then(|run| event(run, "commit_result"))
        .map(|event| without_envelope(&event))
        .unwrap_or(Value::Null);
    let best_candidate = run.and_then(|run| best_candidate(run, run_path.as_deref()));
    let audits = run.map_or_else(Vec::new, audit_rows);
    let checks = verification
        .as_ref()
        .and_then(|event| event.get("checks").cloned())
        .unwrap_or_else(|| json!([]));
    let acceptance = verification
        .as_ref()
        .and_then(|event| event.get("acceptance").cloned())
        .unwrap_or_else(|| json!([]));
    let findings = findings(&checks);
    let credential = json!({
        "source": start.as_ref().and_then(|event| event.get("credential_source").cloned()).unwrap_or(Value::Null),
        "label": start.as_ref().and_then(|event| event.get("credential_label").cloned()).unwrap_or(Value::Null),
    });
    let mut report = Map::new();
    report.insert("slug".to_owned(), json!(intent.facts.slug));
    report.insert("status".to_owned(), json!(intent.kind.as_str()));
    report.insert(
        "build_id".to_owned(),
        run.map_or(Value::Null, |run| json!(run.run_id)),
    );
    report.insert(
        "journal".to_owned(),
        run_path
            .as_ref()
            .map_or(Value::Null, |path| json!(path.display().to_string())),
    );
    report.insert(
        "verdict".to_owned(),
        finished
            .as_ref()
            .and_then(|event| event.get("verdict").cloned())
            .unwrap_or(Value::Null),
    );
    report.insert(
        "land_policy".to_owned(),
        start
            .as_ref()
            .and_then(|event| event.get("land").cloned())
            .unwrap_or_else(|| json!("green-or-advisory")),
    );
    report.insert(
        "advisory_items".to_owned(),
        finished
            .as_ref()
            .and_then(|event| event.get("advisory_items").cloned())
            .unwrap_or_else(|| json!([])),
    );
    report.insert("approval".to_owned(), approval);
    report.insert("approved_by".to_owned(), approved_by);
    report.insert("base".to_owned(), base);
    report.insert("candidate".to_owned(), candidate);
    report.insert(
        "landed_sha".to_owned(),
        intent
            .facts
            .landed_sha
            .clone()
            .map_or(Value::Null, Value::String),
    );
    report.insert("priority".to_owned(), json!(intent.facts.priority));
    report.insert("blocks_on".to_owned(), json!(intent.facts.dependencies));
    if !status.agents.is_empty() {
        report.insert(
            "agents".to_owned(),
            json!(
                status
                    .agents
                    .iter()
                    .map(|agent| json!({
                        "type": "agent",
                        "id": agent.id,
                        "role": agent.role,
                        "build": agent.build,
                        "status": agent.status,
                        "elapsed_ms": agent.elapsed_ms,
                        "activity": agent.activity,
                        "events": agent.events,
                    }))
                    .collect::<Vec<_>>()
            ),
        );
    }
    report.insert("cache_hit_rate".to_owned(), cache_hit_rate);
    report.insert("credential".to_owned(), credential);
    report.insert("rungs".to_owned(), json!(rungs));
    report.insert(
        "best_candidate".to_owned(),
        best_candidate.unwrap_or(Value::Null),
    );
    report.insert("audit".to_owned(), json!(audits));
    report.insert("acceptance".to_owned(), acceptance);
    report.insert("checks".to_owned(), project_checks(&checks));
    report.insert("model_stages".to_owned(), json!(model_stages));
    report.insert("findings".to_owned(), json!(findings));
    report.insert(
        "failures".to_owned(),
        finished
            .as_ref()
            .and_then(|event| event.get("failures").cloned())
            .unwrap_or_else(|| json!([])),
    );
    report.insert(
        "sandbox".to_owned(),
        start
            .as_ref()
            .and_then(|event| event.get("sandbox").cloned())
            .unwrap_or(Value::Null),
    );
    report.insert(
        "budget".to_owned(),
        json!({ "budget_ms": budget_ms, "used_ms": used_ms, "paused_ms": paused_ms }),
    );
    Value::Object(report)
}

fn build_rungs(run: &StatusRun) -> Vec<Value> {
    events(run, "rung_started").into_iter().map(|started| {
        let rung = started.get("rung").and_then(Value::as_str).unwrap_or_default();
        let finished = events(run, "rung_finished").into_iter().find(|event| event.get("rung").and_then(Value::as_str) == Some(rung));
        let stages = events(run, "model_stage")
            .into_iter()
            .filter(|event| event.get("rung").and_then(Value::as_str) == Some(rung))
            .collect::<Vec<_>>();
        let mut tokens = Map::new();
        for key in ["input", "cached_input", "cache_write", "output", "reasoning"] {
            let sum: i64 = stages.iter().map(|event| event.get("tokens").and_then(|tokens| tokens.get(key)).and_then(Value::as_i64).unwrap_or(0)).sum();
            tokens.insert(key.to_owned(), json!(sum));
        }
        json!({
            "rung": rung,
            "model": started.get("model").cloned().unwrap_or(Value::Null),
            "effort": started.get("effort").cloned().unwrap_or(Value::Null),
            "reason": started.get("entered_because").cloned().unwrap_or(Value::Null),
            "verdict": finished.as_ref().and_then(|event| event.get("verdict")).cloned().unwrap_or(Value::Null),
            "diff_lines": finished.as_ref().and_then(|event| event.get("diff_lines")).cloned().unwrap_or(Value::Null),
            "candidate_ref": finished.as_ref().and_then(|event| event.get("candidate_ref")).cloned().unwrap_or(Value::Null),
            "wall_ms": started.get("wall_ms").cloned().unwrap_or(json!(0)),
            "tokens": Value::Object(tokens),
        })
    }).collect()
}

fn best_candidate(run: &StatusRun, run_dir: Option<&Path>) -> Option<Value> {
    let event = events(run, "rung_finished")
        .into_iter()
        .rev()
        .find(|event| event.get("candidate_ref").is_some())?;
    let diff_path = run_dir?.join("candidate.diff");
    Some(json!({
        "rung": event.get("rung").cloned().unwrap_or(Value::Null),
        "ref": event.get("candidate_ref").cloned().unwrap_or(Value::Null),
        "diff_path": diff_path.display().to_string(),
        "verdict": event.get("verdict").cloned().unwrap_or(Value::Null),
    }))
}

fn audit_rows(run: &StatusRun) -> Vec<Value> {
    events(run, "audit")
        .into_iter()
        .flat_map(|event| {
            event
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |item| {
                    json!({
                        "rung": event.get("rung").cloned().unwrap_or(Value::Null),
                        "id": item.get("id").cloned().unwrap_or(Value::Null),
                        "verdict": item.get("verdict").cloned().unwrap_or(Value::Null),
                        "reason": item.get("reason").cloned().unwrap_or(Value::Null),
                    })
                })
        })
        .collect()
}

fn project_checks(checks: &Value) -> Value {
    Value::Array(
        checks
            .as_array()
            .into_iter()
            .flatten()
            .map(|check| {
                json!({
                    "name": check.get("name").cloned().unwrap_or(Value::Null),
                    "status": check.get("status").cloned().unwrap_or(Value::Null),
                    "excused": check.get("excused").cloned().unwrap_or(json!(false)),
                })
            })
            .collect(),
    )
}

fn findings(checks: &Value) -> Vec<Value> {
    checks
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|check| {
            check
                .get("findings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

fn cache_hit_rate(stages: &[Value]) -> Value {
    let (uncached, cached) = stages
        .iter()
        .fold((0_i64, 0_i64), |(uncached, cached), event| {
            let tokens = event.get("tokens");
            (
                uncached
                    + tokens
                        .and_then(|v| v.get("input"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                cached
                    + tokens
                        .and_then(|v| v.get("cached_input"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
            )
        });
    let total = uncached + cached;
    if total == 0 {
        Value::Null
    } else {
        json!(cached as f64 / total as f64)
    }
}

fn events(run: &StatusRun, name: &str) -> Vec<Value> {
    run.events
        .iter()
        .filter(|event| event.get("event").and_then(Value::as_str) == Some(name))
        .map(without_envelope)
        .collect()
}

fn event(run: &StatusRun, name: &str) -> Option<Value> {
    run.events
        .iter()
        .find(|event| event.get("event").and_then(Value::as_str) == Some(name))
        .cloned()
}

fn last_event(run: &StatusRun, name: &str) -> Option<Value> {
    run.events
        .iter()
        .rev()
        .find(|event| event.get("event").and_then(Value::as_str) == Some(name))
        .cloned()
}

fn without_envelope(event: &Value) -> Value {
    let mut event = event.clone();
    if let Some(fields) = event.as_object_mut() {
        fields.remove("event");
        fields.remove("ts");
    }
    event
}

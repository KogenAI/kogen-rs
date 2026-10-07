//! Slug detail text from a durable run snapshot and journal.

use kogen_core::project::ProjectResolution;
use kogen_core::status::{IntentStatus, StatusKind, StatusReport};
use serde_json::Value;

pub(in crate::handlers::status) fn render_detail(
    intent: &IntentStatus,
    report: &StatusReport,
    project: &ProjectResolution,
) -> String {
    let state = match intent.kind {
        StatusKind::Landed => format!(
            "landed {}",
            short_id(intent.facts.landed_sha.as_deref().unwrap_or(""))
        ),
        StatusKind::Approved => {
            let position = report
                .board
                .queue
                .iter()
                .position(|slug| slug == &intent.facts.slug)
                .map_or(0, |n| n + 1);
            format!("queued, {position} of {}", report.board.queue.len())
        }
        StatusKind::Building => format!("building, {}", building_detail(intent, report)),
        StatusKind::Blocked => intent
            .wait_reason
            .clone()
            .unwrap_or_else(|| "blocked".to_owned()),
        StatusKind::Draft => format!(
            "draft; review it with kogen intent approve {}",
            intent.facts.slug
        ),
        StatusKind::Failed | StatusKind::Parked | StatusKind::Interrupted => {
            format!("{}, {}", intent.kind.as_str(), reason(intent))
        }
    };
    let mut output = format!("{}: {state}\n", intent.facts.slug);
    if let Some(run) = intent
        .facts
        .latest_run
        .as_ref()
        .filter(|_| intent.kind != StatusKind::Approved)
    {
        output.push_str(&format!(
            "Build {}: {}{}\n",
            short_id(&run.run_id),
            run.status,
            if run.reason.is_empty() {
                String::new()
            } else {
                format!(", {}", run.reason)
            }
        ));
        append_build_details(&mut output, run);
        let run_dir = project.state_root.join("runs").join(&run.run_id);
        let diff = run_dir.join("candidate.diff");
        if diff.is_file() {
            output.push_str(&format!("  candidate diff: {}\n", diff.display()));
        }
        output.push_str(&format!("  journal: {}\n", run_dir.display()));
    }
    output
}

fn append_build_details(output: &mut String, run: &kogen_core::status::StatusRun) {
    let mut stages = Vec::new();
    for event in &run.events {
        if event.get("event").and_then(Value::as_str) == Some("model_stage") {
            let stage = event
                .get("stage")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let wall = event.get("wall_ms").and_then(Value::as_i64).unwrap_or(0);
            stages.push((stage.to_owned(), wall));
        }
    }
    if !stages.is_empty() {
        output.push_str("  model time: ");
        output.push_str(
            &stages
                .iter()
                .map(|(stage, millis)| format!("{stage} {}", duration(*millis)))
                .collect::<Vec<_>>()
                .join(", "),
        );
        output.push('\n');
    }
    if let Some(setup) = run
        .events
        .iter()
        .find(|event| event_name(event) == "setup_reused")
    {
        let saved = integer(setup, "saved_wall_ms").unwrap_or(0);
        output.push_str(&format!("  setup: reused (saved preparation {saved} ms)\n"));
    } else if let Some(setup) = run
        .events
        .iter()
        .find(|event| matches!(event_name(event), "setup_prepared" | "setup_finished"))
    {
        let wall = integer(setup, "wall_ms").unwrap_or(0);
        output.push_str(&format!("  setup: prepared in {wall} ms\n"));
    }
    let continuations = run
        .events
        .iter()
        .filter(|event| {
            matches!(
                event_name(event),
                "context_continuation" | "context_continued"
            )
        })
        .count();
    if continuations > 0 {
        output.push_str(&format!(
            "  context continuations: {continuations} (same approved Build; checkpoints in journal)\n"
        ));
    }
    if let Some(gate) =
        run.events.iter().rev().find(|event| {
            event_name(event) == "phase_timing" && string(event, "phase") == Some("gate")
        })
    {
        let timing = integer(gate, "wall_ms").unwrap_or(0);
        output.push_str(&format!("  gate: {}\n", duration(timing)));
    }
    if let Some(proposal) = run.events.iter().rev().find(|event| {
        matches!(
            event_name(event),
            "check_proposal" | "candidate_checks" | "check_proposal_created"
        )
    }) && let Some(paths) = proposal.get("paths").and_then(Value::as_array)
    {
        let paths = paths
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if !paths.is_empty() {
            output.push_str(&format!(
                "  candidate checks (caller approval required): {paths}\n"
            ));
        }
    }
    let acceptance = run
        .events
        .iter()
        .rev()
        .find(|event| event_name(event) == "verification")
        .and_then(|event| event.get("acceptance"))
        .and_then(Value::as_array);
    if let Some(items) = acceptance {
        let verified = items
            .iter()
            .filter(|item| matches!(string(item, "status"), Some("pass" | "passed")))
            .filter_map(|item| string(item, "id"))
            .collect::<Vec<_>>();
        let remaining = items
            .iter()
            .filter(|item| !matches!(string(item, "status"), Some("pass" | "passed")))
            .filter_map(|item| string(item, "id"))
            .collect::<Vec<_>>();
        if !verified.is_empty() || !remaining.is_empty() {
            if !verified.is_empty() {
                output.push_str(&format!(
                    "  acceptance verified: {}\n",
                    display_ids(&verified)
                ));
            }
            output.push_str(&format!(
                "  acceptance remaining: {}\n",
                display_ids(&remaining)
            ));
        }
    }
}

fn event_name(event: &Value) -> &str {
    event
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn string<'a>(event: &'a Value, key: &str) -> Option<&'a str> {
    event.get(key).and_then(Value::as_str)
}

fn integer(event: &Value, key: &str) -> Option<i64> {
    event.get(key).and_then(Value::as_i64)
}

fn display_ids(ids: &[&str]) -> String {
    if ids.is_empty() {
        "-".to_owned()
    } else {
        ids.join(", ")
    }
}

pub(super) fn building_detail(intent: &IntentStatus, report: &StatusReport) -> String {
    let Some(run) = intent.facts.latest_run.as_ref() else {
        return "starting".to_owned();
    };
    let mut stage = "starting";
    for event in &run.events {
        match event.get("event").and_then(Value::as_str) {
            Some("model_stage") => {
                stage = event.get("stage").and_then(Value::as_str).unwrap_or(stage)
            }
            Some("rung_started") => {
                stage = event.get("rung").and_then(Value::as_str).unwrap_or(stage)
            }
            _ => {}
        }
    }
    let elapsed = duration((report.now_ms - run.started_ms).max(0));
    let elapsed = if elapsed.is_empty() {
        String::new()
    } else {
        format!(", {elapsed}")
    };
    format!("{stage}{elapsed} (Build {})", short_id(&run.run_id))
}

pub(super) fn reason(intent: &IntentStatus) -> String {
    intent
        .facts
        .latest_run
        .as_ref()
        .filter(|run| !run.reason.is_empty())
        .map_or_else(|| "unknown".to_owned(), |run| run.reason.clone())
}

fn duration(milliseconds: i64) -> String {
    let seconds = milliseconds / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

pub(super) fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::{display_ids, render_detail};
    use kogen_core::project::ProjectResolution;
    use kogen_core::status::{IntentFacts, IntentStatus, StatusKind, StatusReport, StatusRun};

    #[test]
    fn empty_acceptance_progress_lists_render_a_dash() {
        assert_eq!(display_ids(&[]), "-");
        assert_eq!(display_ids(&["A1", "A2"]), "A1, A2");
    }

    #[test]
    fn building_slug_status_includes_the_required_prefix() {
        let intent = IntentStatus {
            facts: IntentFacts {
                slug: "greet".to_owned(),
                latest_run: Some(StatusRun {
                    run_id: "12345678abcdef".to_owned(),
                    status: "running".to_owned(),
                    started_ms: 1_000,
                    ..StatusRun::default()
                }),
                ..IntentFacts::default()
            },
            kind: StatusKind::Building,
            wait_reason: None,
        };
        let report = StatusReport {
            board: Default::default(),
            agents: Vec::new(),
            queue_pid: None,
            now_ms: 1_000,
        };
        let project = ProjectResolution {
            checkout: std::env::temp_dir(),
            origin: std::env::temp_dir(),
            base: "main".to_owned(),
            state_root: std::env::temp_dir(),
            config: None,
        };

        let output = render_detail(&intent, &report, &project);
        assert!(
            output.starts_with("greet: building, starting, 0s (Build 12345678)\n"),
            "{output:?}"
        );
    }
}

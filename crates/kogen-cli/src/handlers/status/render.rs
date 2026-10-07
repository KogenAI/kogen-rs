//! Overview status renderers.

mod detail;
pub(super) use detail::render_detail;

use kogen_core::status::{IntentStatus, StatusKind, StatusReport};
use serde_json::json;

pub(super) fn render_json_lines(report: &StatusReport) -> String {
    let mut output = String::new();
    for intent in &report.board.intents {
        let row = json!({
            "slug": intent.facts.slug,
            "status": intent.kind.as_str(),
            "build_id": intent.facts.latest_run.as_ref().map(|run| run.run_id.clone()),
            "landed_sha": intent.facts.landed_sha,
            "priority": intent.facts.priority,
            "blocks_on": intent.facts.dependencies,
        });
        output.push_str(&serde_json::to_string(&row).unwrap_or_else(|_| "{}".to_owned()));
        output.push('\n');
    }
    for agent in &report.agents {
        let row = json!({
            "type": "agent",
            "id": agent.id,
            "role": agent.role,
            "build": agent.build,
            "status": agent.status,
            "elapsed_ms": agent.elapsed_ms,
            "activity": agent.activity,
            "events": agent.events,
        });
        output.push_str(&serde_json::to_string(&row).unwrap_or_else(|_| "{}".to_owned()));
        output.push('\n');
    }
    output
}

pub(super) fn render_overview(report: &StatusReport) -> String {
    let mut output = String::new();
    let queue = &report.board.queue;
    if let Some(pid) = report.queue_pid {
        output.push_str(&format!("Queue: running (pid {pid})\n"));
    } else if !queue.is_empty() {
        output.push_str(&format!(
            "Queue: stopped, {} waiting; start it with kogen queue start\n",
            queue.len()
        ));
    } else {
        output.push_str("Queue: stopped\n");
    }
    if report.queue_pid.is_none()
        && let Some(slug) = queue.first()
        && let Some(intent) = find(report, slug)
    {
        let dependencies = if intent.facts.dependencies.is_empty() {
            "no dependencies"
        } else {
            "dependencies delivered"
        };
        output.push_str(&format!(
            "Next: {slug} (priority {}; {dependencies}; ties by approval time and slug)\n",
            intent.facts.priority
        ));
    }
    if report.board.intents.is_empty() {
        output.push_str("No Intents.\n");
        render_agents(&mut output, report);
        return output;
    }
    render_section(&mut output, report, "Building", StatusKind::Building);
    if !queue.is_empty() {
        output.push_str("Queued:\n");
        for slug in queue {
            output.push_str(&format!("  {slug}\n"));
        }
    }
    render_section(&mut output, report, "Blocked", StatusKind::Blocked);
    render_section(&mut output, report, "Failed", StatusKind::Failed);
    render_section(&mut output, report, "Parked", StatusKind::Parked);
    render_section(&mut output, report, "Interrupted", StatusKind::Interrupted);
    render_section(&mut output, report, "Drafts", StatusKind::Draft);
    render_landed(&mut output, report);
    render_agents(&mut output, report);
    output
}

fn render_agents(output: &mut String, report: &StatusReport) {
    if report.agents.is_empty() {
        return;
    }
    output.push_str("Agents:\n");
    for agent in &report.agents {
        output.push_str(&format!(
            "  {} {} Build={} {} elapsed_ms={} {}\n    events: {}\n",
            agent.id,
            agent.role,
            agent.build,
            agent.status,
            agent.elapsed_ms,
            agent.activity,
            agent.events
        ));
    }
}

fn render_section(output: &mut String, report: &StatusReport, title: &str, kind: StatusKind) {
    let rows = report
        .board
        .intents
        .iter()
        .filter(|intent| intent.kind == kind)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }
    output.push_str(&format!("{title}:\n"));
    let width = rows
        .iter()
        .map(|intent| intent.facts.slug.len())
        .max()
        .unwrap_or(0);
    for intent in rows {
        let detail = match kind {
            StatusKind::Building => detail::building_detail(intent, report),
            StatusKind::Blocked => intent
                .wait_reason
                .clone()
                .unwrap_or_else(|| "blocked".to_owned()),
            StatusKind::Failed | StatusKind::Parked | StatusKind::Interrupted => format!(
                "{} (Build {})",
                detail::reason(intent),
                detail::short_id(
                    intent
                        .facts
                        .latest_run
                        .as_ref()
                        .map(|run| run.run_id.as_str())
                        .unwrap_or("")
                )
            ),
            StatusKind::Draft => String::new(),
            _ => String::new(),
        };
        if detail.is_empty() {
            output.push_str(&format!("  {}\n", intent.facts.slug));
        } else {
            output.push_str(&format!("  {:width$}  {detail}\n", intent.facts.slug));
        }
    }
}

fn render_landed(output: &mut String, report: &StatusReport) {
    let mut landed = report
        .board
        .intents
        .iter()
        .filter(|intent| intent.kind == StatusKind::Landed)
        .collect::<Vec<_>>();
    if landed.is_empty() {
        return;
    }
    landed.sort_by(|left, right| {
        right
            .facts
            .landed_time
            .cmp(&left.facts.landed_time)
            .then_with(|| right.facts.slug.cmp(&left.facts.slug))
    });
    output.push_str(&format!("Landed ({}):\n", landed.len()));
    for intent in landed.iter().take(5) {
        output.push_str(&format!(
            "  {}  {}\n",
            intent.facts.slug,
            detail::short_id(intent.facts.landed_sha.as_deref().unwrap_or(""))
        ));
    }
    if landed.len() > 5 {
        output.push_str(&format!("  and {} earlier\n", landed.len() - 5));
    }
}

fn find<'a>(report: &'a StatusReport, slug: &str) -> Option<&'a IntentStatus> {
    report
        .board
        .intents
        .iter()
        .find(|intent| intent.facts.slug == slug)
}

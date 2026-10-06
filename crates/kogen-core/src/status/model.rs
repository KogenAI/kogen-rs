//! Shared status precedence, dependency checks, and queue ordering.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusKind {
    Building,
    Approved,
    Blocked,
    Failed,
    Parked,
    Interrupted,
    Draft,
    Landed,
}

impl StatusKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Building => "building",
            Self::Approved => "approved",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Parked => "parked",
            Self::Interrupted => "interrupted",
            Self::Draft => "draft",
            Self::Landed => "landed",
        }
    }
}

pub fn is_status(status: &str) -> bool {
    matches!(
        status,
        "approved" | "building" | "failed" | "parked" | "draft" | "landed" | "interrupted"
    )
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusRun {
    pub run_id: String,
    pub approval_commit: String,
    pub status: String,
    pub reason: String,
    pub last_event: String,
    pub owner_alive: bool,
    pub same_approval: bool,
    pub started_ms: i64,
    pub snapshot: Value,
    pub events: Vec<Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IntentFacts {
    pub slug: String,
    pub priority: i64,
    pub approval_time: i64,
    pub approval_commit: Option<String>,
    pub approval: Option<Value>,
    pub dependencies: Vec<String>,
    pub scheduling_error: Option<String>,
    pub landed_sha: Option<String>,
    pub landed_time: i64,
    pub latest_run: Option<StatusRun>,
    pub claimed_run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IntentStatus {
    pub facts: IntentFacts,
    pub kind: StatusKind,
    pub wait_reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusBoard {
    pub intents: Vec<IntentStatus>,
    /// Slugs in the deterministic order used by the serial queue.
    pub queue: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DependencyState {
    pub status: String,
    pub dependencies: Vec<String>,
    pub scheduling_error: Option<String>,
}

/// Derive statuses in precedence order, then block approved rows and sort the queue.
#[must_use]
pub fn derive_board(facts: Vec<IntentFacts>) -> StatusBoard {
    let mut intents = facts
        .into_iter()
        .map(|facts| {
            let kind = classify_facts(&facts);
            IntentStatus {
                facts,
                kind,
                wait_reason: None,
            }
        })
        .collect::<Vec<_>>();

    let dependencies = intents
        .iter()
        .map(|intent| {
            (
                intent.facts.slug.clone(),
                DependencyState {
                    status: intent.kind.as_str().to_owned(),
                    dependencies: intent.facts.dependencies.clone(),
                    scheduling_error: intent.facts.scheduling_error.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for intent in &mut intents {
        if intent.kind == StatusKind::Approved
            && let Some(reason) = dependency_reason(&dependencies, &intent.facts.slug)
        {
            intent.kind = StatusKind::Blocked;
            intent.wait_reason = Some(reason);
        }
    }

    let mut queue = intents
        .iter()
        .filter(|intent| intent.kind == StatusKind::Approved)
        .map(|intent| {
            (
                intent.facts.priority,
                intent.facts.approval_time,
                intent.facts.slug.clone(),
            )
        })
        .collect::<Vec<_>>();
    queue.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let queue = queue.into_iter().map(|(_, _, slug)| slug).collect();
    intents.sort_by(|left, right| left.facts.slug.cmp(&right.facts.slug));
    StatusBoard { intents, queue }
}

#[must_use]
pub fn classify_facts(facts: &IntentFacts) -> StatusKind {
    if facts.landed_sha.is_some() {
        return StatusKind::Landed;
    }
    let run = facts.latest_run.as_ref();
    let claimed = run.is_some_and(|run| {
        facts.claimed_run_id.as_deref() == Some(run.run_id.as_str()) && run.status == "running"
    });
    if claimed {
        if run.is_some_and(|run| is_dead_interrupted(facts, run)) {
            return StatusKind::Interrupted;
        }
        return StatusKind::Building;
    }
    let Some(approval_commit) = facts.approval_commit.as_deref() else {
        return StatusKind::Draft;
    };
    let Some(run) = run.filter(|run| run.approval_commit == approval_commit) else {
        return StatusKind::Approved;
    };
    if is_dead_interrupted(facts, run) {
        return StatusKind::Interrupted;
    }
    if run.status == "failed" && run.reason == "interrupted" {
        return StatusKind::Interrupted;
    }
    match run.status.as_str() {
        "failed" => StatusKind::Failed,
        "parked" => StatusKind::Parked,
        // A stopped Build leaves the current approval queued for a later drain.
        _ => StatusKind::Approved,
    }
}

fn current_approval(facts: &IntentFacts, run: &StatusRun) -> bool {
    run.same_approval || facts.approval_commit.as_deref() == Some(run.approval_commit.as_str())
}

fn is_dead_interrupted(facts: &IntentFacts, run: &StatusRun) -> bool {
    run.status == "running"
        && run.last_event == "interrupted"
        && !run.owner_alive
        && current_approval(facts, run)
}

/// Return the first deterministic scheduling/dependency refusal for an approved Intent.
#[must_use]
pub fn dependency_reason(
    entries: &BTreeMap<String, DependencyState>,
    slug: &str,
) -> Option<String> {
    let item = entries.get(slug)?;
    if item.status != "approved" && item.status != "blocked" {
        return None;
    }
    if let Some(reason) = &item.scheduling_error {
        return Some(reason.clone());
    }
    if item.dependencies.is_empty() {
        return None;
    }
    if item.dependencies.iter().any(|name| name == "BAD") {
        return Some("invalid dependencies: BAD".to_owned());
    }
    let unknown = item
        .dependencies
        .iter()
        .filter(|dependency| !entries.contains_key(*dependency))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Some(format!("unknown dependencies: {}", unknown.join(", ")));
    }
    if let Some(cycle) = cycle_from(entries, slug) {
        return Some(format!("dependency cycle: {}", cycle.join(" -> ")));
    }
    let pending = item
        .dependencies
        .iter()
        .filter(|dependency| {
            entries
                .get(*dependency)
                .is_some_and(|entry| entry.status != "landed")
        })
        .cloned()
        .collect::<Vec<_>>();
    (!pending.is_empty())
        .then(|| format!("waiting for delivered dependencies: {}", pending.join(", ")))
}

fn cycle_from(entries: &BTreeMap<String, DependencyState>, start: &str) -> Option<Vec<String>> {
    fn visit(
        entries: &BTreeMap<String, DependencyState>,
        node: &str,
        start: &str,
        stack: &mut Vec<String>,
        seen: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        if let Some(index) = stack.iter().position(|item| item == node) {
            let mut cycle = stack[index..].to_vec();
            cycle.push(node.to_owned());
            return (node == start || cycle.iter().any(|item| item == start)).then_some(cycle);
        }
        if !seen.insert(node.to_owned()) {
            return None;
        }
        stack.push(node.to_owned());
        let mut children = entries
            .get(node)
            .filter(|entry| entry.status == "approved" || entry.status == "blocked")
            .map(|entry| entry.dependencies.clone())
            .unwrap_or_default();
        children.sort();
        for child in children {
            if !entries.contains_key(&child) {
                continue;
            }
            if let Some(cycle) = visit(entries, &child, start, stack, seen) {
                return Some(cycle);
            }
        }
        stack.pop();
        None
    }

    visit(entries, start, start, &mut Vec::new(), &mut BTreeSet::new())
}

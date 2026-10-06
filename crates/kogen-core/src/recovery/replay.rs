//! Pure recovery transition shared by the production reconciler and xspec.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRun {
    pub status: String,
    pub alive: bool,
    pub on_base: bool,
    pub last_event: String,
    pub incoming: bool,
    pub queued: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecoveryFact {
    pub id: String,
    pub status: String,
    pub alive: bool,
    pub on_base: bool,
    pub last_event: String,
    pub incoming: bool,
    pub queued: bool,
    pub claim: bool,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryDecision {
    pub status: &'static str,
    pub reason: &'static str,
}

/// Decide the terminal state for one dead running owner; reachable landing wins.
#[must_use]
pub fn recovery_decision(
    status: &str,
    alive: bool,
    on_base: bool,
    last_event: &str,
) -> Option<RecoveryDecision> {
    if status != "running" || alive {
        return None;
    }
    Some(if on_base {
        RecoveryDecision {
            status: "landed",
            reason: "reconciled",
        }
    } else if last_event == "interrupted" {
        RecoveryDecision {
            status: "failed",
            reason: "interrupted",
        }
    } else {
        RecoveryDecision {
            status: "failed",
            reason: "crashed",
        }
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecoveryEvent {
    Put(RecoveryFact),
    Recover,
    Reapprove(String),
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct RecoveryObservation {
    pub last: String,
    pub line: String,
    pub claim: String,
    pub runs: BTreeMap<String, RecoveryRun>,
}

#[derive(Clone, Debug, Default)]
pub struct RecoveryModel {
    runs: BTreeMap<String, RecoveryRun>,
    claim: String,
    line: String,
    last: String,
}

impl RecoveryModel {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last: "ok".to_owned(),
            ..Self::default()
        }
    }

    pub fn apply(&mut self, event: RecoveryEvent) -> RecoveryObservation {
        match event {
            RecoveryEvent::Put(fact) => self.put(fact),
            RecoveryEvent::Recover => self.recover(),
            RecoveryEvent::Reapprove(id) => self.reapprove(&id),
        }
        self.observe()
    }

    #[must_use]
    pub fn observe(&self) -> RecoveryObservation {
        RecoveryObservation {
            last: self.last.clone(),
            line: self.line.clone(),
            claim: self.claim.clone(),
            runs: self.runs.clone(),
        }
    }

    fn put(&mut self, fact: RecoveryFact) {
        if !known(&fact.id) {
            self.error("unknown_run");
            return;
        }
        if !known_status(&fact.status) {
            self.error("bad_status");
            return;
        }
        if fact.incoming && fact.status != "running" {
            self.error("bad_run");
            return;
        }
        if fact.status == "landed" && !fact.on_base {
            self.error("bad_run");
            return;
        }
        if fact.claim && fact.status != "running" {
            self.error("bad_run");
            return;
        }
        if fact.claim && !self.claim.is_empty() && self.claim != fact.id {
            self.error("claim_held");
            return;
        }
        if fact.queued && !matches!(fact.status.as_str(), "approved" | "stopped") {
            self.error("bad_run");
            return;
        }
        if fact.queued
            && fact.status == "failed"
            && matches!(fact.reason.as_str(), "crashed" | "interrupted")
        {
            self.error("bad_run");
            return;
        }
        self.runs.insert(
            fact.id.clone(),
            RecoveryRun {
                status: fact.status,
                alive: fact.alive,
                on_base: fact.on_base,
                last_event: fact.last_event,
                incoming: fact.incoming,
                queued: fact.queued,
                reason: fact.reason,
            },
        );
        if fact.claim {
            self.claim = fact.id.clone();
        } else if self.claim == fact.id {
            self.claim.clear();
        }
        self.last = "ok".to_owned();
        self.line = "stored".to_owned();
    }

    fn recover(&mut self) {
        let before = self.clone();
        for id in ["r1", "r2"] {
            let Some(run) = self.runs.get(id).cloned() else {
                continue;
            };
            if let Some(decision) =
                recovery_decision(&run.status, run.alive, run.on_base, &run.last_event)
            {
                self.finish(id, decision.status, decision.reason);
            }
        }
        if self.line == before.line && self.claim == before.claim {
            self.last = "ok".to_owned();
            self.line = "unchanged".to_owned();
        }
    }

    fn finish(&mut self, id: &str, status: &str, reason: &str) {
        if let Some(run) = self.runs.get_mut(id) {
            run.status = status.to_owned();
            run.reason = reason.to_owned();
            run.incoming = false;
            run.queued = false;
        }
        if self.claim == id {
            self.claim.clear();
        }
        self.last = "ok".to_owned();
        self.line = reason.to_owned();
    }

    fn reapprove(&mut self, id: &str) {
        if !known(id) || !self.runs.contains_key(id) {
            self.error("unknown_run");
            return;
        }
        let run = self.runs.get(id).cloned().unwrap_or_default();
        if run.status == "landed" {
            self.error("already_landed");
            return;
        }
        if run.status == "failed" && matches!(run.reason.as_str(), "crashed" | "interrupted") {
            if let Some(run) = self.runs.get_mut(id) {
                run.status = "approved".to_owned();
                run.queued = true;
                run.reason.clear();
            }
            self.last = "ok".to_owned();
            self.line = "reapproved".to_owned();
        } else {
            self.error("not_recoverable");
        }
    }

    fn error(&mut self, reason: &str) {
        self.last = reason.to_owned();
        self.line = reason.to_owned();
    }
}

fn known(id: &str) -> bool {
    matches!(id, "r1" | "r2")
}
fn known_status(status: &str) -> bool {
    matches!(
        status,
        "running" | "landed" | "failed" | "parked" | "stopped" | "approved"
    )
}

//! Recovery decisions shared by the production reconciler and deterministic replay effects.

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
    pub work: bool,
    pub preserved: bool,
    pub preserve_ok: bool,
    pub cleanup_pending: bool,
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
    pub work: bool,
    pub preserved: bool,
    pub preserve_ok: bool,
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
    PreservationResult { id: String, ok: bool },
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
            RecoveryEvent::PreservationResult { id, ok } => {
                if let Some(run) = self.runs.get_mut(&id) {
                    run.preserve_ok = ok;
                } else {
                    self.error("unknown_run");
                }
            }
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
                work: fact.work,
                preserved: fact.preserved,
                preserve_ok: fact.preserve_ok,
                cleanup_pending: false,
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
            if run.cleanup_pending {
                self.finish(id, &run.status, &run.reason);
            } else if let Some(decision) =
                recovery_decision(&run.status, run.alive, run.on_base, &run.last_event)
            {
                self.finish(id, decision.status, decision.reason);
            }
        }
        if self.runs.values().any(|run| run.cleanup_pending) {
            self.last = "ok".to_owned();
            self.line = "cleanup_failure".to_owned();
        } else if self.runs == before.runs && self.claim == before.claim {
            self.last = "ok".to_owned();
            self.line = "unchanged".to_owned();
        }
    }

    fn finish(&mut self, id: &str, status: &str, reason: &str) {
        if let Some(run) = self.runs.get_mut(id) {
            let needs_copy = run.work || run.incoming;
            let preserved = run.preserved || (needs_copy && run.preserve_ok);
            let pending = needs_copy && !preserved;
            run.status = status.to_owned();
            run.reason = reason.to_owned();
            run.queued = false;
            run.preserved = preserved;
            run.work &= pending;
            run.incoming = run.incoming && pending;
            run.cleanup_pending = pending;
            self.line = if pending {
                "cleanup_failure".to_owned()
            } else {
                reason.to_owned()
            };
        }
        if self.claim == id {
            self.claim.clear();
        }
        self.last = "ok".to_owned();
    }

    fn reapprove(&mut self, id: &str) {
        if !known(id) || !self.runs.contains_key(id) {
            self.error("unknown_run");
            return;
        }
        let run = self.runs.get(id).cloned().unwrap_or_default();
        if run.cleanup_pending {
            self.error("cleanup_pending");
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::{RecoveryEvent, RecoveryFact, RecoveryModel};

    #[test]
    fn failed_preservation_retains_work_and_incoming_until_retry_succeeds() {
        let mut replay = RecoveryModel::new();
        replay.apply(RecoveryEvent::Put(RecoveryFact {
            id: "r1".to_owned(),
            status: "running".to_owned(),
            alive: false,
            on_base: false,
            last_event: "other".to_owned(),
            incoming: true,
            queued: false,
            claim: true,
            reason: String::new(),
            work: true,
            preserved: false,
            preserve_ok: false,
        }));

        let failed = replay.apply(RecoveryEvent::Recover);
        let run = &failed.runs["r1"];
        assert_eq!(run.status, "failed");
        assert!(run.work && run.incoming && run.cleanup_pending);
        assert!(!run.preserved);
        assert!(failed.claim.is_empty());

        replay.apply(RecoveryEvent::PreservationResult {
            id: "r1".to_owned(),
            ok: true,
        });
        let retried = replay.apply(RecoveryEvent::Recover);
        let run = &retried.runs["r1"];
        assert!(run.preserved);
        assert!(!run.work && !run.incoming && !run.cleanup_pending);
        assert_eq!(run.reason, "crashed");
    }
}

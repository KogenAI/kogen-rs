//! Pure serial-drain transitions shared by the CLI and xspec adapter.
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueApproval {
    pub slug: String,
    /// Commit time of the current immutable approval, in Unix seconds.
    pub approval_time: i64,
    pub priority: i64,
    pub approval_hash: String,
    pub approval_commit: String,
}

impl QueueApproval {
    #[must_use]
    pub fn new(
        slug: impl Into<String>,
        approval_time: i64,
        priority: i64,
        approval_hash: impl Into<String>,
    ) -> Self {
        Self {
            slug: slug.into(),
            approval_time,
            priority,
            approval_hash: approval_hash.into(),
            approval_commit: String::new(),
        }
    }

    #[must_use]
    pub fn with_approval_commit(mut self, commit: impl Into<String>) -> Self {
        self.approval_commit = commit.into();
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DrainOutcome {
    Landed,
    Failed,
    Parked,
    StoppedEnvironment,
    StoppedProvider,
    StoppedController,
    Skipped,
    Other(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueEvent {
    Enqueue(QueueApproval),
    Start,
    Die,
    Halt,
    Outcome(DrainOutcome),
    Release,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct QueueObservation {
    pub last: String,
    pub line: String,
    pub exit: i32,
    pub held: bool,
    pub alive: bool,
    pub stop: bool,
    pub phase: String,
    pub current: String,
    pub queue: Vec<String>,
    pub built: u32,
    pub landed: u32,
}

/// The authoritative pure scheduler used by production drains and private replay.
#[derive(Clone, Debug, Default)]
pub struct QueueScheduler {
    held: bool,
    alive: bool,
    stop: bool,
    current: Option<QueueApproval>,
    waiting: BTreeMap<String, QueueApproval>,
    attempted: BTreeSet<String>,
    built: u32,
    landed: u32,
    exit: i32,
    last: String,
    line: String,
}

impl QueueScheduler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last: "ok".to_owned(),
            line: String::new(),
            ..Self::default()
        }
    }

    pub fn apply(&mut self, event: QueueEvent) -> QueueObservation {
        match event {
            QueueEvent::Enqueue(approval) => self.enqueue(approval),
            QueueEvent::Start => self.start(),
            QueueEvent::Die => self.die(),
            QueueEvent::Halt => self.halt(),
            QueueEvent::Outcome(outcome) => self.outcome(outcome),
            QueueEvent::Release => self.release(),
        }
        self.observe()
    }

    #[must_use]
    pub fn observe(&self) -> QueueObservation {
        QueueObservation {
            last: self.last.clone(),
            line: self.line.clone(),
            exit: self.exit,
            held: self.held,
            alive: self.alive,
            stop: self.stop,
            phase: if self.current.is_some() {
                "building"
            } else {
                "idle"
            }
            .to_owned(),
            current: self
                .current
                .as_ref()
                .map_or_else(String::new, |approval| approval.slug.clone()),
            queue: self
                .ordered()
                .into_iter()
                .map(|item| item.slug.clone())
                .collect(),
            built: self.built,
            landed: self.landed,
        }
    }

    fn enqueue(&mut self, approval: QueueApproval) {
        self.waiting.insert(approval.slug.clone(), approval);
        self.last = "ok".to_owned();
        self.line = "enqueued".to_owned();
    }

    fn start(&mut self) {
        if self.held && self.alive {
            self.last = "ok".to_owned();
            self.line = "already_running".to_owned();
            self.exit = 0;
            return;
        }
        self.held = true;
        self.alive = true;
        self.stop = false;
        self.attempted.clear();
        self.built = 0;
        self.landed = 0;
        self.current = None;
        self.launch();
    }

    fn die(&mut self) {
        if !self.held {
            self.error("no_process");
            return;
        }
        self.alive = false;
        self.last = "ok".to_owned();
        self.line = "owner_dead".to_owned();
    }

    fn halt(&mut self) {
        if !self.held || !self.alive {
            self.last = "ok".to_owned();
            self.line = "not_running".to_owned();
            self.exit = 0;
            return;
        }
        self.stop = true;
        self.last = "ok".to_owned();
        self.line = "stopping".to_owned();
    }

    fn release(&mut self) {
        if !self.held || !self.alive {
            self.error("not_owner");
        } else if self.current.is_some() {
            self.error("build_in_flight");
        } else {
            self.finish_drain("released", 0);
        }
    }

    fn outcome(&mut self, outcome: DrainOutcome) {
        if self.current.is_none() || !self.alive {
            self.error("not_building");
            return;
        }
        match outcome {
            DrainOutcome::Skipped => {
                self.drop_current();
                self.launch();
            }
            DrainOutcome::Landed | DrainOutcome::Failed | DrainOutcome::Parked => {
                self.built += 1;
                if outcome == DrainOutcome::Landed {
                    self.landed += 1;
                }
                self.drop_current();
                self.launch();
            }
            DrainOutcome::StoppedEnvironment
            | DrainOutcome::StoppedProvider
            | DrainOutcome::StoppedController => {
                self.built += 1;
                let exit = match outcome {
                    DrainOutcome::StoppedEnvironment => 3,
                    DrainOutcome::StoppedProvider => 4,
                    DrainOutcome::StoppedController => 70,
                    _ => unreachable!("stopped class was matched above"),
                };
                self.finish_drain("stopped_because", exit);
            }
            DrainOutcome::Other(_) => self.error("unknown_outcome"),
        }
    }

    fn launch(&mut self) {
        if self.stop {
            self.finish_drain("stopped_on_request", 0);
            return;
        }
        let Some(next_slug) = self
            .ordered()
            .into_iter()
            .find(|approval| !self.attempted.contains(&approval.slug))
            .map(|approval| approval.slug.clone())
        else {
            let line = if self.built == 0 {
                "nothing_to_build"
            } else {
                "done"
            };
            let exit = if self.built == 0 || self.landed == self.built {
                0
            } else {
                1
            };
            self.finish_drain(line, exit);
            return;
        };
        self.attempted.insert(next_slug.clone());
        self.current = self.waiting.get(&next_slug).cloned();
        self.last = "ok".to_owned();
        self.line = "building".to_owned();
        self.exit = 0;
    }

    fn ordered(&self) -> Vec<&QueueApproval> {
        let mut approvals = self.waiting.values().collect::<Vec<_>>();
        approvals.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.approval_time.cmp(&right.approval_time))
                .then_with(|| left.slug.cmp(&right.slug))
        });
        approvals
            .into_iter()
            .filter(|approval| !self.attempted.contains(&approval.slug))
            .collect()
    }

    fn drop_current(&mut self) {
        if let Some(processing) = self.current.take()
            && self
                .waiting
                .get(&processing.slug)
                .is_some_and(|latest| same_approval(&processing, latest))
        {
            self.waiting.remove(&processing.slug);
        }
    }

    fn finish_drain(&mut self, line: &str, exit: i32) {
        self.held = false;
        self.alive = false;
        self.stop = false;
        self.current = None;
        self.attempted.clear();
        self.last = "ok".to_owned();
        self.line = line.to_owned();
        self.exit = exit;
    }

    fn error(&mut self, code: &str) {
        self.last = code.to_owned();
        self.line = code.to_owned();
    }
}

fn same_approval(processing: &QueueApproval, latest: &QueueApproval) -> bool {
    if !processing.approval_commit.is_empty() && !latest.approval_commit.is_empty() {
        processing.approval_commit == latest.approval_commit
    } else {
        processing.approval_hash.is_empty()
            || latest.approval_hash.is_empty()
            || (processing.approval_hash == latest.approval_hash
                && processing.approval_time == latest.approval_time)
    }
}

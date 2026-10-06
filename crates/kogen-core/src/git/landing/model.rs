//! Pure §3.9 landing transitions, shared by the effect driver and xspec.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RebaseKind {
    Green,
    Conflict,
    Red,
    Impossible,
}

impl RebaseKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Conflict => "conflict",
            Self::Red => "red",
            Self::Impossible => "impossible",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "green" => Some(Self::Green),
            "conflict" => Some(Self::Conflict),
            "red" => Some(Self::Red),
            "impossible" => Some(Self::Impossible),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LandingEvent {
    Record,
    Lock,
    Head(&'static str),
    Push,
    Cas(bool),
    Worktree(bool),
    Drop(bool),
    Again,
    Rebase(RebaseKind),
    Repair(RepairKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairKind {
    Green,
    Red,
    Spent,
}

#[derive(Clone, Debug, Default, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LandingObservation {
    pub last: String,
    pub line: String,
    pub exit: i32,
    pub phase: String,
    pub status: String,
    pub reason: String,
    pub recorded: bool,
    pub incoming: bool,
    pub on_base: bool,
    pub claim: bool,
    pub tries: u32,
    pub repairs: u32,
    pub delay: u64,
    pub warning: bool,
    pub cleanup: bool,
}

#[derive(Clone, Debug)]
struct State {
    observation: LandingObservation,
    head_ok: bool,
}

#[derive(Clone, Debug)]
pub struct LandingModel {
    state: State,
}

impl LandingModel {
    #[must_use]
    pub fn new() -> Self {
        let observation = LandingObservation {
            last: "ok".to_owned(),
            phase: "idle".to_owned(),
            ..LandingObservation::default()
        };
        Self {
            state: State {
                observation,
                head_ok: false,
            },
        }
    }

    #[must_use]
    pub fn observe(&self) -> LandingObservation {
        self.state.observation.clone()
    }

    pub fn apply(&mut self, event: LandingEvent) -> LandingObservation {
        match event {
            LandingEvent::Record => self.record(),
            LandingEvent::Lock => self.transient_if_open(),
            LandingEvent::Head(kind) => self.head(kind),
            LandingEvent::Push => self.push(),
            LandingEvent::Cas(won) => self.cas(won),
            LandingEvent::Worktree(dirty) => self.worktree(dirty),
            LandingEvent::Drop(ok) => self.drop_incoming(ok),
            LandingEvent::Again => self.again(),
            LandingEvent::Rebase(kind) => self.rebase(kind),
            LandingEvent::Repair(kind) => self.repair(kind),
        }
        self.observe()
    }

    fn record(&mut self) {
        if self.phase() != "idle" {
            self.error("not_idle");
            return;
        }
        self.state = Self::new().state;
        let obs = &mut self.state.observation;
        obs.phase = "recorded".to_owned();
        obs.recorded = true;
        obs.claim = true;
        obs.status = "running".to_owned();
        obs.line = "landing_prepared".to_owned();
    }

    fn transient_if_open(&mut self) {
        if !matches!(self.phase(), "recorded" | "incoming") {
            self.error("not_open");
            return;
        }
        self.transient();
    }

    fn head(&mut self, kind: &str) {
        if self.phase() != "recorded" {
            self.error("not_recorded");
        } else if kind == "ok" {
            self.state.head_ok = true;
            self.success("head_ok");
        } else if matches!(kind, "not_fast_forward" | "tree_mismatch") {
            self.stop_controller(kind);
        } else {
            self.error("bad_head");
        }
    }

    fn push(&mut self) {
        if self.phase() != "recorded" || !self.state.head_ok {
            self.error("not_ready");
            return;
        }
        self.state.observation.phase = "incoming".to_owned();
        self.state.observation.incoming = true;
        self.success("pushed");
    }

    fn cas(&mut self, won: bool) {
        if self.phase() != "incoming" {
            self.error("not_incoming");
        } else if won {
            self.state.observation.phase = "based".to_owned();
            self.state.observation.on_base = true;
            self.success("cas");
        } else {
            self.transient();
        }
    }

    fn worktree(&mut self, dirty: bool) {
        if self.phase() != "based" {
            self.error("not_based");
        } else {
            self.state.observation.warning |= dirty;
            self.success(if dirty {
                "landing_warning"
            } else {
                "worktree_clean"
            });
        }
    }

    fn drop_incoming(&mut self, ok: bool) {
        if self.phase() != "based" {
            self.error("not_based");
        } else {
            let obs = &mut self.state.observation;
            obs.phase = "landed".to_owned();
            obs.status = "landed".to_owned();
            obs.reason.clear();
            obs.claim = false;
            obs.incoming = !ok;
            obs.cleanup = !ok;
            obs.delay = 0;
            obs.exit = 0;
            obs.last = "ok".to_owned();
            obs.line = if ok { "landed" } else { "cleanup_failure" }.to_owned();
        }
    }

    fn again(&mut self) {
        if self.phase() != "retrying" {
            self.error("not_retrying");
        } else {
            self.state.observation.phase = "recorded".to_owned();
            self.success("retry");
        }
    }

    fn rebase(&mut self, kind: RebaseKind) {
        if self.phase() != "moved" {
            self.error("not_moved");
            return;
        }
        match kind {
            RebaseKind::Green => {
                let obs = &mut self.state.observation;
                obs.phase = "recorded".to_owned();
                obs.tries = 0;
                obs.incoming = false;
                obs.on_base = false;
                obs.delay = 0;
                self.state.head_ok = false;
                self.success("rebase_green");
            }
            RebaseKind::Impossible => self.park(),
            RebaseKind::Conflict | RebaseKind::Red => {
                self.state.observation.phase = "repairing".to_owned();
                self.success("repairing");
            }
        }
    }

    fn repair(&mut self, kind: RepairKind) {
        if self.phase() != "repairing" {
            self.error("not_repairing");
            return;
        }
        match kind {
            RepairKind::Green => {
                let obs = &mut self.state.observation;
                obs.phase = "recorded".to_owned();
                obs.tries = 0;
                obs.repairs += 1;
                self.state.head_ok = false;
                self.success("repaired");
            }
            RepairKind::Red => {
                self.state.observation.repairs += 1;
                self.success("repairing");
            }
            RepairKind::Spent => self.park(),
        }
    }

    fn transient(&mut self) {
        let obs = &mut self.state.observation;
        if obs.tries >= 3 {
            obs.phase = "moved".to_owned();
            obs.incoming = false;
            obs.on_base = false;
            obs.delay = 0;
            obs.last = "ok".to_owned();
            obs.line = "rebase_required".to_owned();
        } else {
            obs.phase = "retrying".to_owned();
            obs.incoming = false;
            obs.tries += 1;
            obs.delay = match obs.tries {
                1 => 1_000,
                2 => 2_000,
                _ => 4_000,
            };
            obs.last = "ok".to_owned();
            obs.line = "landing_retry".to_owned();
        }
        self.state.head_ok = false;
    }

    fn stop_controller(&mut self, reason: &str) {
        let obs = &mut self.state.observation;
        obs.phase = "stopped".to_owned();
        obs.status = "stopped".to_owned();
        obs.reason = format!("controller/{reason}");
        obs.claim = false;
        obs.incoming = false;
        obs.delay = 0;
        obs.last = "ok".to_owned();
        obs.line = "stopped".to_owned();
        obs.exit = 70;
        self.state.head_ok = false;
    }

    fn park(&mut self) {
        let obs = &mut self.state.observation;
        obs.phase = "parked".to_owned();
        obs.status = "parked".to_owned();
        obs.reason = "not_landable".to_owned();
        obs.claim = false;
        obs.incoming = false;
        obs.on_base = false;
        obs.delay = 0;
        obs.last = "ok".to_owned();
        obs.line = "parked".to_owned();
        obs.exit = 1;
        self.state.head_ok = false;
    }

    fn error(&mut self, reason: &str) {
        self.state.observation.last = reason.to_owned();
        self.state.observation.line = reason.to_owned();
    }

    fn success(&mut self, line: &str) {
        self.state.observation.last = "ok".to_owned();
        self.state.observation.line = line.to_owned();
    }

    fn phase(&self) -> &str {
        &self.state.observation.phase
    }
}

impl Default for LandingModel {
    fn default() -> Self {
        Self::new()
    }
}

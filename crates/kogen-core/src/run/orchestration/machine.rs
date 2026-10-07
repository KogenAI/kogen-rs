use super::finish::FinishPolicy;
use serde::{Deserialize, Serialize};
use transitions::{
    audit, base_accept, begin, budget, fail, finish, land, pair, plan, repair, setup, stop, verify,
    witness,
};

/// Current phase of a started Build.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildPhase {
    Idle,
    B1,
    B2,
    B4,
    Setup,
    Base,
    Develop,
    Repair,
    Pair,
    B6,
    B9,
    Done,
}

impl BuildPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::B1 => "b1",
            Self::B2 => "b2",
            Self::B4 => "b4",
            Self::Setup => "setup",
            Self::Base => "base",
            Self::Develop => "dev",
            Self::Repair => "repair",
            Self::Pair => "pair",
            Self::B6 => "b6",
            Self::B9 => "b9",
            Self::Done => "done",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    Running,
    Landed,
    Failed,
    Parked,
    Stopped,
    Skipped,
}

impl BuildStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Landed => "landed",
            Self::Failed => "failed",
            Self::Parked => "parked",
            Self::Stopped => "stopped",
            Self::Skipped => "skipped",
        }
    }
}

/// State shared by the Build driver and the xspec replay adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildMachine {
    phase: BuildPhase,
    status: Option<BuildStatus>,
    reason: String,
    rung: u8,
    max_rungs: u8,
    entry_rung: u8,
    witness: bool,
    hard: bool,
    planned: bool,
    claim: bool,
    budget_left: bool,
    setup_failures: u8,
    repairs_left: u8,
    granted_repairs: u8,
    previous_red_count: Option<u64>,
    uncounted_reds: u8,
    finish_policy: FinishPolicy,
    snapshots: u32,
    landable: bool,
    parked_ref: bool,
    journal: Vec<String>,
    last: String,
    exit: i32,
}

impl Default for BuildMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl BuildMachine {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: BuildPhase::Idle,
            status: None,
            reason: String::new(),
            rung: 0,
            max_rungs: 4,
            entry_rung: 1,
            witness: false,
            hard: false,
            planned: false,
            claim: false,
            budget_left: true,
            setup_failures: 0,
            repairs_left: 0,
            granted_repairs: 0,
            previous_red_count: None,
            uncounted_reds: 0,
            finish_policy: FinishPolicy::new(),
            snapshots: 0,
            landable: false,
            parked_ref: false,
            journal: Vec::new(),
            last: String::new(),
            exit: 0,
        }
    }

    #[must_use]
    pub fn observation(&self) -> BuildObservation {
        BuildObservation {
            last: if self.last.is_empty() {
                "ok"
            } else {
                &self.last
            }
            .to_owned(),
            exit: self.exit,
            phase: self.phase.as_str().to_owned(),
            status: self
                .status
                .map_or_else(String::new, |value| value.as_str().to_owned()),
            reason: self.reason.clone(),
            rung: self.rung,
            entry: self.entry_rung,
            claim: self.claim,
            planned: self.planned,
            landable: self.landable,
            parked_ref: self.parked_ref,
            repairs: self.repairs_left,
            granted: self.granted_repairs,
            snapshots: self.snapshots,
            journal: self.journal.clone(),
        }
    }

    #[must_use]
    pub fn is_started(&self) -> bool {
        self.status == Some(BuildStatus::Running) || !self.journal.is_empty()
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.phase == BuildPhase::Done
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildObservation {
    pub last: String,
    pub exit: i32,
    pub phase: String,
    pub status: String,
    pub reason: String,
    pub rung: u8,
    pub entry: u8,
    pub claim: bool,
    pub planned: bool,
    pub landable: bool,
    #[serde(rename = "parkedRef")]
    pub parked_ref: bool,
    pub repairs: u8,
    pub granted: u8,
    pub snapshots: u32,
    pub journal: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
struct BeginInput {
    valid: bool,
    branch_ok: bool,
    claim_free: bool,
    witness: bool,
    hard: bool,
    max_rungs: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "tag", content = "value")]
pub enum BuildEvent {
    Init,
    Begin {
        valid: bool,
        branch_ok: bool,
        claim_free: bool,
        witness: bool,
        hard: bool,
        max_rungs: u8,
    },
    Probe {
        confined: bool,
    },
    Witness {
        green: bool,
    },
    Plan,
    Setup {
        ok: bool,
    },
    BaseAccept {
        runner: bool,
    },
    Verify {
        red: bool,
        landable: bool,
        counted: bool,
        count: u64,
    },
    Repair {
        lower: bool,
    },
    Finish {
        implementation_changed: bool,
    },
    Pair {
        first: String,
        second: String,
    },
    Audit {
        now_landable: bool,
    },
    Budget,
    Land {
        how: String,
    },
    Stop {
        why: String,
    },
}

/// Effects are interpreted by production around Git, models, checks, and disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    CreateRun,
    ProbeSandbox,
    RecordSandboxWarning,
    ResolveBuildBase,
    VerifyWitness,
    MakePlan,
    StartRung(u8),
    StartParallelRungs(u8, u8),
    PrepareWorkspace(u8),
    RunBaseAcceptance,
    CallBuilder(u8),
    VerifyCandidate(u8),
    RepairCandidate(u8),
    ContinueBuilder(String),
    AuditAcceptance(u8),
    SnapshotCandidate(u8),
    SelectBestCandidate,
    LandCandidate(u8),
    ParkBestCandidate,
    DestroyWorkspaces,
    ReleaseClaim,
    StopDrain(i32),
}

/// Pure policy transition. Effects contain work for the caller, never model decisions.
pub fn step(state: &BuildMachine, event: &BuildEvent) -> (BuildMachine, Vec<Effect>) {
    let mut next = state.clone();
    let mut effects = Vec::new();
    next.last.clear();
    match event {
        BuildEvent::Init => next = BuildMachine::new(),
        BuildEvent::Begin {
            valid,
            branch_ok,
            claim_free,
            witness,
            hard,
            max_rungs,
        } => {
            let input = BeginInput {
                valid: *valid,
                branch_ok: *branch_ok,
                claim_free: *claim_free,
                witness: *witness,
                hard: *hard,
                max_rungs: *max_rungs,
            };
            begin(&mut next, &input, &mut effects);
        }
        BuildEvent::Probe { confined } => {
            if next.phase != BuildPhase::B1 {
                fail(&mut next, "not_b1");
            } else {
                next.phase = BuildPhase::B2;
                effects.push(Effect::ResolveBuildBase);
                if !confined {
                    next.journal.push("sandbox_unavailable".to_owned());
                    effects.push(Effect::RecordSandboxWarning);
                }
            }
        }
        BuildEvent::Witness { green } => witness(&mut next, *green, &mut effects),
        BuildEvent::Plan => plan(&mut next, &mut effects),
        BuildEvent::Setup { ok } => setup(&mut next, *ok, &mut effects),
        BuildEvent::BaseAccept { runner } => base_accept(&mut next, *runner, &mut effects),
        BuildEvent::Verify {
            red,
            landable,
            counted,
            count,
        } => verify(&mut next, *red, *landable, *counted, *count, &mut effects),
        BuildEvent::Repair { lower: _ } => repair(&mut next, &mut effects),
        BuildEvent::Finish {
            implementation_changed,
        } => finish(&mut next, *implementation_changed, &mut effects),
        BuildEvent::Pair { first, second } => pair(&mut next, first, second, &mut effects),
        BuildEvent::Audit { now_landable } => audit(&mut next, *now_landable, &mut effects),
        BuildEvent::Budget => budget(&mut next, &mut effects),
        BuildEvent::Land { how } => land(&mut next, how, &mut effects),
        BuildEvent::Stop { why } => stop(&mut next, why, &mut effects),
    }
    (next, effects)
}

mod transitions;

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;

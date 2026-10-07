use super::super::super::finish::FinishPolicy;
use super::super::{BeginInput, BuildMachine, BuildPhase, BuildStatus, Effect};
use super::terminal::{fail, terminate, terminate_before_start};
pub(in crate::run::orchestration::machine) fn begin(
    state: &mut BuildMachine,
    input: &BeginInput,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::Idle && state.phase != BuildPhase::Done {
        fail(state, "busy");
    } else if !(1..=4).contains(&input.max_rungs) {
        fail(state, "bad_max_rungs");
    } else if !input.valid {
        *state = BuildMachine::new();
        terminate_before_start(
            state,
            BuildStatus::Stopped,
            "controller/approval_invalid",
            70,
        );
    } else if !input.branch_ok {
        *state = BuildMachine::new();
        terminate_before_start(
            state,
            BuildStatus::Skipped,
            "environment/approval_branch_mismatch",
            0,
        );
    } else if !input.claim_free {
        *state = BuildMachine::new();
        terminate_before_start(
            state,
            BuildStatus::Stopped,
            "environment/build_already_claimed",
            3,
        );
    } else {
        *state = BuildMachine::new();
        state.phase = BuildPhase::B1;
        state.status = Some(BuildStatus::Running);
        state.max_rungs = input.max_rungs;
        state.witness = input.witness;
        state.hard = input.hard;
        state.claim = true;
        state.journal.push("started".to_owned());
        effects.extend([Effect::CreateRun, Effect::ProbeSandbox]);
    }
}

pub(in crate::run::orchestration::machine) fn witness(
    state: &mut BuildMachine,
    green: bool,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::B2 || !state.witness {
        fail(state, "not_witness");
    } else {
        state.journal.push("verification".to_owned());
        if green {
            state.phase = BuildPhase::B9;
            state.landable = true;
            state.reason = "witness".to_owned();
            effects.push(Effect::LandCandidate(0));
        } else {
            state.phase = BuildPhase::B4;
            effects.push(Effect::MakePlan);
        }
    }
}

pub(in crate::run::orchestration::machine) fn plan(
    state: &mut BuildMachine,
    effects: &mut Vec<Effect>,
) {
    if state.planned {
        fail(state, "already_planned");
    } else if state.phase != BuildPhase::B4 && !(state.phase == BuildPhase::B2 && !state.witness) {
        fail(state, "not_plan");
    } else {
        state.planned = true;
        state.entry_rung = 1;
        state.rung = 1;
        state.repairs_left = 6;
        state.granted_repairs = 6;
        state.setup_failures = 0;
        state.previous_red_count = None;
        state.uncounted_reds = 0;
        state.finish_policy = FinishPolicy::new();
        if state.hard && state.max_rungs >= 2 {
            state.phase = BuildPhase::Pair;
            state
                .journal
                .extend(["plan".to_owned(), "parallel_started".to_owned()]);
            effects.push(Effect::MakePlan);
            effects.push(Effect::StartParallelRungs(1, 2));
        } else {
            state.phase = BuildPhase::Setup;
            state
                .journal
                .extend(["plan".to_owned(), "rung_started".to_owned()]);
            effects.push(Effect::MakePlan);
            effects.push(Effect::StartRung(1));
            effects.push(Effect::PrepareWorkspace(1));
        }
    }
}

pub(in crate::run::orchestration::machine) fn setup(
    state: &mut BuildMachine,
    ok: bool,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::Setup {
        fail(state, "not_setup");
    } else if ok {
        state.setup_failures = 0;
        state.phase = if state.rung == state.entry_rung {
            BuildPhase::Base
        } else {
            BuildPhase::Develop
        };
        if state.phase == BuildPhase::Base {
            effects.push(Effect::RunBaseAcceptance);
        } else {
            effects.push(Effect::CallBuilder(state.rung));
        }
    } else {
        state.setup_failures = state.setup_failures.saturating_add(1);
        if state.setup_failures >= 2 {
            terminate(
                state,
                BuildStatus::Stopped,
                "environment/setup_failed",
                3,
                effects,
            );
        }
    }
}

pub(in crate::run::orchestration::machine) fn base_accept(
    state: &mut BuildMachine,
    runner: bool,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::Base {
        fail(state, "not_base");
    } else if !runner {
        terminate(
            state,
            BuildStatus::Stopped,
            "environment/tool_missing",
            3,
            effects,
        );
    } else {
        state.phase = BuildPhase::Develop;
        state.journal.push("base_acceptance".to_owned());
        effects.push(Effect::CallBuilder(state.rung));
    }
}

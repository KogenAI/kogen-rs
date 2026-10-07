use super::super::super::finish::{FinishAction, FinishPolicy};
use super::super::{BuildMachine, BuildPhase, BuildStatus, Effect};
use super::terminal::{fail, terminal_effects};
pub(in crate::run::orchestration::machine) fn verify(
    state: &mut BuildMachine,
    red: bool,
    landable: bool,
    counted: bool,
    count: u64,
    effects: &mut Vec<Effect>,
) {
    let budget_verification = !state.budget_left
        && matches!(
            state.phase,
            BuildPhase::Setup | BuildPhase::Base | BuildPhase::Develop | BuildPhase::Repair
        );
    if state.phase != BuildPhase::Develop && !budget_verification {
        fail(state, "not_dev");
    } else if !state.budget_left {
        finish_rung(state, landable, "budget", effects);
    } else if !red {
        finish_rung(
            state,
            landable,
            if landable { "green" } else { "unverified" },
            effects,
        );
    } else if (!counted && state.uncounted_reds >= 1)
        || (counted
            && state
                .previous_red_count
                .is_some_and(|previous| count >= previous))
    {
        finish_rung(state, false, "no_progress", effects);
    } else if state.repairs_left == 0 {
        finish_rung(state, false, "repair_cap", effects);
    } else {
        state.phase = BuildPhase::Repair;
        if counted {
            state.previous_red_count = Some(count);
        } else {
            state.uncounted_reds = state.uncounted_reds.saturating_add(1);
        }
        state.journal.push("verification".to_owned());
        effects.push(Effect::RepairCandidate(state.rung));
    }
}

fn finish_rung(state: &mut BuildMachine, landable: bool, reason: &str, effects: &mut Vec<Effect>) {
    state.landable = landable;
    state.reason = reason.to_owned();
    state.snapshots = state.snapshots.saturating_add(1);
    state
        .journal
        .extend(["verification".to_owned(), "rung_finished".to_owned()]);
    effects.push(Effect::SnapshotCandidate(state.rung));
    if landable {
        state.phase = BuildPhase::B9;
        effects.push(Effect::LandCandidate(state.rung));
    } else if state.budget_left {
        state.phase = BuildPhase::B6;
        effects.push(Effect::AuditAcceptance(state.rung));
    } else {
        choose_best(state, effects);
    }
}

pub(in crate::run::orchestration::machine) fn repair(
    state: &mut BuildMachine,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::Repair {
        fail(state, "not_repair");
    } else {
        state.phase = BuildPhase::Develop;
        state.repairs_left = state.repairs_left.saturating_sub(1);
        state.journal.push("repair".to_owned());
        effects.push(Effect::CallBuilder(state.rung));
    }
}

pub(in crate::run::orchestration::machine) fn finish(
    state: &mut BuildMachine,
    implementation_changed: bool,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::Develop {
        fail(state, "not_dev");
        return;
    }
    if !state.budget_left {
        effects.push(Effect::VerifyCandidate(state.rung));
        return;
    }
    match state.finish_policy.finish(implementation_changed) {
        FinishAction::Continue(feedback) => effects.push(Effect::ContinueBuilder(feedback)),
        FinishAction::Verify => effects.push(Effect::VerifyCandidate(state.rung)),
    }
}

pub(in crate::run::orchestration::machine) fn pair(
    state: &mut BuildMachine,
    first: &str,
    second: &str,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::Pair {
        fail(state, "not_pair");
    } else if !matches!(first, "green" | "red") || !matches!(second, "green" | "red") {
        fail(state, "bad_pair");
    } else {
        state.snapshots = state.snapshots.saturating_add(2);
        if first == "green" || second == "green" {
            state.journal.push("rung_finished".to_owned());
            state.rung = if first == "green" { 1 } else { 2 };
            state.phase = BuildPhase::B9;
            state.landable = true;
            state.reason = "green".to_owned();
            effects.push(Effect::SnapshotCandidate(state.rung));
            effects.push(Effect::LandCandidate(state.rung));
        } else {
            state.landable = false;
            effects.extend([Effect::SnapshotCandidate(1), Effect::SnapshotCandidate(2)]);
            if state.budget_left && state.max_rungs >= 3 {
                start_rung(state, 3, effects);
            } else {
                choose_best(state, effects);
            }
        }
    }
}

pub(in crate::run::orchestration::machine) fn audit(
    state: &mut BuildMachine,
    now_landable: bool,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::B6 {
        fail(state, "not_audit");
    } else {
        state.journal.push("audit".to_owned());
        state.landable = now_landable;
        if now_landable {
            state.phase = BuildPhase::B9;
            effects.push(Effect::LandCandidate(state.rung));
        } else {
            advance(state, effects);
        }
    }
}

pub(in crate::run::orchestration::machine) fn budget(
    state: &mut BuildMachine,
    effects: &mut Vec<Effect>,
) {
    if !matches!(
        state.phase,
        BuildPhase::Setup
            | BuildPhase::Base
            | BuildPhase::Develop
            | BuildPhase::Repair
            | BuildPhase::B6
            | BuildPhase::B4
            | BuildPhase::B2
            | BuildPhase::Pair
    ) {
        fail(state, "not_budget");
        return;
    }
    state.budget_left = false;
    match state.phase {
        BuildPhase::Setup | BuildPhase::Base | BuildPhase::Develop | BuildPhase::Repair => {
            effects.push(Effect::VerifyCandidate(state.rung));
        }
        BuildPhase::Pair => {
            effects.extend([Effect::VerifyCandidate(1), Effect::VerifyCandidate(2)])
        }
        _ => advance(state, effects),
    }
}

fn advance(state: &mut BuildMachine, effects: &mut Vec<Effect>) {
    let next_rung = state.rung.saturating_add(1);
    if state.budget_left && next_rung <= state.max_rungs {
        start_rung(state, next_rung, effects);
    } else {
        choose_best(state, effects);
    }
}

fn start_rung(state: &mut BuildMachine, rung: u8, effects: &mut Vec<Effect>) {
    state.rung = rung;
    state.phase = BuildPhase::Setup;
    state.repairs_left = 6;
    state.granted_repairs = 6;
    state.setup_failures = 0;
    state.previous_red_count = None;
    state.uncounted_reds = 0;
    state.finish_policy = FinishPolicy::new();
    state.landable = false;
    state.journal.push("rung_started".to_owned());
    effects.push(Effect::StartRung(rung));
    effects.push(Effect::PrepareWorkspace(rung));
}

fn choose_best(state: &mut BuildMachine, effects: &mut Vec<Effect>) {
    state.phase = BuildPhase::Done;
    state.status = Some(BuildStatus::Failed);
    state.reason = "best_candidate".to_owned();
    state.claim = false;
    state.parked_ref = state.snapshots > 0;
    state.landable = false;
    state.exit = 1;
    state
        .journal
        .extend(["selection".to_owned(), "finished".to_owned()]);
    effects.push(Effect::SelectBestCandidate);
    if state.parked_ref {
        effects.push(Effect::ParkBestCandidate);
    }
    terminal_effects(effects, 1);
}

use super::super::{BuildMachine, BuildPhase, BuildStatus, Effect};
pub(in crate::run::orchestration::machine) fn land(
    state: &mut BuildMachine,
    how: &str,
    effects: &mut Vec<Effect>,
) {
    if state.phase != BuildPhase::B9 {
        fail(state, "not_landing");
    } else if how == "landed" {
        state.phase = BuildPhase::Done;
        state.status = Some(BuildStatus::Landed);
        state.reason.clear();
        state.claim = false;
        state.parked_ref = false;
        state.exit = 0;
        state.journal.extend([
            "commit_result".to_owned(),
            "landing_prepared".to_owned(),
            "finished".to_owned(),
        ]);
        effects.extend([Effect::DestroyWorkspaces, Effect::ReleaseClaim]);
    } else if how == "parked" {
        state.phase = BuildPhase::Done;
        state.status = Some(BuildStatus::Parked);
        state.reason = "base_moved".to_owned();
        state.claim = false;
        state.parked_ref = true;
        state.exit = 1;
        state.journal.extend([
            "commit_result".to_owned(),
            "landing_prepared".to_owned(),
            "finished".to_owned(),
        ]);
        effects.extend([
            Effect::ParkBestCandidate,
            Effect::DestroyWorkspaces,
            Effect::ReleaseClaim,
        ]);
    } else if how == "controller" {
        terminate(
            state,
            BuildStatus::Stopped,
            "controller/not_fast_forward",
            70,
            effects,
        );
    } else {
        fail(state, "bad_land");
    }
}

pub(in crate::run::orchestration::machine) fn stop(
    state: &mut BuildMachine,
    why: &str,
    effects: &mut Vec<Effect>,
) {
    if state.phase == BuildPhase::Idle || state.phase == BuildPhase::Done {
        fail(state, "not_running");
        return;
    }
    let (reason, exit) = match why {
        "provider" => ("provider/overload", 4),
        "environment" => ("environment/setup_failed", 3),
        "controller" => ("controller/internal_error", 70),
        _ => {
            fail(state, "bad_stop");
            return;
        }
    };
    terminate(state, BuildStatus::Stopped, reason, exit, effects);
}

pub(in crate::run::orchestration::machine) fn terminate(
    state: &mut BuildMachine,
    status: BuildStatus,
    reason: &str,
    exit: i32,
    effects: &mut Vec<Effect>,
) {
    state.phase = BuildPhase::Done;
    state.status = Some(status);
    state.reason = reason.to_owned();
    state.claim = false;
    state.landable = false;
    state.parked_ref = false;
    state.exit = exit;
    state.journal.push("finished".to_owned());
    terminal_effects(effects, exit);
}

pub(in crate::run::orchestration::machine) fn terminate_before_start(
    state: &mut BuildMachine,
    status: BuildStatus,
    reason: &str,
    exit: i32,
) {
    state.phase = BuildPhase::Done;
    state.status = Some(status);
    state.reason = reason.to_owned();
    state.claim = false;
    state.landable = false;
    state.parked_ref = false;
    state.exit = exit;
}

pub(in crate::run::orchestration::machine) fn terminal_effects(
    effects: &mut Vec<Effect>,
    exit: i32,
) {
    effects.extend([Effect::DestroyWorkspaces, Effect::ReleaseClaim]);
    if exit == 3 || exit == 4 || exit == 70 {
        effects.push(Effect::StopDrain(exit));
    }
}

pub(in crate::run::orchestration::machine) fn fail(state: &mut BuildMachine, code: &str) {
    state.last = code.to_owned();
}

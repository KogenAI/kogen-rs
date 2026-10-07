use super::{BuildEvent as E, BuildMachine, BuildPhase, BuildStatus, Effect, step};

fn apply(state: BuildMachine, event: E) -> (BuildMachine, Vec<Effect>) {
    step(&state, &event)
}

fn started(max_rungs: u8, hard: bool) -> BuildMachine {
    let state = BuildMachine::new();
    let (state, _) = apply(
        state,
        E::Begin {
            valid: true,
            branch_ok: true,
            claim_free: true,
            witness: false,
            hard,
            max_rungs,
        },
    );
    let (state, _) = apply(state, E::Probe { confined: true });
    state
}

#[test]
fn prestart_refusals_and_started_stops_have_distinct_terminal_journals() {
    let (refused, effects) = apply(
        BuildMachine::new(),
        E::Begin {
            valid: false,
            branch_ok: true,
            claim_free: true,
            witness: false,
            hard: false,
            max_rungs: 4,
        },
    );
    assert_eq!(refused.phase, BuildPhase::Done);
    assert_eq!(refused.status, Some(BuildStatus::Stopped));
    assert!(refused.journal.is_empty());
    assert!(effects.is_empty());

    let (running, _) = apply(
        BuildMachine::new(),
        E::Begin {
            valid: true,
            branch_ok: true,
            claim_free: true,
            witness: false,
            hard: false,
            max_rungs: 4,
        },
    );
    let (stopped, effects) = apply(
        running,
        E::Stop {
            why: "environment".to_owned(),
        },
    );
    assert_eq!(stopped.status, Some(BuildStatus::Stopped));
    assert_eq!(stopped.journal, ["started", "finished"]);
    assert!(!stopped.claim);
    assert!(effects.contains(&Effect::ReleaseClaim));
    assert!(effects.contains(&Effect::StopDrain(3)));
}

#[test]
fn hard_plan_starts_two_rungs_and_red_pair_advances_to_three() {
    let (planned, effects) = apply(started(4, true), E::Plan);
    assert_eq!(planned.phase, BuildPhase::Pair);
    assert!(effects.contains(&Effect::StartParallelRungs(1, 2)));
    let (next, effects) = apply(
        planned,
        E::Pair {
            first: "red".to_owned(),
            second: "red".to_owned(),
        },
    );
    assert_eq!(next.phase, BuildPhase::Setup);
    assert_eq!(next.rung, 3);
    assert_eq!(next.snapshots, 2);
    assert!(effects.contains(&Effect::PrepareWorkspace(3)));
}

#[test]
fn repair_progress_and_per_rung_caps_are_tracked() {
    let (state, _) = apply(started(2, false), E::Plan);
    let (state, _) = apply(state, E::Setup { ok: true });
    let (state, _) = apply(state, E::BaseAccept { runner: true });
    let (state, effects) = apply(
        state,
        E::Verify {
            red: true,
            landable: false,
            counted: true,
            count: 4,
        },
    );
    assert_eq!(state.phase, BuildPhase::Repair);
    assert!(effects.contains(&Effect::RepairCandidate(1)));
    let (state, _) = apply(state, E::Repair { lower: true });
    assert_eq!(state.repairs_left, 5);
    let (state, _) = apply(
        state,
        E::Verify {
            red: true,
            landable: false,
            counted: true,
            count: 4,
        },
    );
    assert_eq!(state.phase, BuildPhase::B6);
    assert_eq!(state.reason, "no_progress");
    let (state, _) = apply(
        state,
        E::Audit {
            now_landable: false,
        },
    );
    assert_eq!(state.rung, 2);
    assert_eq!(state.repairs_left, 6);
    assert_eq!(state.granted_repairs, 6);
}

#[test]
fn setup_and_base_runner_failures_stop_without_judging_the_intent() {
    let (state, _) = apply(started(1, false), E::Plan);
    let (state, _) = apply(state, E::Setup { ok: false });
    let (state, effects) = apply(state, E::Setup { ok: false });
    assert_eq!(state.status, Some(BuildStatus::Stopped));
    assert_eq!(state.reason, "environment/setup_failed");
    assert!(effects.contains(&Effect::StopDrain(3)));

    let (state, _) = apply(started(1, false), E::Plan);
    let (state, _) = apply(state, E::Setup { ok: true });
    let (state, _) = apply(state, E::BaseAccept { runner: false });
    assert_eq!(state.reason, "environment/tool_missing");
    assert_eq!(state.phase, BuildPhase::Done);
}

#[test]
fn witness_green_bypasses_model_stages_and_goes_to_landing() {
    let (state, _) = apply(
        BuildMachine::new(),
        E::Begin {
            valid: true,
            branch_ok: true,
            claim_free: true,
            witness: true,
            hard: false,
            max_rungs: 4,
        },
    );
    let (state, _) = apply(state, E::Probe { confined: true });
    let (state, effects) = apply(state, E::Witness { green: true });
    assert_eq!(state.phase, BuildPhase::B9);
    assert!(!state.planned);
    assert!(effects.contains(&Effect::LandCandidate(0)));
}

#[test]
fn only_a_valid_finish_requests_verification_and_empty_finish_is_refused_once() {
    let (state, _) = apply(started(1, false), E::Plan);
    let (state, _) = apply(state, E::Setup { ok: true });
    let (state, _) = apply(state, E::BaseAccept { runner: true });
    let (state, effects) = apply(
        state,
        E::Finish {
            implementation_changed: false,
        },
    );
    assert!(effects.contains(&Effect::ContinueBuilder(
        "Kogen found no changed files. Make the requested change before claiming done.".to_owned()
    )));
    let (_, effects) = apply(
        state,
        E::Finish {
            implementation_changed: false,
        },
    );
    assert!(effects.contains(&Effect::VerifyCandidate(1)));
}

#[test]
fn budget_cancels_builder_work_verifies_the_tree_and_preserves_the_candidate() {
    let (state, _) = apply(started(4, false), E::Plan);
    let (state, _) = apply(state, E::Setup { ok: true });
    let (state, _) = apply(state, E::BaseAccept { runner: true });
    let (state, effects) = apply(state, E::Budget);
    assert_eq!(state.phase, BuildPhase::Develop);
    assert!(effects.contains(&Effect::VerifyCandidate(1)));
    assert!(!effects.contains(&Effect::RepairCandidate(1)));

    let (state, effects) = apply(
        state,
        E::Verify {
            red: true,
            landable: false,
            counted: true,
            count: 3,
        },
    );
    assert_eq!(state.phase, BuildPhase::Done);
    assert_eq!(state.status, Some(BuildStatus::Failed));
    assert_eq!(state.reason, "best_candidate");
    assert!(state.parked_ref);
    assert_eq!(state.snapshots, 1);
    assert!(effects.contains(&Effect::SnapshotCandidate(1)));
    assert!(effects.contains(&Effect::SelectBestCandidate));
    assert!(!effects.contains(&Effect::RepairCandidate(1)));
}

#[test]
fn budget_in_a_parallel_entry_verifies_both_rungs_before_selection() {
    let (state, _) = apply(started(4, true), E::Plan);
    let (state, effects) = apply(state, E::Budget);
    assert_eq!(state.phase, BuildPhase::Pair);
    assert!(effects.contains(&Effect::VerifyCandidate(1)));
    assert!(effects.contains(&Effect::VerifyCandidate(2)));

    let (state, effects) = apply(
        state,
        E::Pair {
            first: "red".to_owned(),
            second: "red".to_owned(),
        },
    );
    assert_eq!(state.phase, BuildPhase::Done);
    assert_eq!(state.snapshots, 2);
    assert!(effects.contains(&Effect::SnapshotCandidate(1)));
    assert!(effects.contains(&Effect::SnapshotCandidate(2)));
    assert!(effects.contains(&Effect::SelectBestCandidate));
}

#[test]
fn budget_before_the_first_snapshot_does_not_park_a_nonexistent_candidate() {
    let (state, _) = apply(
        BuildMachine::new(),
        E::Begin {
            valid: true,
            branch_ok: true,
            claim_free: true,
            witness: true,
            hard: false,
            max_rungs: 4,
        },
    );
    let (state, _) = apply(state, E::Probe { confined: true });
    let (state, _) = apply(state, E::Witness { green: false });
    let (state, effects) = apply(state, E::Budget);
    assert_eq!(state.status, Some(BuildStatus::Failed));
    assert!(!state.parked_ref);
    assert!(effects.contains(&Effect::SelectBestCandidate));
    assert!(!effects.contains(&Effect::ParkBestCandidate));
}

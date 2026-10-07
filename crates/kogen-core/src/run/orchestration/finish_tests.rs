use super::{FinishAction, FinishPolicy};

#[test]
fn first_empty_finish_prompts_and_second_empty_finish_runs_gate() {
    let mut policy = FinishPolicy::new();
    assert_eq!(
        policy.finish(false),
        FinishAction::Continue(FinishPolicy::EMPTY_FINISH_FEEDBACK.to_owned())
    );
    assert_eq!(policy.empty_finishes(), 1);
    assert_eq!(policy.finish(false), FinishAction::Verify);
}

#[test]
fn changed_implementation_finishes_into_gate_immediately() {
    let mut policy = FinishPolicy::new();
    assert_eq!(policy.finish(true), FinishAction::Verify);
    assert_eq!(policy.empty_finishes(), 0);
}

use super::super::QueueScheduler;
use super::super::{DrainOutcome as Outcome, QueueApproval as Approval, QueueEvent as Event};

fn enqueue(queue: &mut QueueScheduler, slug: &str, time: i64, priority: i64, hash: &str) {
    queue.apply(Event::Enqueue(Approval::new(slug, time, priority, hash)));
}

fn enqueue_commit(
    queue: &mut QueueScheduler,
    slug: &str,
    time: i64,
    priority: i64,
    hash: &str,
    commit: &str,
) {
    queue.apply(Event::Enqueue(
        Approval::new(slug, time, priority, hash).with_approval_commit(commit),
    ));
}

#[test]
fn orders_priority_then_approval_time_then_slug() {
    let mut queue = QueueScheduler::new();
    enqueue(&mut queue, "charlie", 2, 0, "c");
    enqueue(&mut queue, "bravo", 1, 0, "b");
    enqueue(&mut queue, "alpha", 1, 0, "a");
    enqueue(&mut queue, "high", 9, 2, "h");

    assert_eq!(queue.observe().queue, ["high", "alpha", "bravo", "charlie"]);
}

#[test]
fn a_live_second_start_is_a_noop_for_the_inflight_build() {
    let mut queue = QueueScheduler::new();
    enqueue(&mut queue, "alpha", 1, 0, "a");
    queue.apply(Event::Start);

    let observation = queue.apply(Event::Start);

    assert_eq!(observation.line, "already_running");
    assert_eq!(observation.exit, 0);
    assert_eq!(observation.current, "alpha");
    assert_eq!(observation.phase, "building");
}

#[test]
fn same_hash_reapproval_is_one_attempt_in_a_drain() {
    let mut queue = QueueScheduler::new();
    enqueue_commit(&mut queue, "alpha", 1, 0, "same-hash", "commit-one");
    enqueue(&mut queue, "bravo", 2, 0, "bravo-hash");
    queue.apply(Event::Start);
    enqueue_commit(&mut queue, "alpha", 3, 8, "same-hash", "commit-two");

    let observation = queue.apply(Event::Outcome(Outcome::Failed));

    assert_eq!(observation.current, "bravo");
    assert!(observation.queue.is_empty());
    assert_eq!(observation.built, 1);

    let finished = queue.apply(Event::Outcome(Outcome::Landed));
    assert_eq!(finished.line, "done");
    assert_eq!(finished.queue, ["alpha"]);
}

#[test]
fn provider_exhaustion_stops_with_current_and_next_approval_queued() {
    let mut queue = QueueScheduler::new();
    enqueue(&mut queue, "alpha", 1, 0, "a");
    enqueue(&mut queue, "bravo", 2, 0, "b");
    queue.apply(Event::Start);
    let stopped = queue.apply(Event::Outcome(Outcome::StoppedProvider));
    assert_eq!(stopped.line, "stopped_because");
    assert_eq!(stopped.exit, 4);
    assert_eq!(stopped.current, "");
    assert_eq!(stopped.queue, ["alpha", "bravo"]);
    assert_eq!(stopped.built, 1);
    let resumed = queue.apply(Event::Start);
    assert_eq!(resumed.current, "alpha");
    assert_eq!(resumed.queue, ["bravo"]);
}

#[test]
fn a_dead_owner_is_taken_over_and_retries_the_inflight_approval() {
    let mut queue = QueueScheduler::new();
    enqueue(&mut queue, "alpha", 1, 0, "a");
    queue.apply(Event::Start);
    queue.apply(Event::Die);

    let observation = queue.apply(Event::Start);

    assert_eq!(observation.line, "building");
    assert_eq!(observation.current, "alpha");
    assert!(observation.alive && observation.held);
}

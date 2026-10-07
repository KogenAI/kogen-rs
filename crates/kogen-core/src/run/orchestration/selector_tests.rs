use super::{
    Candidate, CandidateMetadata, CandidateSelector, CheckScoreInput, GatePolicy, GateReplay,
    GateScore, ItemKind, ItemResult, ItemVerdict, VerificationVerdict, blocking_count,
    score_verification,
};
use serde_json::json;

fn candidate(rung: &str, order: usize, passed: usize, blocking: usize, diff: usize) -> Candidate {
    Candidate {
        rung: rung.to_owned(),
        rung_index: rung.parse().unwrap_or(9),
        order,
        model: "model".to_owned(),
        effort: "medium".to_owned(),
        reason: "repair_cap".to_owned(),
        verdict: VerificationVerdict::Unverified,
        passing_undemoted_items: passed,
        blocking_findings: blocking,
        diff_lines: diff,
        candidate_ref: format!("refs/kogen/candidate/run/{rung}"),
        diff_path: format!("diff-{rung}.patch"),
        wall_ms: 10,
        tokens: None,
    }
}

#[test]
fn selector_ranks_passes_then_blockers_then_diff_then_earliest_rung() {
    let mut selector = CandidateSelector::default();
    selector.offer(candidate("3", 3, 1, 0, 11));
    selector.offer(candidate("2", 2, 1, 0, 10));
    selector.offer(candidate("1", 1, 1, 8, 90));
    assert_eq!(selector.select().unwrap().rung, "2");

    let mut pass_priority = CandidateSelector::default();
    pass_priority.offer(candidate("2", 2, 2, 9, 100));
    pass_priority.offer(candidate("3", 3, 1, 0, 1));
    assert_eq!(pass_priority.select().unwrap().rung, "2");

    let mut tie = CandidateSelector::default();
    tie.offer(candidate("later", 2, 3, 1, 4));
    tie.offer(candidate("earlier", 1, 3, 1, 4));
    assert_eq!(tie.select().unwrap().rung, "earlier");

    let mut parallel = CandidateSelector::default();
    parallel.offer(candidate("3", 1, 2, 0, 5));
    parallel.offer(candidate("2", 2, 2, 0, 5));
    assert_eq!(parallel.select().unwrap().rung, "2");
}

#[test]
fn blocking_count_uses_distinct_check_identities_and_item_failures() {
    assert_eq!(
        blocking_count(
            [
                "lint/a".to_owned(),
                "lint/a".to_owned(),
                "tests/A1".to_owned()
            ],
            1,
            2
        ),
        5,
    );
}

#[test]
fn pure_verdict_requires_a_passing_undemoted_change_item() {
    let checks = [CheckScoreInput {
        name: "lint".to_owned(),
        green: true,
        excused: false,
        finding_identities: Vec::new(),
        red_without_identity: false,
    }];
    let only_demoted_change = [ItemResult {
        id: "A1".to_owned(),
        kind: ItemKind::Change,
        verdict: ItemVerdict::Fail,
        demoted: true,
    }];
    let score = score_verification(GatePolicy::GreenOrAdvisory, &checks, &only_demoted_change);
    assert_eq!(score.verdict, VerificationVerdict::GreenWithAdvisoryTests);
    assert!(!score.landable);

    let mut items = only_demoted_change.to_vec();
    items.push(ItemResult {
        id: "A2".to_owned(),
        kind: ItemKind::Change,
        verdict: ItemVerdict::Pass,
        demoted: false,
    });
    let score = score_verification(GatePolicy::GreenOrAdvisory, &checks, &items);
    assert!(score.landable);
    assert_eq!(score.passing_undemoted_items, 1);
}

#[test]
fn advisory_demotions_land_only_under_the_default_policy() {
    let green = GateScore {
        verdict: VerificationVerdict::GreenWithAdvisoryTests,
        landable: true,
        passing_undemoted_items: 1,
        blocking_findings: 0,
    };
    let candidate = Candidate::score(
        CandidateMetadata {
            rung: "2".to_owned(),
            rung_index: 2,
            order: 2,
            model: "m".to_owned(),
            effort: "high".to_owned(),
            reason: "green".to_owned(),
            diff_lines: 1,
            candidate_ref: "ref".to_owned(),
            diff_path: "diff".to_owned(),
            wall_ms: 4,
            tokens: None,
        },
        &green,
    );
    assert!(candidate.landable(GatePolicy::GreenOrAdvisory, true));
    assert!(!candidate.landable(GatePolicy::Green, true));
    assert!(!candidate.landable(GatePolicy::GreenOrAdvisory, false));
}

#[test]
fn gate_replay_scores_excuses_and_change_items_through_core_policy() {
    let mut replay = GateReplay::new();
    replay
        .apply(&json!({"tag":"Baseline","value":{"name":"c1","status":"red"}}))
        .unwrap();
    replay.apply(&json!({"tag":"Now","value":{"name":"c1","status":"red","hasIds":true,"subset":true,"sameExit":true}})).unwrap();
    replay.apply(&json!({"tag":"Rows","value":{"id":"A1","kind":"change","report":"ok","runnerDown":false,"exit0":true,"mutated":false,"failed":0,"rows":1}})).unwrap();
    replay
        .apply(&json!({"tag":"Score","value":{"policy":"green-or-advisory"}}))
        .unwrap();
    let observation = replay.observe();
    assert_eq!(observation["verdict"], "green");
    assert_eq!(observation["landable"], true);
    assert_eq!(observation["excused"]["c1"], true);
}

#[test]
fn advisory_is_not_a_valid_change_item_and_selection_never_uses_demoted_passes() {
    let items = [
        ItemResult {
            id: "A1".to_owned(),
            kind: ItemKind::Change,
            verdict: ItemVerdict::Pass,
            demoted: true,
        },
        ItemResult {
            id: "A2".to_owned(),
            kind: ItemKind::Keep,
            verdict: ItemVerdict::Pass,
            demoted: false,
        },
    ];
    assert_eq!(
        items
            .iter()
            .filter(|item| item.kind == ItemKind::Change
                && !item.demoted
                && item.verdict == ItemVerdict::Pass)
            .count(),
        0
    );
}

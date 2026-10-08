use super::{BuildAuditRequest, BuildAuditVerdict, BuildAuditor, decode_build_audit};

#[test]
fn only_valid_demotions_for_the_rung_failures_are_accepted() {
    let ids = ["A1".to_owned(), "A2".to_owned()];
    let dispositions = decode_build_audit(
        r#"{"items":[{"id":"A1","verdict":"over_strict","reason":"extra requirement"},{"id":"A2","verdict":"valid","reason":"matches request"},{"id":"A3","verdict":"contradicts","reason":"unknown"}]}"#,
        &ids,
    );
    assert_eq!(dispositions.len(), 2);
    assert_eq!(dispositions[0].verdict, BuildAuditVerdict::OverStrict);
    assert!(!dispositions[0].demote);
    assert_eq!(dispositions[1].verdict, BuildAuditVerdict::Valid);
    assert!(!dispositions[1].demote);
}

#[test]
fn malformed_or_unknown_auditor_output_keeps_items_upheld() {
    let ids = ["A4".to_owned()];
    let parsed = decode_build_audit("not json", &ids);
    assert_eq!(parsed[0].verdict, BuildAuditVerdict::Valid);
    assert!(!parsed[0].demote);
    let unknown = decode_build_audit(r#"{"items":[{"id":"A4","verdict":"maybe"}]}"#, &ids);
    assert_eq!(unknown[0].verdict, BuildAuditVerdict::Valid);
}

#[test]
fn audit_prompt_clips_candidate_diff_by_characters() {
    let request = BuildAuditRequest {
        ids: vec!["A1".to_owned()],
        request: "verbatim task".to_owned(),
        test_source: "acceptance source".to_owned(),
        failure_output: "assertion failed".to_owned(),
        candidate_diff: "x".repeat(60_100),
    };
    assert_eq!(request.clipped_diff().chars().count(), 60_000);
    assert!(
        request
            .user_message()
            .contains("Failed acceptance items: A1")
    );
    assert!(request.user_message().contains("verbatim task"));
}

#[test]
fn build_audit_runs_only_for_acceptance_only_red_and_at_most_once_per_item() {
    let mut auditor = BuildAuditor::default();
    let ids = ["A1".to_owned(), "A2".to_owned()];
    assert!(auditor.begin_rung_audit(&ids, false, false).is_empty());
    assert!(auditor.begin_rung_audit(&ids, true, true).is_empty());
    let pending = auditor.begin_rung_audit(&ids, true, false);
    assert_eq!(pending, ids);
    let disposition = auditor.complete_rung_audit(&pending, "not json");
    assert!(disposition.iter().all(|item| !item.demote));
    assert!(auditor.begin_rung_audit(&ids, true, false).is_empty());
    assert_eq!(auditor.records().len(), 2);
}

#[test]
fn malformed_or_ambiguous_rows_cannot_demote_an_item() {
    let ids = ["A1".to_owned()];
    let missing_reason =
        decode_build_audit(r#"{"items":[{"id":"A1","verdict":"over_strict"}]}"#, &ids);
    assert_eq!(missing_reason[0].verdict, BuildAuditVerdict::Valid);
    assert!(!missing_reason[0].demote);

    let duplicate = decode_build_audit(
        r#"{"items":[{"id":"A1","verdict":"over_strict","reason":"one"},{"id":"A1","verdict":"contradicts","reason":"two"}]}"#,
        &ids,
    );
    assert_eq!(duplicate[0].verdict, BuildAuditVerdict::Valid);
    assert!(!duplicate[0].demote);
}

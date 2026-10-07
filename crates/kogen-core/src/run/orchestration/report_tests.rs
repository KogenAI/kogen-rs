use super::{BestCandidateReport, BuildReport};
use serde_json::Value;

#[test]
fn report_contains_v12_fields_and_emits_agents_only_when_present() {
    let mut report = BuildReport::new("greet", "run-1");
    report.status = "failed".to_owned();
    report.best_candidate = Some(BestCandidateReport {
        rung: "R3".to_owned(),
        ref_name: "refs/kogen/candidate/run-1/R3".to_owned(),
        diff_path: "candidate.diff".to_owned(),
        verdict: "unverified".to_owned(),
    });
    report.set_agents(Vec::new());
    let empty: Value = serde_json::from_slice(&report.to_json().unwrap()).unwrap();
    assert!(empty.get("agents").is_none());
    assert_eq!(
        empty["best_candidate"]["ref"],
        "refs/kogen/candidate/run-1/R3"
    );
    assert!(empty.get("rungs").is_some());
    assert!(empty.get("audit").is_some());
    assert!(empty.get("model_stages").is_some());
    assert!(empty.get("budget").is_some());

    report.set_agents(vec![serde_json::json!({"role":"context"})]);
    let with_agents: Value = serde_json::from_slice(&report.to_json().unwrap()).unwrap();
    assert_eq!(with_agents["agents"].as_array().unwrap().len(), 1);
}

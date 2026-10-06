use super::*;
use crate::gate::{ABSENT_SHA256, CheckStatus, LedgerRow, LedgerStatus, ProtectedEntry};
use crate::intent::intent_sha256;
use crate::run::{ProcessError, ProcessRequest, ProcessResult};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

#[test]
fn green_verification_receipt_binds_the_candidate_tree_and_change_items_land() {
    let fixture = Fixture::new("green");
    let runner = Runner::new(Some(0), LedgerStatus::Passed);
    let report = run_gate(&runner, &fixture.request(BTreeSet::from(["A1".to_owned()]))).unwrap();
    assert_eq!(report.verdict, GateVerdict::Green);
    assert!(report.is_verified());
    assert!(report.is_landable());
    assert_eq!(
        report.receipt().unwrap().tree_id(),
        report.verified_tree.as_deref().unwrap()
    );
    assert_eq!(
        runner.calls.lock().unwrap().as_slice(),
        ["lint", "lint", "acceptance"]
    );
    assert!(
        !fixture
            .candidate
            .join(".kogen/acceptance/greet.t.sh")
            .exists()
    );
    assert_eq!(
        fs::read(fixture.candidate.join("test/acceptance/greet.t.sh")).unwrap(),
        fixture.acceptance
    );
    fixture.remove();
}

#[test]
fn a_green_no_change_intent_is_verified_but_not_landable() {
    let fixture = Fixture::new("no-change");
    let runner = Runner::new(Some(0), LedgerStatus::Passed);
    let report = run_gate(&runner, &fixture.request(BTreeSet::new())).unwrap();
    assert_eq!(report.verdict, GateVerdict::Green);
    assert!(report.is_verified());
    assert!(!report.is_landable());
    fixture.remove();
}

#[test]
fn a_finish_response_does_not_create_a_receipt_when_a_check_is_red() {
    let fixture = Fixture::new("red-check");
    let runner = Runner::new(Some(1), LedgerStatus::Passed);
    let report = run_gate(&runner, &fixture.request(BTreeSet::from(["A1".to_owned()]))).unwrap();
    assert_eq!(report.verdict, GateVerdict::Unverified);
    assert!(!report.is_verified());
    assert!(!report.is_landable());
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.status == CheckStatus::Red)
    );
    fixture.remove();
}

#[test]
fn failed_tagged_acceptance_item_refuses_a_green_check() {
    let fixture = Fixture::new("red-acceptance");
    let runner = Runner::new(Some(0), LedgerStatus::Failed);
    let report = run_gate(&runner, &fixture.request(BTreeSet::from(["A1".to_owned()]))).unwrap();
    assert_eq!(report.verdict, GateVerdict::Unverified);
    assert!(!report.is_verified());
    assert!(!report.acceptance.item_pass["A1"]);
    fixture.remove();
}

struct Fixture {
    root: PathBuf,
    base: PathBuf,
    candidate: PathBuf,
    run: PathBuf,
    acceptance: Vec<u8>,
    intent: Vec<u8>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "kogen-gate-verify-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let base = root.join("base");
        let candidate = root.join("candidate");
        let run = root.join("run");
        let acceptance = b"approved acceptance test\n".to_vec();
        let intent = b"approved intent\n".to_vec();
        for workspace in [&base, &candidate] {
            fs::create_dir_all(workspace.join(".kogen/acceptance")).unwrap();
            fs::create_dir_all(workspace.join(".kogen/intents/greet")).unwrap();
            fs::write(workspace.join(".kogen/acceptance/greet.t.sh"), &acceptance).unwrap();
            fs::write(workspace.join(".kogen/intents/greet/intent.md"), &intent).unwrap();
            let output = Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(workspace)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        Self {
            root,
            base,
            candidate,
            run,
            acceptance,
            intent,
        }
    }

    fn request(&self, change_items: BTreeSet<String>) -> GateRequest {
        let source = ".kogen/acceptance/greet.t.sh";
        let candidate_test = "test/acceptance/greet.t.sh";
        let protection = ProtectedWorkspace::new(
            BTreeMap::from([
                (
                    ".kogen/intents/greet/intent.md".to_owned(),
                    ProtectedEntry {
                        sha256: intent_sha256(&self.intent),
                        bytes: Some(self.intent.clone()),
                    },
                ),
                (
                    candidate_test.to_owned(),
                    ProtectedEntry {
                        sha256: intent_sha256(&self.acceptance),
                        bytes: Some(self.acceptance.clone()),
                    },
                ),
                (
                    "ABSENT.txt".to_owned(),
                    ProtectedEntry {
                        sha256: ABSENT_SHA256.to_owned(),
                        bytes: None,
                    },
                ),
            ]),
            vec![source.to_owned()],
        )
        .unwrap();
        GateRequest {
            base_workspace: self.base.clone(),
            candidate_workspace: self.candidate.clone(),
            run_dir: self.run.clone(),
            environment: ChildEnvironment::new(),
            fixes: Vec::new(),
            checks: vec![CheckCommand {
                name: "lint".to_owned(),
                argv: vec![OsString::from("lint")],
                timeout: Duration::from_secs(1),
            }],
            approved_baseline: vec![CheckBaseline {
                name: "lint".to_owned(),
                status: CheckStatus::Green,
                exit_status: Some(0),
                findings: Vec::new(),
            }],
            acceptance: AcceptancePlan {
                slug: "greet".to_owned(),
                source_path: source.to_owned(),
                candidate_path: candidate_test.to_owned(),
                approved_bytes: self.acceptance.clone(),
                command: vec![OsString::from("accept")],
                timeout: Duration::from_secs(1),
                expected_items: BTreeSet::from(["A1".to_owned()]),
                change_items,
                adapter_unavailable: false,
            },
            protection,
        }
    }

    fn remove(self) {
        let _ = fs::remove_dir_all(self.root);
    }
}

struct Runner {
    check_exit: Option<i32>,
    ledger_status: LedgerStatus,
    calls: Mutex<Vec<String>>,
}

impl Runner {
    fn new(check_exit: Option<i32>, ledger_status: LedgerStatus) -> Self {
        Self {
            check_exit,
            ledger_status,
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl ProcessPort for Runner {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        self.calls.lock().unwrap().push(request.log_name.clone());
        let exit_status = if request.log_name == "acceptance" {
            let report = request
                .env
                .get(OsStr::new("KOGEN_LEDGER_REPORT"))
                .expect("ledger report path");
            let row = LedgerRow {
                tag: "greet/A1".to_owned(),
                test: "test accepted behavior".to_owned(),
                status: self.ledger_status,
            };
            fs::write(report, format!("{}\n", row.to_json_line().unwrap())).unwrap();
            Some(0)
        } else {
            self.check_exit
        };
        Ok(ProcessResult {
            exit_status,
            timed_out: false,
            unavailable: false,
            output_tail: Vec::new(),
            log_path: request.run_dir.join("fake.log"),
            duration_ms: 1,
            sandbox: None,
        })
    }
}

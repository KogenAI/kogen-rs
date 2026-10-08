use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

struct ScriptedRunner {
    bytes: Option<Vec<u8>>,
    exit_status: Option<i32>,
    unavailable: bool,
}

impl ProcessPort for ScriptedRunner {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        if let Some(bytes) = &self.bytes {
            let report = request
                .env
                .get(std::ffi::OsStr::new("KOGEN_LEDGER_REPORT"))
                .expect("report environment");
            fs::write(report, bytes).expect("write simulated report");
        }
        Ok(ProcessResult {
            exit_status: self.exit_status,
            timed_out: false,
            unavailable: self.unavailable,
            output_tail: Vec::new(),
            log_path: request.run_dir.join("logs/acceptance.log"),
            duration_ms: 1,
            sandbox: None,
        })
    }
}

struct ScriptedTree(Mutex<VecDeque<String>>);

struct MountableReportRunner(ScriptedRunner);

impl ProcessPort for MountableReportRunner {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        let report = request
            .env
            .get(std::ffi::OsStr::new("KOGEN_LEDGER_REPORT"))
            .unwrap();
        assert!(fs::metadata(report).unwrap().is_file());
        assert!(
            fs::read(report).unwrap().is_empty(),
            "stale rows reached the runner"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(report).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        self.0.run(request)
    }
}

#[test]
fn root_report_is_private_and_mountable_without_stale_rows_before_spawn() {
    let root = test_dir("mountable");
    let mut request = request(&root);
    request.report_path = request.run_dir.join("ledger.jsonl");
    fs::write(&request.report_path, "stale invalid row\n").unwrap();
    let runner = MountableReportRunner(ScriptedRunner {
        bytes: Some(b"{\"tag\":\"greet/A1\",\"test\":\"new\",\"status\":\"passed\"}\n".to_vec()),
        exit_status: Some(0),
        unavailable: false,
    });
    let tree = ScriptedTree(Mutex::new(["base".to_owned(), "base".to_owned()].into()));
    let result = run_command_acceptance(&runner, &tree, request).unwrap();
    assert!(result.item_pass["A1"]);
    assert!(result.failures.is_empty());
    let _ = fs::remove_dir_all(root);
}

impl TreeSnapshotPort for ScriptedTree {
    fn snapshot(&self, _workdir: &Path) -> Result<String, String> {
        self.0
            .lock()
            .expect("tree queue")
            .pop_front()
            .ok_or_else(|| "no snapshot".to_owned())
    }
}

struct SymlinkReportRunner {
    outside: PathBuf,
}

impl ProcessPort for SymlinkReportRunner {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        fs::remove_file(
            request
                .env
                .get(std::ffi::OsStr::new("KOGEN_LEDGER_REPORT"))
                .unwrap(),
        )
        .expect("remove prepared report");
        let reports = request.run_dir.join("reports");
        fs::remove_dir(&reports).expect("remove report directory");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&self.outside, &reports).expect("redirect report directory");
        Ok(ProcessResult {
            exit_status: Some(0),
            timed_out: false,
            unavailable: false,
            output_tail: Vec::new(),
            log_path: request.run_dir.join("logs/acceptance.log"),
            duration_ms: 1,
            sandbox: None,
        })
    }
}

#[test]
fn absent_ledger_with_missing_runner_is_tool_missing() {
    let root = test_dir("absent");
    let runner = ScriptedRunner {
        bytes: None,
        exit_status: Some(127),
        unavailable: true,
    };
    let tree = ScriptedTree(Mutex::new(["base".to_owned(), "base".to_owned()].into()));
    let result = run_command_acceptance(&runner, &tree, request(&root)).expect("acceptance result");
    assert!(result.failures.contains(&AcceptanceFailure::ToolMissing));
    assert!(!result.failures.contains(&AcceptanceFailure::NoTaggedTests));
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn acceptance_report_read_rejects_a_child_replaced_parent_symlink() {
    let root = test_dir("report-symlink");
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let row = LedgerRow {
        tag: "greet/A1".to_owned(),
        test: "external report must not be read".to_owned(),
        status: LedgerStatus::Passed,
    };
    fs::write(
        outside.join("ledger.jsonl"),
        format!("{}\n", row.to_json_line().unwrap()),
    )
    .unwrap();
    let runner = SymlinkReportRunner {
        outside: outside.clone(),
    };
    let tree = ScriptedTree(Mutex::new(["base".to_owned(), "base".to_owned()].into()));
    let result = run_command_acceptance(&runner, &tree, request(&root)).expect("acceptance result");
    assert!(result.rows.is_empty());
    assert!(result.failures.contains(&AcceptanceFailure::NoTaggedTests));
    assert_eq!(
        fs::read_to_string(outside.join("ledger.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn malformed_jsonl_row_is_ledger_invalid() {
    let root = test_dir("malformed");
    let runner = ScriptedRunner {
        bytes: Some(
            b"{\"tag\":\"greet/A1\",\"test\":\"test one\",\"status\":\"unknown\"}\n".to_vec(),
        ),
        exit_status: Some(0),
        unavailable: false,
    };
    let tree = ScriptedTree(Mutex::new(["base".to_owned(), "base".to_owned()].into()));
    let result = run_command_acceptance(&runner, &tree, request(&root)).expect("acceptance result");
    assert!(matches!(
        result.failures.as_slice(),
        [AcceptanceFailure::LedgerInvalid { line: Some(1), .. }]
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn changed_tree_adds_tree_mutated_even_when_item_passes() {
    let root = test_dir("mutation");
    let row = LedgerRow {
        tag: "greet/A1".to_owned(),
        test: "test one".to_owned(),
        status: LedgerStatus::Passed,
    };
    let runner = ScriptedRunner {
        bytes: Some(format!("{}\n", row.to_json_line().unwrap()).into_bytes()),
        exit_status: Some(0),
        unavailable: false,
    };
    let tree = ScriptedTree(Mutex::new(["before".to_owned(), "after".to_owned()].into()));
    let result = run_command_acceptance(&runner, &tree, request(&root)).expect("acceptance result");
    assert!(result.failures.contains(&AcceptanceFailure::TreeMutated));
    assert_eq!(result.item_pass.get("A1"), Some(&true));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn empty_successful_report_means_no_tagged_tests() {
    let root = test_dir("empty");
    let runner = ScriptedRunner {
        bytes: Some(Vec::new()),
        exit_status: Some(0),
        unavailable: false,
    };
    let tree = ScriptedTree(Mutex::new(["same".to_owned(), "same".to_owned()].into()));
    let result = run_command_acceptance(&runner, &tree, request(&root)).expect("acceptance result");
    assert!(result.failures.contains(&AcceptanceFailure::NoTaggedTests));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn only_passed_rows_satisfy_an_acceptance_item() {
    let root = test_dir("status");
    let first = LedgerRow {
        tag: "greet/A1".to_owned(),
        test: "test one".to_owned(),
        status: LedgerStatus::Passed,
    };
    let second = LedgerRow {
        tag: "greet/A1".to_owned(),
        test: "test two".to_owned(),
        status: LedgerStatus::Skipped,
    };
    let bytes = format!(
        "{}\n{}\n",
        first.to_json_line().unwrap(),
        second.to_json_line().unwrap()
    )
    .into_bytes();
    let runner = ScriptedRunner {
        bytes: Some(bytes),
        exit_status: Some(0),
        unavailable: false,
    };
    let tree = ScriptedTree(Mutex::new(["same".to_owned(), "same".to_owned()].into()));
    let result = run_command_acceptance(&runner, &tree, request(&root)).expect("acceptance result");
    assert_eq!(result.item_pass.get("A1"), Some(&false));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn ledger_rows_serialize_to_the_exact_four_field_json_shape() {
    let row = LedgerRow {
        tag: "greet/A1".to_owned(),
        test: "test says hello".to_owned(),
        status: LedgerStatus::Passed,
    };
    assert_eq!(
        row.to_json_line().unwrap(),
        r#"{"tag":"greet/A1","test":"test says hello","status":"passed"}"#
    );
}

#[test]
fn path_placeholder_is_replaced_even_inside_an_argument() {
    let candidate = Path::new("/workspace/test.t.sh");
    let argument = replace_candidate_path("run {path} now".into(), candidate);
    assert_eq!(argument, "run /workspace/test.t.sh now");
}

fn request(root: &Path) -> CommandAcceptanceRequest {
    let run_dir = root.join("run");
    let workdir = root.join("work");
    fs::create_dir_all(&run_dir).expect("create run directory");
    fs::create_dir_all(&workdir).expect("create workdir");
    CommandAcceptanceRequest {
        slug: "greet".to_owned(),
        command: vec!["kt-runner".into(), "--test".into(), "{path}".into()],
        candidate_path: workdir.join("greet.t.sh"),
        workdir,
        run_dir: run_dir.clone(),
        report_path: run_dir.join("reports/ledger.jsonl"),
        env: ChildEnvironment::new(),
        timeout: Duration::from_secs(5),
        expected_items: BTreeSet::from(["A1".to_owned()]),
        adapter_unavailable: false,
    }
}

fn test_dir(label: &str) -> PathBuf {
    static NEXT_DIR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "kogen-ledger-{label}-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create temp directory");
    path
}

use super::*;
use std::fs;

#[test]
fn matching_base_red_findings_are_excused_but_new_identities_are_not() {
    let baseline = CheckBaseline {
        name: "lint".to_owned(),
        status: CheckStatus::Red,
        exit_status: Some(1),
        findings: vec![finding("lib/a.rs", "lint/todo", "", "old text", 1)],
    };
    let same = CheckResult {
        name: "lint".to_owned(),
        program: "lint".to_owned(),
        status: CheckStatus::Red,
        exit_status: Some(1),
        findings: vec![finding("lib/a.rs", "lint/todo", "", "changed text", 9)],
        changed_paths: Vec::new(),
        log_path: PathBuf::new(),
        duration_ms: 1,
        timeout: Duration::from_secs(1),
        excused: false,
    };
    assert!(is_excused(&baseline, &same));
    let extra = CheckResult {
        findings: vec![
            same.findings[0].clone(),
            finding("lib/b.rs", "lint/new", "", "new issue", 2),
        ],
        ..same.clone()
    };
    assert!(!is_excused(&baseline, &extra));
    let green = CheckBaseline {
        status: CheckStatus::Green,
        ..baseline
    };
    assert!(!is_excused(&green, &same));
}

#[test]
fn check_mutation_is_reported_and_restored_before_return() {
    let root = test_dir("mutating");
    fs::write(root.join("source.txt"), b"baseline").unwrap();
    let runner = EditingRunner;
    let command = CheckCommand {
        name: "edit".to_owned(),
        argv: vec![OsString::from("edit")],
        timeout: Duration::from_secs(1),
    };
    let result = run_check(
        &runner,
        &command,
        &root,
        &root.join("run"),
        &ChildEnvironment::new(),
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Mutating);
    assert_eq!(result.changed_paths, vec!["source.txt"]);
    assert_eq!(fs::read(root.join("source.txt")).unwrap(), b"baseline");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn gnu_finding_identity_ignores_position_and_message() {
    let parsed = parse_gnu_finding(
        "lib/a.ex:4:8: error: [exunit/failure] test says hello: assertion failed",
    )
    .unwrap();
    assert_eq!(parsed.path, "lib/a.ex");
    assert_eq!(parsed.line, Some(4));
    assert_eq!(parsed.column, Some(8));
    assert_eq!(parsed.symbol, "test says hello");
    assert_eq!(parsed.message, "assertion failed");
}

#[test]
fn kt_test_findings_keep_the_test_symbol_in_the_identity() {
    let parsed =
        parse_gnu_finding("test/unit/greet.t.sh:1:1: error: [kt/test] alpha: failed").unwrap();
    assert_eq!(parsed.symbol, "alpha");
    assert_eq!(parsed.message, "failed");
}

struct EditingRunner;

impl ProcessPort for EditingRunner {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        fs::write(request.cwd.join("source.txt"), b"changed").unwrap();
        Ok(ProcessResult {
            exit_status: Some(0),
            timed_out: false,
            unavailable: false,
            output_tail: Vec::new(),
            log_path: request.run_dir.join("edit.log"),
            duration_ms: 1,
            sandbox: None,
        })
    }
}

fn finding(path: &str, rule: &str, symbol: &str, message: &str, line: u32) -> CheckFinding {
    CheckFinding {
        path: path.to_owned(),
        rule: rule.to_owned(),
        symbol: symbol.to_owned(),
        message: message.to_owned(),
        line: Some(line),
        column: None,
    }
}

fn test_dir(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "kogen-gate-check-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

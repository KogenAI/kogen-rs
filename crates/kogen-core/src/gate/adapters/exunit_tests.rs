use super::*;
use crate::gate::ledger::{CommandAcceptanceRequest, TreeSnapshotPort};
use crate::run::{ProcessError, ProcessPort, ProcessRequest, ProcessResult};
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_directory(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("kogen-{name}-{}-{nonce}", std::process::id()))
}

#[test]
fn exunit_paths_and_setup_seeds_are_fixed() {
    assert_eq!(
        source_path("greet"),
        PathBuf::from(".kogen/acceptance/greet_test.exs")
    );
    assert_eq!(
        candidate_path("greet"),
        PathBuf::from("test/acceptance/greet_test.exs")
    );
    assert_eq!(setup_seeds(), &["deps", "_build"]);
}

#[test]
fn formatter_is_derived_from_the_first_format_check_or_mix_format() {
    let check = CheckCommand {
        name: "format".to_owned(),
        argv: words([
            "mix",
            "format",
            "--check-formatted",
            "--dot-formatter",
            ".formatter.exs",
        ]),
        timeout: std::time::Duration::from_secs(10),
    };
    assert_eq!(
        formatter(&[check], Path::new("test/a_test.exs")),
        Some(words([
            "mix",
            "format",
            "--dot-formatter",
            ".formatter.exs"
        ]))
    );
    assert_eq!(
        formatter(&[], Path::new("lib/a.ex")),
        Some(words(["mix", "format", "lib/a.ex"]))
    );
    assert_eq!(formatter(&[], Path::new("README.md")), None);
}

#[test]
fn formatter_stays_in_the_run_directory_and_is_private() {
    let root = temporary_directory("exunit-formatter");
    std::fs::create_dir_all(&root).unwrap();
    let path = write_formatter(&root).unwrap();
    assert_eq!(path, root.join("ledger_formatter.ex"));
    assert_eq!(std::fs::read(&path).unwrap(), formatter_source().as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let _ = std::fs::remove_file(root.join("ledger_formatter.ex"));
    let _ = std::fs::remove_dir(root);
}

#[test]
fn runner_command_loads_the_run_formatter_and_runs_both_formatters() {
    let command = runner_command(Path::new("/run dir/ledger_formatter.ex"), true).unwrap();
    assert_eq!(command[0], "mise");
    assert_eq!(command[1], "exec");
    assert_eq!(command[2], "--");
    assert_eq!(command[3], "elixir");
    assert_eq!(
        command[5],
        r#"Code.require_file("/run dir/ledger_formatter.ex"); Code.ensure_loaded!(KogenLedgerFormatter)"#
    );
    assert_eq!(command[7], "mix");
    assert_eq!(command[9], "--formatter");
    assert_eq!(command[10], "KogenLedgerFormatter");
    assert_eq!(command[12], "ExUnit.CLIFormatter");
    assert_eq!(command[13], "{path}");
}

#[test]
fn unavailable_detection_reads_only_the_first_twenty_lines() {
    assert!(unavailable(
        b"/usr/bin/env: erl: No such file or directory\n"
    ));
    assert!(unavailable(b"elixir: command not found\n"));
    let mut later = b"ordinary output\n".repeat(20);
    later.extend_from_slice(b"mix: command not found\n");
    assert!(!unavailable(&later));
    assert!(!unavailable(b"mix test failed because test is missing\n"));
}

#[test]
fn missing_erl_log_overrides_an_empty_report_to_tool_missing() {
    struct MissingErl;

    impl ProcessPort for MissingErl {
        fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
            let log_path = request.run_dir.join("logs/acceptance.log");
            std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
            std::fs::write(&log_path, b"/usr/bin/env: erl: No such file or directory\n").unwrap();
            Ok(ProcessResult {
                exit_status: Some(1),
                timed_out: false,
                unavailable: false,
                output_tail: Vec::new(),
                log_path,
                duration_ms: 1,
                sandbox: None,
            })
        }
    }

    struct StableTree;

    impl TreeSnapshotPort for StableTree {
        fn snapshot(&self, _workdir: &Path) -> Result<String, String> {
            Ok("same".to_owned())
        }
    }

    let root = temporary_directory("exunit-missing-erl");
    let workspace = root.join("workspace");
    let run_dir = root.join("run");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&run_dir).unwrap();
    let request = CommandAcceptanceRequest {
        slug: "greet".to_owned(),
        command: Vec::new(),
        candidate_path: workspace.join("test/acceptance/greet_test.exs"),
        workdir: workspace,
        run_dir: run_dir.clone(),
        report_path: run_dir.join("ledger.jsonl"),
        env: Default::default(),
        timeout: std::time::Duration::from_secs(10),
        expected_items: std::collections::BTreeSet::from(["A1".to_owned()]),
        adapter_unavailable: false,
    };
    let result = run_acceptance(&MissingErl, &StableTree, request, false).unwrap();
    assert!(result.process.unavailable);
    assert!(result.failures.contains(&AcceptanceFailure::ToolMissing));
    assert!(
        !result
            .failures
            .contains(&AcceptanceFailure::AcceptanceCompileFailed)
    );
}

#[test]
fn exunit_failures_compile_errors_credo_and_format_paths_are_parsed() {
    let output = "  1) test legacy is broken (HelloTest)\n     test/hello_test.exs:3\n     Assertion with == failed\n** (CompileError) lib/hello.ex:9: undefined function Hello.foo/0\n┃ [W] ↗ Credo.Check.Readability.ModuleDoc: Add a moduledoc.\n┃ lib/hello.ex:4:2\nThe following files are not formatted:\n  * test/hello_test.exs\n".as_bytes();
    let findings = parse_findings(output, Path::new("/tmp/project"));
    assert_eq!(findings.len(), 4, "{findings:#?}");
    assert_eq!(findings[0].path, "test/hello_test.exs");
    assert_eq!(findings[0].rule, "exunit/assertion");
    assert_eq!(findings[0].symbol, "legacy is broken");
    assert_eq!(findings[1].rule, "compile/undefined");
    assert_eq!(findings[1].path, "lib/hello.ex");
    assert_eq!(findings[2].rule, "credo/Readability.ModuleDoc");
    assert_eq!(findings[3].rule, "format/unformatted");
}

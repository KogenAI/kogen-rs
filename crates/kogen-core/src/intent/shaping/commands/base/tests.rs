use super::*;
use crate::gate::ledger::{LedgerRow, LedgerStatus};
use crate::intent::shaping::{journal, prompts};
use std::path::{Path, PathBuf};

const SYN_06_TEST: &str = include_str!("../../testdata/syn-06-r6_test.exs");
const SYN_20_TEST: &str = include_str!("../../testdata/syn-20-r6_test.exs");
const SYN_06_LOG: &str = include_str!("../../testdata/syn-06-r6-acceptance.log");
const SYN_20_LOG: &str = include_str!("../../testdata/syn-20-r6-acceptance.log");

fn run(log: &str, count: usize) -> CommandAcceptanceResult {
    CommandAcceptanceResult {
        process: ProcessResult {
            exit_status: Some(1),
            timed_out: false,
            unavailable: false,
            output_tail: log.as_bytes().to_vec(),
            log_path: PathBuf::from("/nonexistent-kogen-test/acceptance.log"),
            duration_ms: 1,
            sandbox: None,
        },
        rows: Vec::new(),
        item_pass: (1..=count).map(|n| (format!("A{n}"), false)).collect(),
        failures: vec![AcceptanceFailure::AcceptanceCompileFailed],
    }
}

fn executed_rows(slug: &str, count: usize, tagged: bool) -> Vec<LedgerRow> {
    (1..=count)
        .map(|n| LedgerRow {
            tag: if tagged {
                format!("{slug}/A{n}")
            } else {
                String::new()
            },
            test: format!("item {n}"),
            status: if n == count {
                LedgerStatus::Passed
            } else {
                LedgerStatus::Failed
            },
        })
        .collect()
}

#[test]
fn r6_executed_tests_with_wrong_tags_request_exact_tags_instead_of_becoming_red() {
    for (slug, source, log, count) in [
        (
            "syn-06-migration-ticket-numbers",
            SYN_06_TEST,
            SYN_06_LOG,
            5,
        ),
        ("syn-20-email-invite-flow", SYN_20_TEST, SYN_20_LOG, 6),
    ] {
        assert_eq!(source.matches("@tag acceptance:").count(), count);
        assert!(!source.contains("@tag intent:"));
        assert!(log.contains(&format!("Result: 1/{count} passed")));
        let mut result = run(log, count);
        // The corrected formatter emits the executed untagged rows, like R74.
        result.rows = executed_rows(slug, count, false);
        result.failures.clear();
        let failure = assess(&result, slug, "elixir").unwrap_err();
        assert_eq!(failure.reason, "acceptance_missing_on_base");
        for n in 1..=count {
            assert!(
                failure
                    .detail
                    .contains(&format!("@tag intent: \"{slug}/A{n}\""))
            );
        }
        assert!(
            failure
                .detail
                .contains("Acceptance test: item 1 [] (Failed)")
        );
        assert!(failure.detail.contains(log.lines().next().unwrap()));
        let feedback = prompts::validation_feedback(&failure);
        assert!(
            prompts::repair_message(Path::new("intent.md"), Path::new("test.exs"), &feedback)
                .ends_with(&feedback)
        );
        assert_eq!(
            journal::feedback_value(2, "validation", &feedback)["feedback"],
            feedback
        );
        let diagnostic = diagnostics(1, slug, "test.exs", &result, &Err(failure));
        assert_eq!(diagnostic["missing_tags"].as_array().unwrap().len(), count);
        assert_eq!(diagnostic["rows"].as_array().unwrap().len(), count);

        // Correcting the tags lets the real red/keep mix reach reclassification.
        result.rows = executed_rows(slug, count, true);
        result.item_pass.insert(format!("A{count}"), true);
        assert_eq!(
            assess(&result, slug, "elixir").unwrap(),
            BTreeSet::from([format!("A{count}")])
        );
        let diagnostic = diagnostics(
            2,
            slug,
            "test.exs",
            &result,
            &assess(&result, slug, "elixir"),
        );
        assert!(diagnostic["missing_tags"].as_array().unwrap().is_empty());
        assert!(diagnostic["reason"].is_null());
    }
}

#[test]
fn compile_failure_in_new_acceptance_or_project_source_preserves_the_exact_error() {
    for path in [".kogen/acceptance/slug_test.exs", "lib/existing.ex"] {
        let error = format!(
            "== Compilation error in file {path} ==\n** (CompileError) {path}:42: NewFeature.__struct__/1 is undefined, cannot expand struct NewFeature"
        );
        let modern_error =
            "error: NewFeature.__struct__/1 is undefined, cannot expand struct NewFeature";
        let log = format!(
            "{}\n{modern_error}\n{error}\n",
            "dependency progress\n".repeat(30)
        );
        let result = run(&log, 2);
        let failure = assess(&result, "slug", "elixir").unwrap_err();
        assert_eq!(failure.reason, "acceptance_compile_failed");
        assert!(failure.detail.contains(&error));
        assert!(failure.detail.contains(modern_error));
        let feedback = prompts::validation_feedback(&failure);
        let repair =
            prompts::repair_message(Path::new("intent.md"), Path::new("test.exs"), &feedback);
        assert!(repair.contains(&error));
        let diagnostic = diagnostics(1, "slug", "test.exs", &result, &Err(failure));
        assert!(diagnostic["detail"].as_str().unwrap().contains(&error));
        assert_eq!(diagnostic["reason"], "acceptance_compile_failed");

        let log = format!(
            "== Compilation error in file {path} ==\n{}\n{modern_error}",
            "diagnostic context\n".repeat(25)
        );
        let failure = assess(&run(&log, 2), "slug", "elixir").unwrap_err();
        assert!(failure.detail.contains(modern_error));
    }
}

#[test]
fn missing_rows_invalid_ledgers_timeouts_and_unavailable_runners_do_not_prove_red() {
    let mut result = run("", 2);
    result.failures = vec![AcceptanceFailure::NoTaggedTests];
    assert_eq!(
        assess(&result, "slug", "elixir").unwrap_err().reason,
        "no_tagged_tests"
    );
    result.failures = vec![AcceptanceFailure::LedgerInvalid {
        line: Some(1),
        detail: "broken JSON".to_owned(),
    }];
    assert_eq!(
        assess(&result, "slug", "elixir").unwrap_err().reason,
        "ledger_invalid"
    );
    result.failures = vec![
        AcceptanceFailure::AcceptanceTimeout,
        AcceptanceFailure::AcceptanceCompileFailed,
    ];
    result.process.timed_out = true;
    assert_eq!(
        assess(&result, "slug", "elixir").unwrap_err().reason,
        "acceptance_timeout"
    );
    result.failures.push(AcceptanceFailure::TreeMutated);
    assert_eq!(
        assess(&result, "slug", "elixir").unwrap_err().reason,
        "tree_mutated"
    );
    result.process.unavailable = true;
    assert_eq!(
        assess(&result, "slug", "elixir").unwrap_err().reason,
        "tool_missing"
    );
    result.process.unavailable = false;
    result.process.exit_status = Some(127);
    assert_eq!(
        assess(&result, "slug", "elixir").unwrap_err().reason,
        "tool_missing"
    );
}

#[test]
fn partially_tagged_rows_and_unknown_ids_require_repair() {
    let mut result = run("", 2);
    result.failures.clear();
    result.rows = executed_rows("slug", 1, true);
    assert_eq!(
        assess(&result, "slug", "elixir").unwrap_err().reason,
        "acceptance_missing_on_base"
    );
    result.rows = executed_rows("slug", 3, true);
    assert_eq!(
        assess(&result, "slug", "elixir").unwrap_err().reason,
        "unknown_acceptance_id"
    );
}

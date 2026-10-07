//! Base results require executed, tagged rows; a compiler failure proves no item.

use super::super::validation::ValidationFailure;
use crate::gate::ledger::{AcceptanceFailure, CommandAcceptanceResult};
use crate::run::ProcessResult;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;

pub(super) fn assess(
    result: &CommandAcceptanceResult,
    slug: &str,
    runner_name: &str,
) -> Result<BTreeSet<String>, ValidationFailure> {
    let failure = |reason, detail: String| ValidationFailure { reason, detail };
    if result.process.unavailable || matches!(result.process.exit_status, Some(126 | 127)) {
        return Err(failure(
            "tool_missing",
            format!("{runner_name} is not available"),
        ));
    }
    if result.failures.contains(&AcceptanceFailure::TreeMutated) {
        return Err(failure(
            "tree_mutated",
            "base acceptance run changed the checkout tree".to_owned(),
        ));
    }
    if let Some(problem) = result
        .failures
        .iter()
        .find(|problem| !matches!(problem, AcceptanceFailure::Suite))
    {
        let (reason, detail) = match problem {
            AcceptanceFailure::AcceptanceCompileFailed => (
                "acceptance_compile_failed",
                "Acceptance test file failed to compile or load; no executed test rows were recorded.".to_owned(),
            ),
            AcceptanceFailure::NoTaggedTests => ("no_tagged_tests", "No executed test rows were recorded.".to_owned()),
            AcceptanceFailure::AcceptanceTimeout => ("acceptance_timeout", "Base acceptance run timed out.".to_owned()),
            AcceptanceFailure::LedgerInvalid { .. } => ("ledger_invalid", format!("{problem:?}")),
            _ => ("acceptance_failed", format!("{problem:?}")),
        };
        return Err(failure(reason, feedback(result, slug, &detail)));
    }
    let prefix = format!("{slug}/");
    let unknown = result
        .rows
        .iter()
        .filter(|row| {
            row.tag
                .strip_prefix(&prefix)
                .is_some_and(|id| !result.item_pass.contains_key(id))
        })
        .map(|row| row.tag.as_str())
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(failure(
            "unknown_acceptance_id",
            feedback(result, slug, &unknown.join(", ")),
        ));
    }
    if !missing_tags(result, slug).is_empty() {
        return Err(failure(
            "acceptance_missing_on_base",
            feedback(
                result,
                slug,
                "Base results are missing required Intent tags.",
            ),
        ));
    }
    Ok(result
        .item_pass
        .iter()
        .filter_map(|(id, passed)| passed.then_some(id.clone()))
        .collect())
}

fn missing_tags(result: &CommandAcceptanceResult, slug: &str) -> Vec<String> {
    result
        .item_pass
        .keys()
        .map(|id| format!("{slug}/{id}"))
        .filter(|tag| !result.rows.iter().any(|row| &row.tag == tag))
        .collect()
}

fn feedback(result: &CommandAcceptanceResult, slug: &str, detail: &str) -> String {
    let mut message = detail.to_owned();
    for tag in missing_tags(result, slug) {
        message.push_str(&format!(
            "\nno tests tagged intent: {tag}; use @tag intent: \"{tag}\""
        ));
    }
    for row in &result.rows {
        message.push_str(&format!(
            "\nAcceptance test: {} [{}] ({:?})",
            row.test, row.tag, row.status
        ));
    }
    message.push_str(&format!(
        "\nExit status: {:?}; log: {}\nOutput:\n{}",
        result.process.exit_status,
        result.process.log_path.display(),
        output(&result.process)
    ));
    message
}

/// Keep the reference's leading context and any later compiler diagnostic.
pub(super) fn output(process: &ProcessResult) -> String {
    let bytes = fs::read(&process.log_path).unwrap_or_else(|_| process.output_tail.clone());
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().collect::<Vec<_>>();
    let mut excerpt = lines.iter().take(20).copied().collect::<Vec<_>>();
    if let Some(index) = lines.iter().position(|line| {
        line.contains("Compilation error in file")
            || ["(CompileError)", "(SyntaxError)", "(TokenMissingError)"]
                .iter()
                .any(|marker| line.contains(marker))
    }) {
        // Recent Elixir versions print the specific `error:` above CompileError.
        let start = index.saturating_sub(20).max(20);
        let end = (index + 40).min(lines.len());
        if start < end {
            excerpt.push("[compiler diagnostic after leading output]");
            excerpt.extend(lines[start..end].iter().copied());
        }
    }
    if excerpt.is_empty() {
        "(no output captured)".to_owned()
    } else {
        excerpt.join("\n")
    }
}

pub(super) fn diagnostics(
    pass: usize,
    slug: &str,
    source_path: &str,
    result: &CommandAcceptanceResult,
    assessed: &Result<BTreeSet<String>, ValidationFailure>,
) -> Value {
    json!({
        "kind": "shape_base_acceptance",
        "pass_index": pass,
        "source_path": source_path,
        "exit_status": result.process.exit_status,
        "timed_out": result.process.timed_out,
        "unavailable": result.process.unavailable,
        "log_path": result.process.log_path,
        "rows": result.rows,
        "item_pass": result.item_pass,
        "missing_tags": missing_tags(result, slug),
        "reason": assessed.as_ref().err().map(|failure| failure.reason),
        "detail": assessed.as_ref().err().map(|failure| &failure.detail),
    })
}

#[cfg(test)]
mod tests;

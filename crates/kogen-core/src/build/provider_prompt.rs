use crate::error::CoreError;
use crate::provider::session::ConversationHistory;
use crate::run::RunStore;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) fn planner_instructions() -> String {
    "You are Kogen's planner. Produce a one-shot implementation plan for a cheaper coding agent. Return Difficulty: easy or hard, then ## Acceptance criteria, ## Technical approach, and ## Implementation steps.".to_owned()
}

pub(super) fn auditor_instructions() -> String {
    format!(
        "{} Check whether each failed acceptance test follows the verbatim Request. Reply with JSON only in this form: {{\"items\":[{{\"id\":\"A1\",\"verdict\":\"valid|over_strict|contradicts\",\"reason\":\"...\"}}]}}.",
        crate::run::orchestration::BUILD_AUDITOR_MARKER
    )
}

pub(super) fn witness_auditor_instructions() -> String {
    format!(
        "{} For each failing witness assertion, decide whether the test is wrong, the witness implementation is wrong, or the evidence is insufficient. Reply with JSON only in this form: {{\"items\":[{{\"id\":\"A1\",\"verdict\":\"TEST-WRONG|WITNESS-WRONG|UNDECIDED\",\"citation\":\"…\",\"reason\":\"…\"}}]}}.",
        crate::run::orchestration::BUILD_AUDITOR_MARKER
    )
}

pub(super) fn builder_instructions(direct: bool) -> String {
    let access = if direct {
        "Use read, search, edit, write and shell tools as needed."
    } else {
        "Use the shell tool to make and verify the implementation."
    };
    format!(
        "You are Kogen's builder. Implement the approved Intent. The Intent and acceptance test are read-only. The Request is context and the Acceptance section is the gate. The plan is advice. Add no dependencies the Intent does not ask for. Ignore AGENTS.md and CLAUDE.md. {access} Call finish alone with {{}} when implementation and targeted verification are complete."
    )
}

pub(super) fn builder_message(intent: &[u8], acceptance: &str, plan: &str) -> String {
    let plan = if plan.is_empty() {
        "No implementation plan was supplied.".to_owned()
    } else {
        format!("Implementation plan:\n{plan}")
    };
    format!(
        "Approved Intent:\n{}\n\nAcceptance on the base:\n{}\n\n{}\n\nRepairs available: 6. Begin work in the supplied worktree.",
        String::from_utf8_lossy(intent),
        acceptance,
        plan,
    )
}

pub(super) fn user_item(text: &str) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":text}]})
}

pub(super) fn usage_value(usage: &crate::provider::ModelUsage) -> Value {
    json!({
        "input":usage.input,
        "cached_input":usage.cached_input,
        "cache_write":usage.cache_write,
        "output":usage.output,
        "reasoning":usage.reasoning,
    })
}

pub(super) fn append_turn_budget_note(
    history: &mut ConversationHistory,
    completed_turns: u32,
    already_added: &mut bool,
) {
    if completed_turns >= 48 && !*already_added {
        history.append_user(format!(
            "System note: {} turns remain. Run the targeted tests now and finish the smallest complete change.",
            60_u32.saturating_sub(completed_turns)
        ));
        *already_added = true;
    }
}

pub(super) fn append_transcript(store: &RunStore, row: Value) -> Result<(), CoreError> {
    store
        .append_transcript(&row)
        .map_err(|error| super::controller_error("transcript_write_failed", error.to_string()))
}

pub(super) fn workspace_tree(
    workspace: &Path,
    excluded_paths: &[PathBuf],
) -> Result<String, crate::gate::TreeSnapshotError> {
    crate::gate::snapshot_tree_excluding(workspace, excluded_paths)
}

pub(super) fn workspace_changed(
    workspace: &Path,
    baseline_tree: &str,
    excluded_paths: &[PathBuf],
) -> Result<bool, crate::gate::TreeSnapshotError> {
    Ok(workspace_tree(workspace, excluded_paths)? != baseline_tree)
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
#[path = "provider_prompt_tests.rs"]
mod tests;

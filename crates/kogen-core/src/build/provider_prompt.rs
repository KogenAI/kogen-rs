use crate::error::CoreError;
use crate::provider::session::ConversationHistory;
use crate::run::RunStore;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

const CONTEXT_PACKET_MAX_TOKENS: usize = 2_000;
// The packet is UTF-8 text. A byte-level tokenizer cannot emit more than one
// token per input byte, so this byte cap is a tokenizer-independent upper bound.
const CONTEXT_PACKET_MAX_BYTES: usize = CONTEXT_PACKET_MAX_TOKENS;
const CONTEXT_FILE_MAX_BYTES: u64 = 256_000;
const CONTEXT_LINE_MAX_BYTES: usize = 768;

struct ContextLine {
    score: usize,
    path: String,
    line_number: usize,
    text: String,
}

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

pub(super) fn public_context_packet(workspace: &Path, intent: &[u8]) -> String {
    let query = context_terms(&String::from_utf8_lossy(intent));
    if query.is_empty() {
        return String::new();
    }

    let Ok(root) = workspace.canonicalize() else {
        return String::new();
    };
    let tracked =
        match crate::git::GitRepo::workspace(workspace).output(&["ls-files", "--cached", "-z"]) {
            Ok(tracked) => tracked,
            Err(_) => return String::new(),
        };
    let mut candidates = Vec::new();
    for raw_path in tracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let Ok(path) = std::str::from_utf8(raw_path) else {
            continue;
        };
        if !is_public_context_path(path) {
            continue;
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            continue;
        }
        let file = workspace.join(relative);
        let Ok(metadata) = fs::symlink_metadata(&file) else {
            continue;
        };
        if !metadata.file_type().is_file() || metadata.len() > CONTEXT_FILE_MAX_BYTES {
            continue;
        }
        let Ok(canonical_file) = file.canonicalize() else {
            continue;
        };
        if !canonical_file.starts_with(&root) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&canonical_file) else {
            continue;
        };
        let path_terms = context_terms(path);
        for (index, line) in contents.lines().enumerate() {
            let text = line.trim();
            if text.is_empty() || text.len() > CONTEXT_LINE_MAX_BYTES || contains_private_path(text)
            {
                continue;
            }
            let line_terms = context_terms(text);
            let line_hits = query.intersection(&line_terms).count();
            if line_hits == 0 {
                continue;
            }
            let path_hits = query.intersection(&path_terms).count();
            candidates.push(ContextLine {
                score: line_hits.saturating_mul(8).saturating_add(path_hits),
                path: path.to_owned(),
                line_number: index + 1,
                text: text.to_owned(),
            });
        }
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line_number.cmp(&right.line_number))
    });

    let mut packet = String::from("Context packet (provided):");
    let mut selected = 0;
    for candidate in candidates {
        let item = format!(
            "\n- [{}:L{}] {}",
            candidate.path, candidate.line_number, candidate.text
        );
        if packet.len().saturating_add(item.len()) > CONTEXT_PACKET_MAX_BYTES {
            continue;
        }
        packet.push_str(&item);
        selected += 1;
    }
    if selected == 0 { String::new() } else { packet }
}

pub(super) fn append_context_packet(message: &str, packet: &str) -> String {
    if packet.is_empty() {
        message.to_owned()
    } else {
        format!("{message}\n\n{packet}")
    }
}

#[cfg(test)]
pub(super) fn context_packet_token_upper_bound(packet: &str) -> usize {
    packet.len()
}

fn is_public_context_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') {
        return false;
    }
    let components = path.split('/').collect::<Vec<_>>();
    if components.iter().any(|part| {
        part.is_empty()
            || *part == "."
            || *part == ".."
            || part.starts_with('.')
            || matches!(
                part.to_ascii_lowercase().as_str(),
                "acceptance"
                    | "grader"
                    | "grading"
                    | "hidden"
                    | "private"
                    | "secrets"
                    | "credentials"
                    | "test"
                    | "tests"
                    | "__tests__"
                    | "fixtures"
                    | "fixture"
                    | "internal"
                    | "node_modules"
                    | "vendor"
                    | "target"
                    | "dist"
                    | "coverage"
            )
    }) {
        return false;
    }
    let Some(name) = components.last() else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "agents.md"
            | "claude.md"
            | "cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
    ) || lower.starts_with(".env")
        || lower.ends_with(".pem")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_tests.rs")
        || lower.ends_with("_tests.go")
        || lower.contains(".test.")
        || lower.contains(".spec.")
    {
        return false;
    }
    if path == "docs/work" || path.starts_with("docs/work/") {
        return false;
    }
    true
}

fn contains_private_path(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "/users/",
        "/home/",
        "/private/",
        "/tmp/",
        "/var/",
        "\\users\\",
        ":\\",
        ".kogen",
        "test/acceptance/",
        "tests/acceptance/",
        "secrets/",
        "credentials/",
        "agents.md",
        "claude.md",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn context_terms(text: &str) -> BTreeSet<String> {
    const STOP_WORDS: &[&str] = &[
        "about", "after", "again", "also", "and", "are", "been", "before", "being", "both",
        "builder", "build", "context", "does", "each", "from", "have", "into", "only", "over",
        "packet", "public", "same", "should", "spec", "such", "than", "that", "their", "them",
        "then", "there", "these", "they", "this", "those", "through", "under", "using", "what",
        "when", "where", "which", "with", "within", "would",
    ];
    let mut terms = BTreeSet::new();
    let mut word = String::new();
    let flush = |word: &mut String, terms: &mut BTreeSet<String>| {
        if word.len() >= 3 && !STOP_WORDS.contains(&word.as_str()) {
            terms.insert(std::mem::take(word));
        } else {
            word.clear();
        }
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            word.push(character.to_ascii_lowercase());
        } else if !word.is_empty() {
            flush(&mut word, &mut terms);
        }
    }
    if !word.is_empty() {
        flush(&mut word, &mut terms);
    }
    terms
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

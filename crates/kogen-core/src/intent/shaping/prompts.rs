//! Stable shaping prompts and exact repair framing.

use crate::intent::shaping::validation::ValidationFailure;
use std::fs;
use std::path::Path;

pub(super) const SHAPER_SYSTEM: &str = "You are Kogen Intent shaper. Write and repair the Intent and its acceptance test. Use only the read, search, and write tools. Do not change files outside the two required paths.";
pub(super) const REQUIREMENT_AUDITOR_SYSTEM: &str = "You are Kogen's requirement auditor. Map every atomic Request constraint to an Acceptance item or an untestable reason. Reply with JSON only.";
pub(super) const TEST_AUDITOR_SYSTEM: &str = "You are Kogen's acceptance test auditor. Check that each acceptance test follows the verbatim Request. Reply with JSON only.";

pub(super) fn first_message(
    slug: &str,
    domains: &[String],
    gate_paths: &[String],
    request: &[u8],
    intent_path: &str,
    acceptance_path: &str,
) -> String {
    let domains = domains.join(", ");
    let gates = gate_paths
        .iter()
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut prompt = format!(
        "Slug: {slug}\n\nConfigured project domains: {domains}. Use only these names in the Intent and Verify lines.\n\nEffective gate paths: {gates}. Set `changes_gate: true` only when the task or planned changes require modifying one of these paths. Omit it for unrelated changes; running or inspecting checks alone does not count.\n\nTask statement:\n"
    );
    prompt.push_str(&String::from_utf8_lossy(request));
    prompt.push_str(&format!(
        "\n\nWrite the Intent to `{intent_path}` and its acceptance test to `{acceptance_path}`."
    ));
    prompt
}

pub(super) fn fallback_message(first: &str, failure: &ValidationFailure) -> String {
    format!(
        "{first}\n\nLast validation failure:\n\ncandidate/{}: {}",
        failure.reason, failure.detail
    )
}

pub(super) fn repair_message(
    intent_path: &Path,
    acceptance_path: &Path,
    failure: &ValidationFailure,
) -> String {
    format!(
        "Validation failed. Repair the generated files in this conversation. The required paths and their current state are:\n{}\n{}\nBoth exact paths must exist after this pass. Every missing path must be written now. Do not delete required files. The available tools can read, search, and write files; they cannot remove them. Preserve present content unless the failure below requires a focused correction.\n\nExact failure output:\n\ncandidate/{}: {}",
        path_state(intent_path),
        path_state(acceptance_path),
        failure.reason,
        failure.detail
    )
}

pub(super) fn missing_guard(intent_path: &Path, acceptance_path: &Path) -> String {
    let mut missing = Vec::new();
    for path in [intent_path, acceptance_path] {
        if fs::read(path).is_err() {
            missing.push(path.display().to_string());
        }
    }
    format!(
        "Both files must exist before you finish. Missing: {}.",
        missing.join(", ")
    )
}

pub(super) fn style_message(findings: &[String]) -> String {
    format!(
        "Repair these style findings without changing the Request or weakening the Acceptance items:\n{}",
        findings
            .iter()
            .map(|finding| format!("- {finding}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub(super) fn requirement_message(request: &[u8]) -> String {
    format!(
        "Extract every atomic constraint from this Request and map each to an Acceptance item id, or use `untestable: <reason>`. Return {{\"rows\":[{{\"constraint\",\"maps_to\"}}]}} only.\n\nRequest:\n{}",
        String::from_utf8_lossy(request)
    )
}

pub(super) fn test_audit_message(
    request: &[u8],
    intent: &[u8],
    test: &[u8],
    base_outputs: &str,
) -> String {
    format!(
        "Audit each acceptance item against the Request. Reply with {{\"items\":[{{\"id\",\"verdict\":\"valid|over_strict|infeasible\",\"citation\",\"reason\"}}]}} only. A citation must be an exact substring of the Request.\n\nRequest:\n{}\n\nIntent:\n{}\n\nAcceptance test:\n{}\n\nBase output for each item:\n{}",
        String::from_utf8_lossy(request),
        String::from_utf8_lossy(intent),
        String::from_utf8_lossy(test),
        base_outputs
    )
}

fn path_state(path: &Path) -> String {
    let status = if fs::read(path).is_ok() {
        "present on disk. Keep it in place; change it only if the failure below requires a correction."
    } else {
        "missing or unreadable. Write it during this repair pass at this exact path."
    };
    format!("- `{}`: {status}", path.display())
}

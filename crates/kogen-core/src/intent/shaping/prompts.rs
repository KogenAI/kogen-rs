//! Stable shaping prompts and exact repair framing.

use crate::intent::shaping::validation::ValidationFailure;
use std::fs;
use std::path::Path;

pub(super) const SHAPER_SYSTEM: &str = r#"You are Kogen Intent shaper. Read the project and task, then shape a short, actionable Intent and its acceptance test. Do not implement the task.

Write exactly the two paths supplied by the user. Use only the read, search, and write tools. Do not change any other path.

Kogen configuration, acceptance sources, and effective gate files are protected. Do not edit them unless the Intent declares `changes_gate: true` and the task truly requires the edit. Keep protected paths out of the change list unless that change is required.

Use this Intent structure. Replace every placeholder with real content. Do not write a `## Brief` or `## Request` heading; Kogen appends the original Request verbatim after shaping.

```markdown
---
title: <plain title, at most 72 characters>
size: <small|medium|large>
domains: [<one or more configured domain names>]
---
<one concise prose paragraph describing the problem, scope, and behavior to preserve>

## Acceptance
- A1: <one observable, testable outcome in at most 25 words>
- A2: <one observable, testable outcome in at most 25 words>

## Verify
- A1: test domain=<configured-domain>
- A2: test keep domain=<configured-domain>

## Notes
Approach: <name the relevant code path and implementation mechanism, then state important behavior or constraints to preserve>
```

Intent format rules:
- The opening and closing `---` lines enclose a YAML map. Include all three required keys: `title` (string), `size` (`small`, `medium`, or `large`), and `domains` (a list of strings). The title belongs in frontmatter; a body heading does not replace it. Never leave the map empty or omit a required key. The only other allowed keys are `changes_gate` (boolean), `limits` (list of strings), `blocks_on` (list of slugs), `priority` (integer), `assumptions` and `shared_contracts` (lists of `{name, path, contains}` maps), and `source` (string). Include optional keys only when needed.
- Set `changes_gate: true` only when the task or planned changes require modifying an effective gate path listed in the task context. Otherwise omit it. Running or inspecting checks alone does not count.
- Keep the Brief as prose without a heading, list, or code block. Use only configured project domain names.
- Use the section headings `## Acceptance`, `## Verify`, and `## Notes`, at most once each. Acceptance entries use sequential ids (`- A1: ...`); reuse each id exactly once in Verify and in its acceptance-test tag.
- Each Acceptance item states one definite, observable result and has at most 25 words. Give every item one Verify line using `test` or `test keep`; optional modifiers are `integration`, `domain=<name>`, and `after=<id>`. A `test keep` item must already pass on the unchanged checkout. At least one item must use `test`.
- Notes must start with `Approach:` and name a code path, an implementation mechanism, and behavior to preserve. Keep the Brief, Acceptance, and Notes within the limits for the declared size: small allows 1 Brief paragraph, 90 Brief words, 3 Acceptance items, and 250 Notes words; medium allows 2 paragraphs, 200 Brief words, 6 items, and 400 Notes words; large allows 3 paragraphs, 330 Brief words, 10 items, and 600 Notes words. Every Brief or Acceptance sentence has at most 30 words.
- Write a complete acceptance test to the exact path supplied by the user. Add one test tagged for each Acceptance id. For ExUnit, use exactly `@tag intent: "<slug>/A<n>"` before each test, replacing `<slug>` with the supplied slug and `<n>` with the item number. `@tag acceptance: "A1"` does not write an Intent ledger row.
- ExUnit tests must compile and load on the unchanged checkout so each tagged test executes and records its own base result. Missing feature behavior must fail at runtime. For new modules, functions, or structs, use runtime lookup, `Code.ensure_loaded?`, `function_exported?`, `apply/3`, or `struct/2` inside the test as needed; avoid compile-time imports, macros, and struct expansion that depend on the feature. A module compile failure is a validation failure, not evidence that all items are red. Existing behavior marked `test keep` must still execute and pass.
- Do not finish by only describing the files: write both required files. If validation asks for repair, preserve valid content and correct the reported failure."#;
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
    if acceptance_path.ends_with("_test.exs") {
        prompt.push_str(&format!(
            "\n\nExUnit ledger tags: use `@tag intent: \"{slug}/A1\"` for A1, `@tag intent: \"{slug}/A2\"` for A2, and the corresponding full slug/item tag for every other item."
        ));
    }
    prompt
}

pub(super) fn fallback_message(first: &str, validation_feedback: &str) -> String {
    format!("{first}\n\nLast validation failure:\n\n{validation_feedback}")
}

pub(super) fn validation_feedback(failure: &ValidationFailure) -> String {
    format!("candidate/{}: {}", failure.reason, failure.detail)
}

pub(super) fn repair_message(
    intent_path: &Path,
    acceptance_path: &Path,
    validation_feedback: &str,
) -> String {
    format!(
        "Validation failed. Repair the generated files in this conversation. The required paths and their current state are:\n{}\n{}\nBoth exact paths must exist after this pass. Every missing path must be written now. Do not delete required files. The available tools can read, search, and write files; they cannot remove them. Preserve present content unless the failure below requires a focused correction.\n\nExact failure output:\n\n{}",
        path_state(intent_path),
        path_state(acceptance_path),
        validation_feedback
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

#[cfg(test)]
mod tests {
    use super::{SHAPER_SYSTEM, fallback_message, repair_message, validation_feedback};
    use crate::intent::shaping::validation::ValidationFailure;
    use std::path::Path;

    #[test]
    fn shaper_instructions_show_required_yaml_frontmatter_and_intent_sections() {
        for required in ["title:", "size:", "domains:", "## Acceptance", "## Verify"] {
            assert!(SHAPER_SYSTEM.contains(required), "missing {required:?}");
        }
        assert!(SHAPER_SYSTEM.contains("YAML map"));
        assert!(SHAPER_SYSTEM.contains("Never leave the map empty or omit a required key."));
        assert!(SHAPER_SYSTEM.contains("Use this Intent structure."));
    }

    #[test]
    fn exunit_instructions_require_executed_intent_tags_and_runtime_feature_lookup() {
        assert!(SHAPER_SYSTEM.contains("@tag intent: \"<slug>/A<n>\""));
        assert!(SHAPER_SYSTEM.contains("compile and load on the unchanged checkout"));
        assert!(SHAPER_SYSTEM.contains("apply/3"));
        assert!(SHAPER_SYSTEM.contains("module compile failure is a validation failure"));
        for slug in [
            "syn-06-migration-ticket-numbers",
            "syn-20-email-invite-flow",
        ] {
            let message = super::first_message(
                slug,
                &[],
                &[],
                b"request",
                "intent.md",
                "acceptance_test.exs",
            );
            assert!(message.contains(&format!("@tag intent: \"{slug}/A1\"")));
            assert!(message.contains(&format!("@tag intent: \"{slug}/A2\"")));
        }
    }

    #[test]
    fn repair_prompts_reuse_the_exact_validation_feedback() {
        let failure = ValidationFailure {
            reason: "intent_parse_failed",
            detail: "line 2: frontmatter is missing required key `size`".to_owned(),
        };
        let feedback = validation_feedback(&failure);
        let repair = repair_message(
            Path::new("intent.md"),
            Path::new("acceptance_test.exs"),
            &feedback,
        );
        let fallback = fallback_message("initial task", &feedback);

        assert_eq!(
            feedback,
            "candidate/intent_parse_failed: line 2: frontmatter is missing required key `size`"
        );
        assert!(repair.ends_with(&format!("Exact failure output:\n\n{feedback}")));
        assert!(fallback.ends_with(&format!("Last validation failure:\n\n{feedback}")));
    }

    #[test]
    fn r10_repeated_gate_feedback_is_repaired_with_path_and_action() {
        const TRANSCRIPT: &str = include_str!("testdata/syn-20-r10-undeclared-gate-feedback.jsonl");

        let events = TRANSCRIPT
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 5);
        assert_eq!(
            events
                .iter()
                .map(|event| event["pass_index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [2, 3, 4, 5, 6]
        );
        let original_feedback = events[0]["feedback"]
            .as_str()
            .expect("r10 feedback is text");
        assert!(
            events
                .iter()
                .all(|event| event["feedback"] == original_feedback)
        );
        let matched_path = original_feedback
            .split("matched path ")
            .nth(1)
            .and_then(|path| path.strip_suffix('.'))
            .expect("r10 feedback names its matched path");
        assert_eq!(matched_path, ".kogen/project.yaml");

        let failure = ValidationFailure {
            reason: "undeclared_gate_path",
            detail: super::super::validation::undeclared_gate_path_detail(matched_path),
        };
        let feedback = validation_feedback(&failure);
        let remove_instruction = format!("Remove `{matched_path}` from the change list");
        for required in [
            "Kogen configuration, acceptance sources, and effective gate files",
            "unless the Intent declares `changes_gate: true` and the change is required",
            "declare `changes_gate: true` if changing it is truly required",
        ] {
            assert!(
                feedback.contains(required),
                "missing {required:?} in {feedback:?}"
            );
        }
        assert!(feedback.contains(matched_path));
        assert!(feedback.contains(&remove_instruction));
    }

    #[test]
    fn shaper_system_protects_configuration_acceptance_and_gate_paths() {
        for required in [
            "Kogen configuration, acceptance sources, and effective gate files are protected",
            "Do not edit them unless the Intent declares `changes_gate: true` and the task truly requires the edit",
            "Keep protected paths out of the change list",
        ] {
            assert!(SHAPER_SYSTEM.contains(required), "missing {required:?}");
        }

        let message = super::first_message(
            "syn-20-email-invite-flow",
            &[],
            &[".kogen/project.yaml".to_owned()],
            b"request",
            ".kogen/intents/syn-20-email-invite-flow/intent.md",
            ".kogen/acceptance/syn-20-email-invite-flow_test.exs",
        );
        assert!(message.contains("Effective gate paths: `.kogen/project.yaml`."));
    }
}

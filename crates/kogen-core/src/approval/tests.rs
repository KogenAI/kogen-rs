use super::render::render_warning_prefix;
use super::support::approval_style_warnings;
use crate::intent::Intent;

#[test]
fn approval_card_includes_remaining_intent_style_warnings() {
    let title = "x".repeat(73);
    let acceptance = "Change the greeting in lib/greet.txt so that it names Almir and preserves the same punctuation and single line layout, with no added trailing spaces or extra lines in the output";
    let brief = "Change the greeting in lib/greet.txt so that it names Almir while preserving every existing punctuation mark and the single line layout, with no extra output lines or trailing spaces in the final result so the existing project output remains stable";
    let mut source = format!(
        "---\ntitle: {title}\nsize: small\ndomains: [app]\n---\n{brief}.\n\n## Acceptance\n- A1: {acceptance}\n\n## Verify\n- A1: test\n\n## Notes\nApproach: replace the line in lib/greet.txt.\n\n```\n"
    );
    for line in 0..16 {
        source.push_str(&format!("line {line}\n"));
    }
    source.push_str("```\n");

    let intent = Intent::parse("greet", source.as_bytes()).expect("valid Intent");
    let warnings = approval_style_warnings(&intent);
    let rendered = render_warning_prefix(false, &[], &warnings);

    for expected in [
        "lint_title_too_long: - — title must be at most 72 characters",
        "lint_item_too_long: A1 — A1 exceeds 25 words",
        "lint_sentence_too_long: - — Brief has a sentence over 30 words",
        "lint_long_code_block: - — Notes code blocks must contain at most 15 lines",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in {rendered:?}"
        );
    }
}

use super::{Intent, approval_sha256, intent_sha256};

const GOOD: &str = "---\ntitle: Greet Almir by name\nsize: small\ndomains: [app]\n---\nChange the greeting in lib/greet.txt so it names Almir.\n\n## Acceptance\n- A1: lib/greet.txt contains the line Hello, Almir!\n\n## Verify\n- A1: test\n";

#[test]
fn request_is_preserved_and_excluded_from_structural_lint() {
    let bytes = format!("{GOOD}\n## Request\nPlease ensure the robust greeting stays raw.\r\nIn order to keep CRLF.\r\n").into_bytes();
    let intent = Intent::parse("greet", &bytes).expect("valid intent");
    assert_eq!(
        intent.request.as_deref(),
        Some("Please ensure the robust greeting stays raw.\r\nIn order to keep CRLF.\r\n")
    );
    assert_eq!(intent.raw_bytes(), bytes);
    assert!(intent.lint().is_empty());
}

#[test]
fn exact_approval_input_binds_nul_and_both_raw_sources() {
    let intent = format!("{GOOD}\n## Request\nraw\r\n").into_bytes();
    let acceptance = b"test A1\r\n";
    let approval = approval_sha256(&intent, acceptance);
    assert_eq!(approval, approval_sha256(&intent, acceptance));
    assert_ne!(approval, approval_sha256(&intent, b"test A1\n"));
    assert_ne!(approval, approval_sha256(b"different intent", acceptance));
    assert_ne!(intent_sha256(&intent), approval);
}

#[test]
fn verify_keep_is_structural_error_when_no_change_item_exists() {
    let source = GOOD.replace("- A1: test\n", "- A1: test keep\n");
    let intent = Intent::parse("greet", source.as_bytes()).expect("valid intent");
    let errors = intent.lint();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].rule, "no_change_item");
    assert_eq!(
        errors[0].message,
        "at least one Acceptance item must be a change item (test)"
    );
}

#[test]
fn parse_reports_first_structural_line_and_message() {
    let cases = [
        ("title: x\n", 1, "frontmatter must start with `---`"),
        (
            "---\ntitle: x\n",
            3,
            "frontmatter is missing its closing `---`",
        ),
        ("---\n- a\n---\n", 2, "frontmatter must be a YAML map"),
        (
            "---\ntitle: x\nsize: small\ndomains: [app]\nowner: me\n---\n",
            5,
            "unknown frontmatter key \"owner\"",
        ),
    ];
    for (source, line, message) in cases {
        let error = Intent::parse("greet", source.as_bytes()).expect_err("invalid intent");
        assert_eq!(error.0.line, line, "{source:?}");
        assert!(error.0.message.contains(message), "{}", error.0.message);
    }
}

#[test]
fn lint_error_kernel_matches_each_structural_rule() {
    let cases = [
        (
            "missing_brief",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "",
                "- A1: item\n",
                "- A1: test\n",
            ),
        ),
        (
            "list_in_brief",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting.\n\n- name Almir\n",
                "- A1: item\n",
                "- A1: test\n",
            ),
        ),
        (
            "heading_in_brief",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting.\n\n## Background\nMore context.\n",
                "- A1: item\n",
                "- A1: test\n",
            ),
        ),
        (
            "code_block_in_brief",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting.\n\n```\nhello\n```\n",
                "- A1: item\n",
                "- A1: test\n",
            ),
        ),
        (
            "unknown_size",
            make_doc(
                "Intent",
                "huge",
                "[app]",
                "Change the greeting.\n",
                "- A1: item\n",
                "- A1: test\n",
            ),
        ),
        (
            "acceptance_count",
            make_doc("Intent", "small", "[app]", "Change the greeting.\n", "", ""),
        ),
        (
            "missing_title",
            make_doc(
                "",
                "small",
                "[app]",
                "Change the greeting.\n",
                "- A1: item\n",
                "- A1: test\n",
            ),
        ),
        (
            "domain_count",
            make_doc(
                "Intent",
                "small",
                "[]",
                "Change the greeting.\n",
                "- A1: item\n",
                "- A1: test\n",
            ),
        ),
        (
            "duplicate_id",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting.\n",
                "- A1: item\n- A1: other\n",
                "- A1: test\n",
            ),
        ),
        (
            "sequential_ids",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting.\n",
                "- A1: item\n- A3: other\n",
                "- A1: test\n- A3: test\n",
            ),
        ),
        (
            "missing_verify",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting.\n",
                "- A1: item\n- A2: other\n",
                "- A1: test\n",
            ),
        ),
        (
            "invalid_verify",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting.\n",
                "- A1: item\n",
                "- A1: manual\n",
            ),
        ),
        (
            "no_change_item",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting.\n",
                "- A1: item\n",
                "- A1: test keep\n",
            ),
        ),
        (
            "open_question",
            make_doc(
                "Intent",
                "small",
                "[app]",
                "Change the greeting. TBD punctuation.\n",
                "- A1: item\n",
                "- A1: test\n",
            ),
        ),
    ];
    for (expected_rule, source) in cases {
        let intent =
            Intent::parse("greet", source.as_bytes()).expect("parse structurally valid intent");
        let errors = intent.lint();
        assert_eq!(
            errors.first().map(|error| error.rule),
            Some(expected_rule),
            "{source}"
        );
    }
}

#[test]
fn verify_modifiers_keep_raw_domain_precedence_and_after_tags() {
    let source = make_doc(
        "Intent",
        "small",
        "[app]",
        "Change the greeting.\n",
        "- A1: item\n",
        "- A1: test integration domain=old after=A2 domain=app\n",
    );
    let intent = Intent::parse("greet", source.as_bytes()).expect("valid verify modifiers");
    let verify = intent.verify_for("A1").expect("Verify A1");
    assert!(verify.is_integration());
    assert_eq!(verify.domain(), Some("app"));
    assert_eq!(verify.after_ids().collect::<Vec<_>>(), vec!["A2"]);
    assert!(intent.lint().is_empty());

    let invalid = source.replace(
        "integration domain=old after=A2 domain=app",
        "integration keep",
    );
    let lint = Intent::parse("greet", invalid.as_bytes())
        .expect("parse invalid Verify kind")
        .lint();
    assert_eq!(lint[0].rule, "invalid_verify");
}

fn make_doc(
    title: &str,
    size: &str,
    domains: &str,
    brief: &str,
    acceptance: &str,
    verify: &str,
) -> String {
    format!(
        "---\ntitle: \"{title}\"\nsize: {size}\ndomains: {domains}\n---\n{brief}\n## Acceptance\n{acceptance}\n## Verify\n{verify}"
    )
}

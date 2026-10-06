use super::parse;

fn first_error(source: &[u8]) -> (Option<usize>, String) {
    let error = parse(source).expect_err("invalid YAML must be rejected");
    (error.line, error.message)
}

#[test]
fn yaml_error_corpus_first_line_and_class() {
    let mut too_large = b"name: kt\nchecks: []\n".to_vec();
    too_large.resize(1_048_577, b'x');
    let deep = format!(
        "name: kt\nchecks: []\nx: {}{}\n",
        "[".repeat(65),
        "]".repeat(65)
    );
    let cases: Vec<(Vec<u8>, Option<usize>, &str)> = vec![
        (too_large, None, "document exceeds the maximum size"),
        (
            b"\xef\xbb\xbfname: kt\nchecks: []\n".to_vec(),
            Some(1),
            "leading UTF-8 BOM",
        ),
        (
            b"name: kt\nchecks: []\nbase: \xff\n".to_vec(),
            Some(3),
            "document is not valid UTF-8",
        ),
        (
            b"name: kt\nchecks:\n\t- name: x\n".to_vec(),
            Some(3),
            "tab character",
        ),
        (
            b"%YAML 1.2\nname: kt\n".to_vec(),
            Some(1),
            "directives and document markers",
        ),
        (
            b"name: kt\nchecks: []\nbase: |\n  main\n".to_vec(),
            Some(3),
            "block scalars are not allowed",
        ),
        (
            b"name: kt\nchecks: []\nbase: &b main\n".to_vec(),
            Some(3),
            "anchors, aliases",
        ),
        (deep.into_bytes(), Some(3), "maximum nesting depth of 64"),
        (
            b"name: kt\nchecks: []\n<<: {a: b}\n".to_vec(),
            Some(3),
            "YAML merge key",
        ),
        (
            b"name: \"k\\u0074\"\nchecks: []\n".to_vec(),
            Some(1),
            "Unicode escape \\u",
        ),
        (
            b"name: \"k\\x\"\nchecks: []\n".to_vec(),
            Some(1),
            "unsupported escape \\x",
        ),
        (
            b"name: kt\nchecks: []\nbase: \"main\n".to_vec(),
            Some(3),
            "unterminated quoted string",
        ),
        (
            b"name: kt\nchecks: []\nbase: \"main\" x\n".to_vec(),
            Some(3),
            "text after closing quote",
        ),
        (
            b"name: kt\nchecks: []\nprotected_paths: [a[1]]\n".to_vec(),
            Some(3),
            "brackets inside a flow collection",
        ),
        (
            b"name: kt\nchecks: []\nprotected_paths: [a,, b]\n".to_vec(),
            Some(3),
            "malformed flow collection",
        ),
        (
            b"name: kt\nchecks: []\nprotected_paths: [a, b\n".to_vec(),
            Some(3),
            "unterminated flow collection",
        ),
        (
            b"name: kt\nchecks: []\nprotected_paths: [a] b\n".to_vec(),
            Some(3),
            "trailing text after flow collection",
        ),
        (
            b"name: kt\nchecks: []\nbase: a: b\n".to_vec(),
            Some(3),
            "unquoted `: ` inside a value",
        ),
        (
            b"name: kt\nchecks: []\nbase: - main\n".to_vec(),
            Some(3),
            "list item in a value position",
        ),
        (
            b"name: kt\n  checks: []\n".to_vec(),
            Some(2),
            "unexpected indentation",
        ),
        (
            b"name: kt\nchecks: []\nname: duplicate\n".to_vec(),
            Some(3),
            "duplicate key \"name\"",
        ),
        (
            b"name: kt\nchecks: []\nbase:\n".to_vec(),
            Some(3),
            "mapping key has no value",
        ),
        (
            b"name: kt\nchecks: []\npaths:\n  -\n".to_vec(),
            Some(4),
            "list item has no value",
        ),
        (Vec::new(), None, "empty document"),
    ];
    for (source, expected_line, expected_class) in cases {
        let (line, message) = first_error(&source);
        assert_eq!(
            line,
            expected_line,
            "source={:?}; error={message}",
            String::from_utf8_lossy(&source)
        );
        assert!(
            message.contains(expected_class),
            "unexpected YAML error class: {message}"
        );
    }
}

#[test]
fn strict_subset_rejects_indentless_sequences_and_flow_duplicates() {
    let indentless = parse(b"name: kt\nchecks:\n- name: x\n  argv: [sh, x]\n  timeout_ms: 1\n")
        .expect_err("indentless sequences are outside the supported YAML subset");
    assert_eq!(indentless.line, Some(3));
    assert!(indentless.message.contains("unexpected indentation"));

    let duplicate = parse(b"name: kt\nchecks: []\nx: {a: one, a: two}\n")
        .expect_err("duplicate flow-map keys must be rejected");
    assert_eq!(duplicate.line, Some(3));
    assert!(duplicate.message.contains("duplicate key \"a\""));
}

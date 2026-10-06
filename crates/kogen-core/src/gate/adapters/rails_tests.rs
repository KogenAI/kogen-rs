use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("kogen-rails-{}-{nonce}", std::process::id()))
}

#[test]
fn rails_detection_requires_both_files_and_respects_explicit_adapter() {
    let root = temporary_directory();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("Gemfile"), "").unwrap();
    assert!(!detected(&root));
    assert!(!selected(&root, None));
    assert!(!selected(&root, Some("command")));
    assert!(selected(&root, Some("rails")));
    std::fs::write(root.join("config/application.rb"), "").unwrap();
    assert!(detected(&root));
    assert!(selected(&root, None));
    assert!(!selected(&root, Some("exunit")));
}

#[test]
fn rails_paths_commands_formatting_and_gate_files_match_the_adapter_contract() {
    assert_eq!(
        source_path("greet"),
        PathBuf::from(".kogen/acceptance/greet_test.rb")
    );
    assert_eq!(
        candidate_path("greet"),
        PathBuf::from("test/acceptance/greet_test.rb")
    );
    assert_eq!(
        runner_command(),
        words(["bundle", "exec", "rails", "test", "{path}"])
    );
    assert_eq!(acceptance_check(), words(["ruby", "-c", "{path}"]));
    assert_eq!(
        formatter("gem 'standard', group: :development"),
        Some(words(["bundle", "exec", "standardrb", "-a"]))
    );
    assert_eq!(
        formatter("gem(\"rubocop\", require: false)"),
        Some(words(["bundle", "exec", "rubocop", "-a"]))
    );
    assert_eq!(
        formatter("# gem 'standard'\nsource 'https://example.invalid'"),
        None
    );
    assert_eq!(setup_seeds(), &["vendor/cache"]);
    assert_eq!(setup_command(), words(["bundle", "install", "--local"]));
    assert!(gate_files().contains(&"bin/rails"));
}

#[test]
fn rails_environment_and_minitest_findings_are_stable() {
    let environment = child_environment(Path::new("vendor/cache"));
    assert_eq!(
        environment.get(&OsString::from("BUNDLE_PATH")),
        Some(&OsString::from("vendor/cache"))
    );
    assert_eq!(
        environment.get(&OsString::from("RAILS_ENV")),
        Some(&OsString::from("test"))
    );
    let findings = parse_findings(
        b"  1) Failure:\nHelloTest#test_legacy [test/hello_test.rb:12]:\nExpected value\n",
        Path::new("/tmp/work"),
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].path, "test/hello_test.rb");
    assert_eq!(findings[0].symbol, "HelloTest#test_legacy");
    assert_eq!(findings[0].line, Some(12));
}

#[test]
fn rails_lint_output_keeps_tool_rule_identity() {
    let findings = parse_findings_for_tool(
        b"app/models/hello.rb:4:7: C: [Correctable] Style/FrozenStringLiteralComment: Missing frozen string literal comment.\n",
        "standard",
        Path::new("/tmp/work"),
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].rule,
        "standard/Style/FrozenStringLiteralComment"
    );
    assert_eq!(findings[0].symbol, "");
}

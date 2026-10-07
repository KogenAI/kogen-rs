use super::{ProjectConfig, ProjectOptions, ProjectResolution, state_key, valid_slug};
use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use std::path::PathBuf;

#[test]
fn slug_contract_is_exact() {
    assert!(valid_slug("greet-user"));
    assert!(valid_slug(&format!("a{}a", "b".repeat(46))));
    for slug in [
        "ab",
        "Greet",
        "my_slug",
        "a--b",
        "-abc",
        "abc-",
        "a".repeat(49).as_str(),
    ] {
        assert!(!valid_slug(slug), "accepted invalid slug {slug:?}");
    }
}

#[test]
fn project_schema_accepts_documented_and_frozen_config_keys() {
    let source = br#"
name: kt
base: main
checks: []
acceptance_checks: []
setup: []
setup_outputs: [build]
setup_inputs: [mix.exs]
fix: []
format: [mix, format]
protected_paths: [Makefile]
gate_paths: [checks/*.sh]
domains: {app: [lib], docs: [docs]}
env: {MODE: test}
sandbox: true
acceptance: {adapter: command, ext: .t.sh, candidate_dir: test/acceptance, run: [sh, run.sh, "{path}"], timeout_ms: 600000}
shaping: {proof: none}
build:
  recipe: ladder
  roles:
    builder: {model: gpt-6-luna, effort: max}
    fallback_shaper: {model: gpt-6.1-sol, effort: high}
    rung2: {model: gpt-6.1-sol, effort: medium}
    rung3: {model: gpt-6.1-sol, effort: high}
  ladder: {max_rungs: 3, experimental_r4: false}
  land: green-or-advisory
  budget_ms: 3600000
  fallback: {gpt-6-luna: {model: gpt-6.1-sol, effort: medium}}
account: default
"#;
    let config = ProjectConfig::from_bytes("project.yaml", source).expect("valid project config");
    assert_eq!(config.name, "kt");
    assert_eq!(config.base.as_deref(), Some("main"));
}

#[test]
fn project_schema_collects_independent_errors() {
    let source = br#"
name: kt
frobnicate: yes
checks:
  - name: lint
    argv: [sh, checks/lint.sh]
    timeout_ms: 60000
    cmd: lint
setup:
  - name: s
    argv: [sh, setup.sh]
    timeout_ms: 1000
  - name: s
    argv: [sh, setup.sh]
    timeout_ms: 1000
fix:
  - name: fmt
    argv: [sh, fmt.sh]
    timeout_ms: soon
build:
  roles:
    judge: {model: model, effort: high}
"#;
    let error = ProjectConfig::from_bytes("project.yaml", source).expect_err("invalid config");
    let details = error
        .issues
        .iter()
        .map(|issue| issue.detail.as_str())
        .collect::<Vec<_>>();
    assert!(details.contains(&"project has unknown key \"frobnicate\""));
    assert!(details.contains(&"checks[1] has unknown key \"cmd\""));
    assert!(details.contains(&"setup has duplicate name \"s\""));
    assert!(details.contains(&"fix[1].timeout_ms must be a positive integer"));
    assert!(details.contains(&"build.roles has unknown role \"judge\""));
}

#[test]
fn config_error_details_are_indented_once_at_the_cli_boundary() {
    let error =
        ProjectConfig::from_bytes(".kogen/project.yaml", b"\xef\xbb\xbfname: kt\nchecks: []\n")
            .expect_err("BOM is rejected");
    let output = CoreError::new(
        ErrorClass::Environment,
        "project_config_invalid",
        error.to_string(),
        ExitCode::Environment,
    )
    .render_stdout();
    assert_eq!(
        output,
        "environment/project_config_invalid: .kogen/project.yaml\n  line 1: leading UTF-8 BOM is not allowed\n"
    );
}

#[test]
fn state_key_uses_sanitized_basename_and_path_digest() {
    let path = std::path::Path::new("/tmp/link to checkout");
    let key = state_key(path);
    assert!(key.starts_with("link-to-checkout-"));
    assert_eq!(key.len(), "link-to-checkout-".len() + 10);
    assert!(
        key.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ".-_".contains(ch))
    );
}

#[cfg(unix)]
#[test]
fn resolution_uses_checkout_root_for_relative_and_symlinked_project_paths() {
    let temp = std::env::temp_dir().join(format!(
        "kogen-project-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let checkout = temp.join("checkout");
    let home = temp.join("home");
    std::fs::create_dir_all(&checkout).expect("create checkout");
    std::fs::create_dir_all(&home).expect("create home");
    std::os::unix::fs::symlink(&checkout, temp.join("link to checkout")).expect("create symlink");
    let init = kogen_test_support::git_command()
        .args(["init", "-q", "-b", "main"])
        .current_dir(&checkout)
        .status()
        .expect("run git init");
    assert!(init.success());
    kogen_test_support::set_identity(&checkout, "Kogen Project Test", "project@example.invalid")
        .expect("configure project fixture identity");
    let resolved = ProjectResolution::resolve(&ProjectOptions {
        cwd: Some(temp.clone()),
        project: Some(PathBuf::from("link to checkout")),
        home: Some(home.clone()),
        ..ProjectOptions::default()
    })
    .expect("resolve project");
    assert_eq!(
        resolved.checkout,
        std::fs::canonicalize(&checkout).expect("canonical checkout")
    );
    assert_eq!(resolved.origin, resolved.checkout);
    assert_eq!(resolved.base, "main");
    resolved.ensure_state_root().expect("create state root");
    assert!(resolved.state_root.is_dir());
    std::fs::remove_dir_all(temp).expect("remove fixture");
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos()
}

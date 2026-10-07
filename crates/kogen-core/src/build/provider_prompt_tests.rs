use super::{
    append_context_packet, builder_instructions, context_packet_token_upper_bound,
    public_context_packet, user_item, witness_auditor_instructions, workspace_changed,
};
use crate::gate::commit_tree_id;
use crate::git::GitRepo;
use crate::provider::auth::{InjectedCredential, RequestCredential};
use crate::provider::http::{RequestContext, ResponseMode, WireConfig, build_wire_request};
use crate::provider::session::ConversationBinding;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use url::Url;

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

#[test]
fn finish_guard_ignores_acceptance_copy_and_detects_builder_commit() {
    let workspace = std::env::temp_dir().join(format!(
        "kogen-builder-progress-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(workspace.join("lib")).expect("create source directory");
    let repo = GitRepo::new(&workspace);
    repo.output(&["init", "--quiet"])
        .expect("initialize repository");
    kogen_test_support::set_identity(&workspace, "Progress Test", "progress@example.invalid")
        .expect("configure test identity");
    fs::write(workspace.join("lib/greet.txt"), b"Hello, world!\n").expect("write base source");
    repo.output(&["add", "-A"]).expect("stage base source");
    repo.output(&["commit", "-m", "base"])
        .expect("commit base source");
    let base = repo.resolve_commit("HEAD").expect("resolve base commit");
    let baseline = commit_tree_id(&workspace, &base).expect("resolve base tree");
    let excluded = [PathBuf::from("test/acceptance/greet_test.rs")];

    fs::create_dir_all(workspace.join("test/acceptance"))
        .expect("create generated acceptance directory");
    fs::write(
        workspace.join("test/acceptance/greet_test.rs"),
        b"#[test] fn acceptance() {}\n",
    )
    .expect("write generated acceptance copy");
    assert!(
        !workspace_changed(&workspace, &baseline, &excluded)
            .expect("snapshot unchanged source tree")
    );

    fs::write(workspace.join("lib/greet.txt"), b"Hello, Almir!\n").expect("write implementation");
    repo.output(&["add", "lib/greet.txt"])
        .expect("stage implementation");
    repo.output(&["commit", "-m", "builder commit"])
        .expect("commit implementation in builder session");
    assert!(
        workspace_changed(&workspace, &baseline, &excluded)
            .expect("compare committed implementation to build base")
    );

    fs::remove_dir_all(workspace).expect("remove temporary repository");
}

#[test]
fn witness_auditor_prompt_uses_the_witness_verdict_contract() {
    let prompt = witness_auditor_instructions();
    assert!(prompt.contains(crate::run::orchestration::BUILD_AUDITOR_MARKER));
    assert!(prompt.contains("TEST-WRONG|WITNESS-WRONG|UNDECIDED"));
    assert!(prompt.contains("\"citation\""));
}

#[test]
fn public_context_packet_is_deterministic_and_uses_tracked_public_lines() {
    let root = temporary_repository();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("secrets")).unwrap();
    fs::create_dir_all(root.join(".kogen/intents/demo")).unwrap();
    fs::create_dir_all(root.join("tests/acceptance")).unwrap();
    fs::write(
        root.join("src/provider.rs"),
        format!(
            "pub fn provider_prompt_cache_assembly() {{}}\n// provider prompt cache root {}\n",
            root.display()
        ),
    )
    .unwrap();
    fs::write(
        root.join("secrets/private.md"),
        "provider prompt cache LEAK_SECRET\n",
    )
    .unwrap();
    fs::write(
        root.join(".kogen/intents/demo/intent.md"),
        "provider prompt cache LEAK_INTENT\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/acceptance/hidden.rs"),
        "provider prompt cache LEAK_TEST\n",
    )
    .unwrap();
    fs::write(root.join("AGENTS.md"), "provider prompt cache LEAK_AGENT\n").unwrap();
    git(&root, &["add", "-A"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Kogen Test",
            "-c",
            "user.email=test@kogen.invalid",
            "commit",
            "-qm",
            "base",
        ],
    );
    fs::write(
        root.join("src/untracked.rs"),
        "provider prompt cache LEAK_UNTRACKED\n",
    )
    .unwrap();

    let intent = b"Add provider prompt cache assembly with a deterministic public context packet.";
    let packet = public_context_packet(&root, intent);
    assert_eq!(packet, public_context_packet(&root, intent));
    assert!(packet.contains("[src/provider.rs:L1]"));
    for private_marker in [
        "LEAK_SECRET",
        "LEAK_INTENT",
        "LEAK_TEST",
        "LEAK_AGENT",
        "LEAK_UNTRACKED",
        "secrets/",
        ".kogen/",
        "tests/",
        "AGENTS.md",
    ] {
        assert!(
            !packet.contains(private_marker),
            "packet contains {private_marker}"
        );
    }
    assert!(!packet.contains(&root.to_string_lossy().to_string()));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn public_context_packet_stays_within_its_token_upper_bound() {
    let root = temporary_repository();
    fs::create_dir_all(root.join("src")).unwrap();
    let lines = (0..200)
        .map(|index| format!("pub fn context_size_cap_line_{index:03}() {{}}\n"))
        .collect::<String>();
    fs::write(root.join("src/context.rs"), lines).unwrap();
    git(&root, &["add", "-A"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Kogen Test",
            "-c",
            "user.email=test@kogen.invalid",
            "commit",
            "-qm",
            "base",
        ],
    );

    let packet = public_context_packet(&root, b"context size cap line");
    assert!(!packet.is_empty());
    assert!(context_packet_token_upper_bound(&packet) <= 2_000);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn context_packet_is_in_user_input_after_instructions_and_tool_schemas() {
    let root = temporary_repository();
    let packet = "Context packet (provided):\n- [src/lib.rs:L1] pub fn cache_prefix() {}";
    let message = append_context_packet("Approved Intent:\nImplement cache_prefix.", packet);
    let binding = ConversationBinding::new(&root, "develop");
    let mut context = RequestContext::for_conversation(
        &binding,
        "gpt-6-luna",
        "max",
        builder_instructions(false),
        vec![user_item(&message)],
    )
    .unwrap();
    context.tools = crate::provider::tools::canonical_tool_schemas();
    let auth = RequestCredential::Injected(InjectedCredential {
        access_token: "fake-token".to_owned(),
        account_id: "fake-account".to_owned(),
        expires_at: i64::MAX,
    });
    let config = WireConfig {
        endpoint_override: Some(Url::parse("https://example.invalid/v1/responses").unwrap()),
        mode: ResponseMode::Injected,
        supports_generation_cap: false,
        user_agent_version: "test".to_owned(),
    };
    let wire = build_wire_request(&context, &auth, &config).unwrap();
    let body_text = String::from_utf8(wire.body).unwrap();
    let body: Value = serde_json::from_str(&body_text).unwrap();
    assert!(!body["instructions"].as_str().unwrap().contains(packet));
    assert!(!body["tools"].to_string().contains(packet));
    assert!(
        body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(packet)
    );
    let instructions_at = body_text.find("\"instructions\"").unwrap();
    let tools_at = body_text.find("\"tools\"").unwrap();
    let input_at = body_text.find("\"input\"").unwrap();
    let packet_at = body_text.find("Context packet (provided):").unwrap();
    assert!(instructions_at < tools_at && tools_at < input_at && input_at < packet_at);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn approved_intent_test_and_setup_outputs_are_not_implementation_changes() {
    let root = temporary_repository();
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(root.join("lib/greet.txt"), "Hello!\n").unwrap();
    git(&root, &["add", "lib/greet.txt"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Kogen Test",
            "-c",
            "user.email=test@kogen.invalid",
            "commit",
            "-qm",
            "base",
        ],
    );
    let base = crate::gate::commit_tree_id(&root, "HEAD").unwrap();

    fs::create_dir_all(root.join(".kogen/intents/greet")).unwrap();
    fs::create_dir_all(root.join(".kogen/acceptance")).unwrap();
    fs::create_dir_all(root.join("test/acceptance")).unwrap();
    fs::write(
        root.join(".kogen/intents/greet/intent.md"),
        "approved intent\n",
    )
    .unwrap();
    fs::write(root.join(".kogen/acceptance/greet.t.sh"), "approved test\n").unwrap();
    fs::write(root.join("test/acceptance/greet.t.sh"), "installed test\n").unwrap();
    fs::create_dir_all(root.join("build")).unwrap();
    fs::write(root.join("build/ready"), "setup output\n").unwrap();

    let excluded = [
        PathBuf::from(".kogen/intents/greet/intent.md"),
        PathBuf::from(".kogen/acceptance/greet.t.sh"),
        PathBuf::from("test/acceptance/greet.t.sh"),
        PathBuf::from("build"),
    ];
    assert!(!workspace_changed(&root, &base, &excluded).unwrap());

    fs::write(root.join("lib/greet.txt"), "Hello, Almir!\n").unwrap();
    assert!(workspace_changed(&root, &base, &excluded).unwrap());
    fs::remove_dir_all(root).unwrap();
}

fn temporary_repository() -> PathBuf {
    let id = NEXT_REPO.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "kogen-workspace-changed-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    root
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("git is available");
    assert!(status.success(), "git {args:?} succeeded");
}

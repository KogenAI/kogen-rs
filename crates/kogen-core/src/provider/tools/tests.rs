use super::{ToolContext, ToolError, ToolRole, canonical_tool_schemas, dispatch};
use crate::provider::ModelToolCall;
use crate::run::ChildEnvironment;
use serde_json::json;

#[test]
fn shaper_scope_and_workspace_paths_are_enforced_after_resolution() {
    let (workspace, run_dir, outside) = temp_tree();
    std::fs::create_dir_all(workspace.join(".kogen/intents/greet")).unwrap();
    std::fs::create_dir_all(workspace.join(".kogen/acceptance")).unwrap();
    std::fs::create_dir_all(workspace.join("lib")).unwrap();
    std::fs::write(workspace.join("lib/source.txt"), "safe\n").unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("lib/escape.txt")).unwrap();

    let context = ToolContext {
        role: ToolRole::Shaper,
        workspace: &workspace,
        run_dir: &run_dir,
        shaper_write_paths: [
            ".kogen/intents/greet/intent.md",
            ".kogen/acceptance/greet.t.sh",
        ],
        result_tokens: 2000,
        process: None,
        environment: ChildEnvironment::new(),
    };
    let write = call(
        "write",
        json!({
            "path":".kogen/intents/greet/intent.md",
            "content":"intent bytes",
            "timeout_ms":10
        }),
    );
    assert_eq!(
        dispatch(&context, &write, 1).unwrap(),
        "Wrote .kogen/intents/greet/intent.md."
    );
    assert_eq!(
        std::fs::read(workspace.join(".kogen/intents/greet/intent.md")).unwrap(),
        b"intent bytes"
    );

    let out_of_scope = call("write", json!({"path":"lib/source.txt","content":"bad"}));
    assert!(matches!(
        dispatch(&context, &out_of_scope, 1),
        Err(ToolError::ShaperScope(_))
    ));
    let escape = call("read", json!({"path":"lib/escape.txt"}));
    assert_eq!(dispatch(&context, &escape, 1), Err(ToolError::PathEscape));
    let parent_escape = call("read", json!({"path":"../outside.txt"}));
    assert_eq!(
        dispatch(&context, &parent_escape, 1),
        Err(ToolError::PathEscape)
    );

    std::fs::remove_dir_all(workspace.parent().unwrap()).unwrap();
}

#[test]
fn finish_is_single_call_and_tool_schemas_are_canonical() {
    let (workspace, run_dir, _outside) = temp_tree();
    let context = ToolContext {
        role: ToolRole::BuilderShell,
        workspace: &workspace,
        run_dir: &run_dir,
        shaper_write_paths: ["a", "b"],
        result_tokens: 2000,
        process: None,
        environment: ChildEnvironment::new(),
    };
    let finish = call("finish", json!({}));
    assert_eq!(
        dispatch(&context, &finish, 1).unwrap(),
        "Completion requested. Kogen will run the gate."
    );
    assert_eq!(dispatch(&context, &finish, 2), Err(ToolError::FinishGuard));
    assert_eq!(
        dispatch(
            &context,
            &call("write", json!({"path":"a","content":"x"})),
            1
        ),
        Err(ToolError::NotAllowed)
    );
    let schemas = canonical_tool_schemas();
    assert_eq!(
        schemas
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "edit",
            "finish",
            "read",
            "search",
            "shell",
            "tool_output",
            "write"
        ]
    );
    assert!(schemas[1]["strict"].as_bool().unwrap());
    std::fs::remove_dir_all(workspace.parent().unwrap()).unwrap();
}

fn call(name: &str, arguments: serde_json::Value) -> ModelToolCall {
    ModelToolCall {
        id: "call_test".to_owned(),
        name: name.to_owned(),
        arguments,
    }
}

fn temp_tree() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "kogen-tools-test-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let workspace = base.join("workspace");
    let run_dir = base.join("run");
    let outside = base.join("outside.txt");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(&outside, "private\n").unwrap();
    (workspace, run_dir, outside)
}

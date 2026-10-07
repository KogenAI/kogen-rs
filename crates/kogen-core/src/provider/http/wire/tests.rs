use super::{
    RequestContext, ResponseMode, WireConfig, build_wire_request, validate_generation_cap,
};
use crate::provider::ProviderErrorKind;
use crate::provider::auth::{InjectedCredential, RequestCredential};
use crate::provider::session::ConversationBinding;
use serde_json::{Value, json};
use url::Url;

#[test]
fn generation_cap_validation_reports_lite_and_noncanonical_endpoint_errors() {
    let injected_endpoint = Url::parse("http://127.0.0.1:8765/v1/responses").unwrap();
    let lite_error = validate_generation_cap(
        &injected_endpoint,
        ResponseMode::Lite,
        "gpt-6-luna",
        Some(32),
        false,
    )
    .unwrap_err();
    assert_eq!(lite_error.kind, ProviderErrorKind::Unsupported);
    assert_eq!(
        lite_error.message,
        "Unsupported adapter or model-generation cap for this backend."
    );

    let endpoint_error = validate_generation_cap(
        &injected_endpoint,
        ResponseMode::Injected,
        "gpt-6-luna",
        Some(32),
        false,
    )
    .unwrap_err();
    assert_eq!(endpoint_error.kind, ProviderErrorKind::Unsupported);
    assert_eq!(
        endpoint_error.message,
        "Model-generation cap is unsupported on this endpoint/adapter."
    );
}

#[test]
fn consecutive_turns_extend_body_byte_prefix_and_keep_distinct_stable_ids() {
    let root = std::env::temp_dir().join(format!(
        "kogen-wire-test-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let binding = ConversationBinding::new(&root, "develop");
    let mut context = RequestContext::for_conversation(
        &binding,
        "gpt-6-luna",
        "max",
        "instructions",
        vec![user_item("turn one")],
    )
    .unwrap();
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

    let first = build_wire_request(&context, &auth, &config).unwrap();
    context.input.push(user_item("turn two"));
    let second = build_wire_request(&context, &auth, &config).unwrap();
    context.input.push(user_item("turn three"));
    let third = build_wire_request(&context, &auth, &config).unwrap();

    assert!(first.body.ends_with(b"]}"));
    assert!(second.body.starts_with(&first.body[..first.body.len() - 2]));
    assert!(
        third
            .body
            .starts_with(&second.body[..second.body.len() - 2])
    );
    for wire in [&first, &second, &third] {
        let body = std::str::from_utf8(&wire.body).unwrap();
        assert!(body.rfind("\"input\":").unwrap() > body.rfind("\"tool_choice\":").unwrap());
        assert!(body.ends_with("]}"));
        assert!(!body.contains("previous_response_id"));
        assert_eq!(wire.header("session-id"), Some(context.cache_key.as_str()));
        assert_eq!(wire.header("thread-id"), Some(context.thread_id.as_str()));
        assert_ne!(wire.header("session-id"), wire.header("thread-id"));
    }
    let third_body: Value = serde_json::from_slice(&third.body).unwrap();
    let input = third_body["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["content"][0]["text"], "turn one");
    assert_eq!(input[2]["content"][0]["text"], "turn three");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn luna_medium_builder_turns_keep_wire_prefix_and_non_input_fields_identical() {
    let root = std::env::temp_dir().join(format!(
        "kogen-wire-luna-builder-test-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let binding = ConversationBinding::new(&root, "develop");
    let mut context = RequestContext::for_conversation(
        &binding,
        "gpt-6-luna",
        "medium",
        "builder instructions",
        vec![
            user_item("implement the task"),
            json!({
                "type":"reasoning",
                "summary":[{"type":"summary_text","text":"inspect the repository"}],
                "encrypted_content":"opaque reasoning payload"
            }),
            json!({
                "type":"function_call",
                "call_id":"call-shell-1",
                "name":"shell",
                "arguments":"{\"cmd\":\"pwd\"}"
            }),
            json!({
                "type":"function_call_output",
                "call_id":"call-shell-1",
                "output":"/workspace"
            }),
        ],
    )
    .unwrap();
    let allowed = crate::provider::tools::ToolRole::BuilderShell.allowed();
    context.callable_tools = allowed.iter().map(|name| (*name).to_owned()).collect();
    context.tools = crate::provider::tools::canonical_tool_schemas()
        .into_iter()
        .filter(|schema| {
            schema
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| allowed.contains(&name))
        })
        .collect();
    let original_input = context.input.clone();
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

    let first = build_wire_request(&context, &auth, &config).unwrap();
    context.input.push(user_item("continue after tool output"));
    let second = build_wire_request(&context, &auth, &config).unwrap();
    assert!(first.body.ends_with(b"]}"));
    assert!(second.body.starts_with(&first.body[..first.body.len() - 2]));

    let first_body: Value = serde_json::from_slice(&first.body).unwrap();
    let second_body: Value = serde_json::from_slice(&second.body).unwrap();
    assert_eq!(first_body["model"], "gpt-6-luna");
    assert_eq!(first_body["reasoning"]["effort"], "medium");
    assert!(first_body["reasoning"].get("summary").is_none());
    assert_eq!(first_body["text"], json!({"verbosity":"low"}));
    assert_eq!(first_body["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        first_body["client_metadata"]["session_id"],
        context.cache_key
    );
    assert_eq!(
        first_body["client_metadata"]["thread_id"],
        context.thread_id
    );
    assert_eq!(
        first_body["client_metadata"]["x-codex-window-id"],
        format!("{}:0", context.thread_id)
    );
    assert_eq!(
        &second_body["input"].as_array().unwrap()[..original_input.len()],
        original_input.as_slice()
    );
    assert_eq!(
        without_input(first_body.clone()),
        without_input(second_body.clone())
    );

    assert_eq!(first.header("session-id"), Some(context.cache_key.as_str()));
    assert_eq!(first.header("thread-id"), Some(context.thread_id.as_str()));
    assert_eq!(
        first.header("x-client-request-id"),
        Some(context.thread_id.as_str())
    );
    assert_eq!(
        first.header("x-codex-window-id"),
        Some(format!("{}:0", context.thread_id).as_str())
    );
    let turn_metadata: Value =
        serde_json::from_str(first.header("x-codex-turn-metadata").unwrap()).unwrap();
    assert_eq!(
        first_body["client_metadata"]["x-codex-turn-metadata"],
        first.header("x-codex-turn-metadata").unwrap()
    );
    assert_eq!(turn_metadata["session_id"], context.cache_key);
    assert_eq!(turn_metadata["thread_id"], context.thread_id);
    assert_eq!(
        turn_metadata["window_id"],
        format!("{}:0", context.thread_id)
    );
    assert_eq!(turn_metadata["request_kind"], "turn");

    std::fs::remove_dir_all(root).unwrap();
}

fn without_input(mut body: Value) -> Value {
    body.as_object_mut().unwrap().remove("input");
    body
}

fn user_item(text: &str) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":text}]})
}

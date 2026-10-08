//! Shaping model conversations over the shared provider request machinery.

use super::ShapeModelCall;
use crate::error::{CoreError, ErrorClass};
use crate::provider::auth;
use crate::provider::http::retry::RetryReplay;
use crate::provider::http::{
    ApiMode, RequestContext, RequestPolicy, ReqwestPort, SystemClock, WireConfig, respond,
};
use crate::provider::session::ConversationBinding;
use crate::provider::tools::{self, ToolContext, ToolRole};
use crate::provider::{ModelResponse, ModelToolCall, ModelUsage, RunAccount};
use crate::run::{ChildEnvironment, ProcessPort};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_TURNS: usize = 60;
const MAX_WRITE_BYTES: usize = 200_000;

pub(super) struct ShapeProvider {
    home: PathBuf,
    account: RunAccount,
    http: ReqwestPort,
    clock: SystemClock,
}

impl ShapeProvider {
    pub fn new(home: impl Into<PathBuf>, account: RunAccount) -> Result<Self, CoreError> {
        let http = ReqwestPort::new().map_err(provider_failure)?;
        Ok(Self {
            home: home.into(),
            account,
            http,
            clock: SystemClock::default(),
        })
    }

    pub fn session(&self, spec: ShapeSessionSpec<'_>) -> Result<ShapeSession, CoreError> {
        let mut binding = ConversationBinding::new(spec.run_dir, spec.stage);
        binding.attempt = spec.attempt.to_owned();
        binding.rung = spec.stage.to_owned();
        let mut context = RequestContext::for_conversation(
            &binding,
            spec.model,
            spec.effort,
            spec.instructions,
            vec![json!({
                "role": "user",
                "content": [{"type": "input_text", "text": spec.initial_user}],
            })],
        )
        .map_err(provider_io_error)?;
        configure_tools(&mut context, spec.tools);
        Ok(ShapeSession {
            output_role: spec.output_role.to_owned(),
            context,
            previous_input_items: 0,
            turns: 0,
        })
    }

    pub fn turn(&self, session: &mut ShapeSession) -> Result<ShapeTurn, Box<ShapeTurnFailure>> {
        if session.turns >= MAX_TURNS {
            return Err(Box::new(ShapeTurnFailure {
                error: CoreError::new(
                    ErrorClass::Candidate,
                    "shape_turn_limit",
                    "Shaper exhausted its turn limit.",
                    crate::ExitCode::Negative,
                ),
                http_attempts: 0,
                usages: Vec::new(),
            }));
        }
        let credential = auth::credential_for_request(&self.home, &self.account)
            .map_err(ShapeTurnFailure::without_attempts)?;
        let config = WireConfig::from_auth(&credential, ApiMode::Responses)
            .map_err(provider_failure)
            .map_err(ShapeTurnFailure::without_attempts)?;
        let mut request_credential = credential;
        let mut retry = RetryReplay::default();
        let fallback_model = session.context.model.clone();
        let fallback_effort = session.context.effort.clone();
        let start = Instant::now();
        let started_at_ms = unix_ms();
        let result = respond(
            &mut session.context,
            &mut request_credential,
            &config,
            &mut retry,
            &RequestPolicy {
                role: "planner".to_owned(),
                mode: "shape".to_owned(),
                fallback_on: false,
                fallback_model,
                fallback_effort,
                wall_budget_ms: None,
            },
            &self.http,
            &self.clock,
            Some(&self.home),
            Some(&self.account.label),
        );
        let ended_at_ms = unix_ms().max(started_at_ms);
        let call = result.map_err(|failure| {
            let http_attempts = failure.attempts.len();
            Box::new(ShapeTurnFailure {
                error: provider_failure(*failure.failure),
                http_attempts,
                usages: failure.usages,
            })
        })?;
        let http_attempts = call.attempts.len();
        let usages = call.usages;
        let wire = call.attempts.last().ok_or_else(|| {
            ShapeTurnFailure::without_attempts(CoreError::new(
                ErrorClass::Provider,
                "shape_request_unavailable",
                "Shape provider returned no request metadata.",
                crate::ExitCode::Provider,
            ))
        })?;
        let request = super::journal::request_metadata(
            wire,
            &session.context.cache_key,
            &session.context.thread_id,
            session.previous_input_items,
            started_at_ms,
            ended_at_ms,
        );
        session.previous_input_items =
            super::journal::input_item_count(&wire.body).unwrap_or_default();
        session.turns += 1;
        let response = call.response;
        session
            .context
            .input
            .extend(response.raw_items.iter().cloned());
        let call = ShapeModelCall {
            role: session.output_role.clone(),
            model: session.context.model.clone(),
            effort: session.context.effort.clone(),
            usage: response.usage.clone(),
            wall_ms: start.elapsed().as_millis() as u64,
            request,
        };
        Ok(ShapeTurn {
            response,
            call,
            http_attempts,
            usages,
        })
    }
}

pub(super) struct ShapeSessionSpec<'a> {
    pub run_dir: &'a Path,
    pub stage: &'a str,
    pub attempt: &'a str,
    pub output_role: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    pub instructions: &'a str,
    pub tools: bool,
    pub initial_user: String,
}

pub(super) struct ShapeSession {
    output_role: String,
    context: RequestContext,
    previous_input_items: usize,
    turns: usize,
}

impl ShapeSession {
    pub fn conversation_id(&self) -> &str {
        &self.context.thread_id
    }

    pub fn effective_model(&self) -> &str {
        &self.context.model
    }

    pub fn effective_effort(&self) -> &str {
        &self.context.effort
    }

    pub fn append_user(&mut self, text: impl Into<String>) {
        self.context.input.push(json!({
            "role": "user",
            "content": [{"type": "input_text", "text": text.into()}],
        }));
    }

    pub fn append_tool_results(&mut self, calls: &[ModelToolCall], outputs: &[String]) {
        for (call, output) in calls.iter().zip(outputs) {
            self.context.input.push(json!({
                "type": "function_call_output",
                "call_id": call.id,
                "output": output,
            }));
        }
    }

    pub fn turn_count(&self) -> usize {
        self.turns
    }
}

fn configure_tools(context: &mut RequestContext, enabled: bool) {
    if enabled {
        let allowed = ToolRole::Shaper.allowed();
        context.tools = tools::canonical_tool_schemas()
            .into_iter()
            .filter(|schema| {
                schema
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| allowed.contains(&name))
            })
            .collect();
        context.callable_tools = allowed.iter().map(|name| (*name).to_owned()).collect();
    } else {
        context.tool_choice = "none".to_owned();
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub(super) struct ShapeTurn {
    pub response: ModelResponse,
    pub call: ShapeModelCall,
    pub http_attempts: usize,
    pub usages: Vec<ModelUsage>,
}

pub(super) struct ShapeTurnFailure {
    pub error: CoreError,
    pub http_attempts: usize,
    pub usages: Vec<ModelUsage>,
}

impl ShapeTurnFailure {
    fn without_attempts(error: CoreError) -> Box<Self> {
        Box::new(Self {
            error,
            http_attempts: 0,
            usages: Vec::new(),
        })
    }
}

pub(super) fn dispatch_shaper_tools(
    turn: &ShapeTurn,
    workspace: &Path,
    run_dir: &Path,
    write_paths: [&str; 2],
    environment: &ChildEnvironment,
    process: &dyn ProcessPort,
    result_tokens: u64,
) -> Vec<String> {
    let context = ToolContext {
        role: ToolRole::Shaper,
        workspace,
        run_dir,
        shaper_write_paths: write_paths,
        result_tokens,
        process: Some(process),
        environment: environment.clone(),
    };
    turn.response
        .tool_calls
        .iter()
        .map(|call| {
            if call.name == "write"
                && call
                    .arguments
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| content.len() > MAX_WRITE_BYTES)
            {
                return format!("ERROR: Write content exceeds {MAX_WRITE_BYTES} bytes.");
            }
            tools::dispatch(&context, call, turn.response.tool_calls.len())
                .unwrap_or_else(|error| error.render())
        })
        .collect()
}

fn provider_failure(failure: crate::provider::ProviderFailure) -> CoreError {
    CoreError::new(
        ErrorClass::Provider,
        failure.kind.as_str(),
        failure.message,
        crate::ExitCode::Provider,
    )
}

fn provider_io_error(error: std::io::Error) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        "shape_scratch_unavailable",
        error.to_string(),
        crate::ExitCode::Environment,
    )
}

#[cfg(test)]
mod tests {
    use super::{ShapeProvider, ShapeSessionSpec};
    use crate::provider::ModelToolCall;
    use crate::provider::RunAccount;
    use crate::provider::auth::{InjectedCredential, RequestCredential};
    use crate::provider::http::{ResponseMode, WireConfig, build_wire_request};
    use serde_json::json;
    use std::fs;
    use url::Url;

    #[test]
    fn shaper_and_fallback_turns_append_with_stable_affinity_and_thread_ids() {
        let root = std::env::temp_dir().join(format!(
            "kogen-shape-session-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let run_dir = root.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let provider = ShapeProvider::new(
            &root,
            RunAccount {
                provider: "chatgpt".to_owned(),
                label: "test".to_owned(),
                credential_source: "test",
            },
        )
        .unwrap();
        let auth = RequestCredential::Injected(InjectedCredential {
            access_token: "fake-token-do-not-journal".to_owned(),
            account_id: "fake-account-do-not-journal".to_owned(),
            expires_at: i64::MAX,
        });
        let config = WireConfig {
            endpoint_override: Some(Url::parse("https://example.invalid/v1/responses").unwrap()),
            mode: ResponseMode::Injected,
            supports_generation_cap: false,
            user_agent_version: "test".to_owned(),
        };
        let mut shaper = provider
            .session(ShapeSessionSpec {
                run_dir: &run_dir,
                stage: "shaper",
                attempt: "pass-1",
                output_role: "shaper",
                model: "gpt-6-luna",
                effort: "max",
                instructions: "stable shaper instructions",
                tools: true,
                initial_user: "stable first request".to_owned(),
            })
            .unwrap();
        let first = build_wire_request(&shaper.context, &auth, &config).unwrap();
        let shaper_cache_key = shaper.context.cache_key.clone();
        let shaper_thread_id = shaper.context.thread_id.clone();

        shaper.context.input.extend([
            json!({"type":"reasoning","summary":[{"type":"summary_text","text":"inspect"}]}),
            json!({"type":"function_call","call_id":"call-1","name":"read","arguments":"{}"}),
        ]);
        shaper.append_tool_results(
            &[ModelToolCall {
                id: "call-1".to_owned(),
                name: "read".to_owned(),
                arguments: json!({}),
            }],
            &["read result".to_owned()],
        );
        shaper.append_user("repair detail");
        shaper.context.sticky_routing_token = Some("route-state".to_owned());
        let second = build_wire_request(&shaper.context, &auth, &config).unwrap();

        assert!(first.body.ends_with(b"]}"));
        assert!(second.body.starts_with(&first.body[..first.body.len() - 2]));
        assert_eq!(first.header("session-id"), Some(shaper_cache_key.as_str()));
        assert_eq!(second.header("session-id"), Some(shaper_cache_key.as_str()));
        assert_eq!(first.header("thread-id"), Some(shaper_thread_id.as_str()));
        assert_eq!(second.header("thread-id"), Some(shaper_thread_id.as_str()));
        assert_eq!(second.header("x-codex-turn-state"), Some("route-state"));
        let shaper_tools = shaper
            .context
            .tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(shaper_tools, ["read", "search", "write"]);

        let mut fallback = provider
            .session(ShapeSessionSpec {
                run_dir: &run_dir,
                stage: "fallback_shaper",
                attempt: "pass-4",
                output_role: "fallback_shaper",
                model: "gpt-6.1-sol",
                effort: "high",
                instructions: "stable shaper instructions",
                tools: true,
                initial_user: "stable first request plus last failure".to_owned(),
            })
            .unwrap();
        let fallback_first = build_wire_request(&fallback.context, &auth, &config).unwrap();
        fallback.append_user("fallback repair detail");
        let fallback_second = build_wire_request(&fallback.context, &auth, &config).unwrap();

        assert!(
            fallback_second
                .body
                .starts_with(&fallback_first.body[..fallback_first.body.len() - 2])
        );
        assert_eq!(
            fallback_first.header("session-id"),
            Some(shaper_cache_key.as_str())
        );
        assert_eq!(
            fallback_second.header("session-id"),
            Some(shaper_cache_key.as_str())
        );
        assert_ne!(fallback.context.thread_id, shaper_thread_id);
        assert_eq!(
            fallback_first.header("thread-id"),
            Some(fallback.context.thread_id.as_str())
        );
        assert_eq!(
            fallback_second.header("thread-id"),
            Some(fallback.context.thread_id.as_str())
        );

        fs::remove_dir_all(root).unwrap();
    }
}

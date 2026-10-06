//! Shaping model conversations over the shared provider request machinery.

use super::ShapeModelCall;
use crate::error::{CoreError, ErrorClass};
use crate::provider::auth;
use crate::provider::http::retry::RetryReplay;
use crate::provider::http::{
    ApiMode, ProviderCallFailure, RequestContext, RequestPolicy, ReqwestPort, SystemClock,
    WireConfig, respond,
};
use crate::provider::session::{ConversationBinding, ConversationHistory};
use crate::provider::tools::{self, ToolContext, ToolRole};
use crate::provider::{ModelResponse, ModelToolCall, RunAccount};
use crate::run::{ChildEnvironment, ProcessPort};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Instant;

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
        let mut history = ConversationHistory::default();
        history.append_user(spec.initial_user);
        // Resolve the binding once here so a scratch path problem fails before
        // the provider receives any request.
        binding.cache_key().map_err(provider_io_error)?;
        Ok(ShapeSession {
            binding,
            output_role: spec.output_role.to_owned(),
            model: spec.model.to_owned(),
            effort: spec.effort.to_owned(),
            instructions: spec.instructions.to_owned(),
            history,
            tools: spec.tools,
            turns: 0,
        })
    }

    pub fn turn(&self, session: &mut ShapeSession) -> Result<ShapeTurn, CoreError> {
        if session.turns >= MAX_TURNS {
            return Err(CoreError::new(
                ErrorClass::Candidate,
                "shape_turn_limit",
                "Shaper exhausted its turn limit.",
                crate::ExitCode::Negative,
            ));
        }
        let credential = auth::credential_for_request(&self.home, &self.account)?;
        let mut context = RequestContext::for_conversation(
            &session.binding,
            session.model.clone(),
            session.effort.clone(),
            session.instructions.clone(),
            session.history.items().to_vec(),
        )
        .map_err(provider_io_error)?;
        if session.tools {
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
        let config =
            WireConfig::from_auth(&credential, ApiMode::Responses).map_err(provider_failure)?;
        let mut request_credential = credential;
        let mut retry = RetryReplay::default();
        let start = Instant::now();
        let call = respond(
            &mut context,
            &mut request_credential,
            &config,
            &mut retry,
            &RequestPolicy {
                role: "planner".to_owned(),
                mode: "shape".to_owned(),
                fallback_on: false,
                fallback_model: session.model.clone(),
                fallback_effort: session.effort.clone(),
                wall_budget_ms: None,
            },
            &self.http,
            &self.clock,
            Some(&self.home),
            Some(&self.account.label),
        )
        .map_err(call_failure)?;
        session.turns += 1;
        let response = call.response;
        session
            .history
            .append_all(response.raw_items.iter().cloned());
        let call = ShapeModelCall {
            role: session.output_role.clone(),
            model: session.model.clone(),
            effort: session.effort.clone(),
            usage: response.usage.clone(),
            wall_ms: start.elapsed().as_millis() as u64,
        };
        Ok(ShapeTurn { response, call })
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
    binding: ConversationBinding,
    output_role: String,
    model: String,
    effort: String,
    instructions: String,
    history: ConversationHistory,
    tools: bool,
    turns: usize,
}

impl ShapeSession {
    pub fn append_user(&mut self, text: impl Into<String>) {
        self.history.append_user(text);
    }

    pub fn append_tool_results(&mut self, calls: &[ModelToolCall], outputs: &[String]) {
        self.history.append_tool_results(calls, outputs);
    }

    pub fn turn_count(&self) -> usize {
        self.turns
    }
}

pub(super) struct ShapeTurn {
    pub response: ModelResponse,
    pub call: ShapeModelCall,
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

fn call_failure(failure: ProviderCallFailure) -> CoreError {
    provider_failure(*failure.failure)
}

fn provider_io_error(error: std::io::Error) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        "shape_scratch_unavailable",
        error.to_string(),
        crate::ExitCode::Environment,
    )
}

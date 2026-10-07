use super::approval::ApprovedBuild;
use super::config::BuildOptions;
use super::provider_error::provider_error;
use super::provider_prompt::{
    append_transcript, auditor_instructions, builder_instructions, builder_message, now_ms,
    planner_instructions, user_item, workspace_changed,
};
use crate::error::CoreError;
use crate::project::ProjectResolution;
use crate::provider::RunAccount;
use crate::provider::auth;
use crate::provider::http::retry::RetryReplay;
use crate::provider::http::{ApiMode, RequestContext, WireConfig};
use crate::provider::http::{ProviderCall, RequestEvent, RequestPolicy, SystemClock, respond};
use crate::provider::session::{ConversationBinding, ConversationHistory};
use crate::run::{RunEvent, RunSnapshot, RunStore};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;

pub(super) struct BuildProvider<'a> {
    approved: &'a ApprovedBuild,
    options: &'a BuildOptions,
    store: &'a RunStore,
    snapshot: &'a mut RunSnapshot,
    home: std::path::PathBuf,
    account: RunAccount,
    http: crate::provider::http::ReqwestPort,
    clock: SystemClock,
}

impl<'a> BuildProvider<'a> {
    pub fn new(
        project: &'a ProjectResolution,
        approved: &'a ApprovedBuild,
        options: &'a BuildOptions,
        store: &'a RunStore,
        snapshot: &'a mut RunSnapshot,
    ) -> Result<Self, CoreError> {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| super::environment_error("home_unavailable", "HOME is not set"))?;
        let committed_account = project
            .config
            .as_ref()
            .and_then(|config| config.raw.as_mapping())
            .and_then(|config| config.get(serde_yaml::Value::String("account".to_owned())))
            .and_then(serde_yaml::Value::as_str);
        let account = crate::provider::resolve_run_account(
            &home,
            &project.checkout,
            committed_account,
            std::env::var("KOGEN_BENCH_PROVIDER").ok().as_deref(),
            std::env::var("KOGEN_BENCH_ACCOUNT").ok().as_deref(),
            std::env::var_os("KOGEN_AUTH_PATH").is_some(),
        )?;
        let http = crate::provider::http::ReqwestPort::new()
            .map_err(|failure| provider_error(failure.kind, failure.message))?;
        Ok(Self {
            approved,
            options,
            store,
            snapshot,
            home,
            account,
            http,
            clock: SystemClock::default(),
        })
    }

    pub fn account(&self) -> &RunAccount {
        &self.account
    }

    pub fn record_event(&mut self, event: &RunEvent) -> Result<(), CoreError> {
        self.record(event)
    }

    pub fn plan(&mut self, run_dir: &Path, files: &str) -> Result<(String, u64), CoreError> {
        let input = format!(
            "Approved Intent:\n{}\n\nRepository files:\n{}",
            String::from_utf8_lossy(&self.approved.intent_bytes),
            files
        );
        let mut context = self.request_context(
            run_dir,
            "plan",
            "planner",
            &self.options.planner_model,
            &self.options.planner_effort,
            planner_instructions(),
            vec![user_item(&input)],
            Vec::new(),
            Vec::new(),
            false,
        )?;
        let before = Instant::now();
        let call = self.call(&mut context, "planner", "plan", None)?;
        let elapsed = before.elapsed().as_millis() as u64;
        let difficulty = call
            .response
            .text
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("Difficulty: "))
            .filter(|value| matches!(*value, "easy" | "hard"))
            .unwrap_or("easy")
            .to_owned();
        self.record(
            &RunEvent::new("plan", now_ms())
                .with("difficulty", json!(difficulty))
                .with("wall_ms", json!(elapsed)),
        )?;
        self.record_call("plan", "", &call, elapsed)?;
        append_transcript(
            self.store,
            json!({
                "stage":"plan",
                "model":context.model,
                "input":context.input,
                "text":&call.response.text,
                "raw_items":&call.response.raw_items,
            }),
        )?;
        Ok((format!("{difficulty}\0{}", call.response.text), elapsed))
    }

    pub fn audit(
        &mut self,
        run_dir: &Path,
        request: &crate::run::orchestration::BuildAuditRequest,
    ) -> Result<String, CoreError> {
        let mut context = self.request_context(
            run_dir,
            "audit",
            "auditor",
            "gpt-6.1-sol",
            "high",
            auditor_instructions(),
            vec![user_item(&request.user_message())],
            Vec::new(),
            Vec::new(),
            false,
        )?;
        context.tool_choice = "none".to_owned();
        let before = Instant::now();
        let call = self.call(&mut context, "auditor", "audit", None)?;
        let elapsed = before.elapsed().as_millis() as u64;
        self.record_call("audit", "R1", &call, elapsed)?;
        append_transcript(
            self.store,
            json!({
                "stage":"audit",
                "model":context.model,
                "input":context.input,
                "text":&call.response.text,
                "raw_items":&call.response.raw_items,
            }),
        )?;
        Ok(call.response.text)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn develop(
        &mut self,
        run_dir: &Path,
        workspace: &Path,
        plan: &str,
        base_acceptance: &str,
        process: &dyn crate::run::ProcessPort,
        environment: crate::run::ChildEnvironment,
        protected: &crate::gate::ProtectedWorkspace,
        recipe_direct: bool,
    ) -> Result<DevelopResult, CoreError> {
        let (_, plan) = plan.split_once('\0').unwrap_or(("easy", plan));
        let role = if recipe_direct {
            crate::provider::tools::ToolRole::BuilderDirect
        } else {
            crate::provider::tools::ToolRole::BuilderShell
        };
        let callable = role.allowed();
        let schemas = crate::provider::tools::canonical_tool_schemas()
            .into_iter()
            .filter(|schema| {
                schema
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| callable.contains(&name))
            })
            .collect();
        let builder_instructions = builder_instructions(recipe_direct);
        let first = builder_message(&self.approved.intent_bytes, base_acceptance, plan);
        let mut context = self.request_context(
            run_dir,
            "develop",
            "builder",
            &self.options.builder_model,
            &self.options.builder_effort,
            builder_instructions,
            vec![user_item(&first)],
            schemas,
            callable.iter().map(|name| (*name).to_owned()).collect(),
            true,
        )?;
        let mut turns = 0_u32;
        let mut empty_finish_count = 0_u8;
        let mut budget_note_added = false;
        loop {
            turns = turns.saturating_add(1);
            if turns > 60 {
                return Ok(DevelopResult {
                    reason: "turn_cap".to_owned(),
                    turns,
                    changed: workspace_changed(workspace, &self.options.setup_outputs),
                    tool_outputs: Vec::new(),
                    model_stages: 0,
                });
            }
            let before = Instant::now();
            let call = self.call(
                &mut context,
                "builder",
                "develop",
                Some(self.options.wall_ms),
            )?;
            self.record_call("develop", "R1", &call, before.elapsed().as_millis() as u64)?;
            append_transcript(
                self.store,
                json!({
                    "stage":"develop",
                    "turn":turns,
                    "model":context.model,
                    "input":&context.input,
                    "raw_items":&call.response.raw_items,
                    "text":&call.response.text,
                }),
            )?;
            if call.response.tool_calls.is_empty() {
                let mut history = ConversationHistory::new(std::mem::take(&mut context.input));
                history.append_all(call.response.raw_items.clone());
                history.append_user("Continue the entire approved Intent with the next useful tool call. Brief progress text does not finish the Build; call finish alone with {} when implementation and targeted verification are complete.");
                super::provider_prompt::append_turn_budget_note(
                    &mut history,
                    turns,
                    &mut budget_note_added,
                );
                context.input = history.items().to_vec();
                continue;
            }
            let calls = call.response.tool_calls.clone();
            let mut outputs = Vec::with_capacity(calls.len());
            let mut finish = false;
            for tool_call in &calls {
                if tool_call.name == "finish" {
                    if calls.len() != 1
                        || !tool_call
                            .arguments
                            .as_object()
                            .is_some_and(|v| v.is_empty())
                    {
                        outputs.push(crate::provider::tools::ToolError::FinishGuard.render());
                        continue;
                    }
                    if !workspace_changed(workspace, &self.options.setup_outputs)
                        && empty_finish_count == 0
                    {
                        empty_finish_count = 1;
                        outputs.push("Kogen found no changed files. Make the requested change before claiming done.".to_owned());
                        continue;
                    }
                    finish = true;
                    outputs.push("Completion requested. Kogen will run the gate.".to_owned());
                    continue;
                }
                let context = crate::provider::tools::ToolContext {
                    role,
                    workspace,
                    run_dir,
                    shaper_write_paths: ["", ""],
                    result_tokens: self.options.tool_tokens,
                    process: Some(process),
                    environment: environment.clone(),
                };
                let output = crate::provider::tools::dispatch(&context, tool_call, calls.len())
                    .unwrap_or_else(|error| error.render());
                outputs.push(output);
                let restored = protected.restore_after_batch(workspace).unwrap_or_default();
                for path in restored {
                    self.record(
                        &RunEvent::new("protected_restored", now_ms())
                            .with("rung", json!("R1"))
                            .with("path", json!(path)),
                    )?;
                    outputs.push(format!("You changed {path}; acceptance tests and the Intent are read-only and have been restored. Make the implementation satisfy them."));
                }
            }
            let mut history = ConversationHistory::new(std::mem::take(&mut context.input));
            history.append_all(call.response.raw_items.clone());
            history.append_tool_results(&calls, &outputs);
            super::provider_prompt::append_turn_budget_note(
                &mut history,
                turns,
                &mut budget_note_added,
            );
            context.input = history.items().to_vec();
            if finish {
                return Ok(DevelopResult {
                    reason: "finish".to_owned(),
                    turns,
                    changed: workspace_changed(workspace, &self.options.setup_outputs),
                    tool_outputs: outputs,
                    model_stages: turns,
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn request_context(
        &self,
        run_dir: &Path,
        stage: &str,
        attempt: &str,
        model: &str,
        effort: &str,
        instructions: String,
        input: Vec<Value>,
        tools: Vec<Value>,
        callable_tools: Vec<String>,
        development: bool,
    ) -> Result<RequestContext, CoreError> {
        let mut binding = ConversationBinding::new(run_dir, stage);
        binding.attempt = attempt.to_owned();
        binding.rung = "R1".to_owned();
        let mut context =
            RequestContext::for_conversation(&binding, model, effort, instructions, input)
                .map_err(|error| {
                    super::environment_error("conversation_identity_failed", error.to_string())
                })?;
        context.tools = tools;
        context.callable_tools = callable_tools;
        context.tool_choice = "auto".to_owned();
        context.development_request = development;
        context.generation_tokens = if development {
            self.options.model_generation_tokens
        } else {
            None
        };
        Ok(context)
    }

    fn call(
        &mut self,
        context: &mut RequestContext,
        role: &str,
        mode: &str,
        wall_budget_ms: Option<u64>,
    ) -> Result<ProviderCall, CoreError> {
        let mut credential = auth::credential_for_request(&self.home, &self.account)?;
        let mut wire = WireConfig::from_auth(&credential, ApiMode::Responses)
            .map_err(|failure| provider_error(failure.kind, failure.message))?;
        if let Ok(endpoint) = std::env::var("KOGEN_PROVIDER_URL") {
            wire.endpoint_override = url::Url::parse(&endpoint).ok();
        }
        let mut retry = RetryReplay::default();
        let no_fallback = std::env::var("KOGEN_BENCH_NO_FALLBACK").is_ok_and(|value| value == "1");
        let mut request_policy = RequestPolicy {
            role: role.to_owned(),
            mode: "build".to_owned(),
            fallback_on: role != "planner" && self.options.fallback_on && !no_fallback,
            fallback_model: "gpt-6.1-sol".to_owned(),
            fallback_effort: "medium".to_owned(),
            wall_budget_ms,
        };
        if self.account.provider == "grok" {
            request_policy.fallback_on = false;
        }
        let result = respond(
            context,
            &mut credential,
            &wire,
            &mut retry,
            &request_policy,
            &self.http,
            &self.clock,
            Some(&self.home),
            Some(&self.account.label),
        );
        match result {
            Ok(call) => Ok(call),
            Err(failure) => {
                self.record_provider_events(mode, &failure.events)?;
                let detail = failure.failure.message.clone();
                Err(provider_error(failure.failure.kind, detail))
            }
        }
    }

    fn record_provider_events(
        &mut self,
        stage: &str,
        events: &[RequestEvent],
    ) -> Result<(), CoreError> {
        super::provider_journal::record_provider_events(self.store, self.snapshot, stage, events)
    }

    fn record_call(
        &mut self,
        stage: &str,
        rung: &str,
        call: &ProviderCall,
        wall_ms: u64,
    ) -> Result<(), CoreError> {
        super::provider_journal::record_call(self.store, self.snapshot, stage, rung, call, wall_ms)
    }

    fn record(&mut self, event: &RunEvent) -> Result<(), CoreError> {
        super::provider_journal::record_event(self.store, self.snapshot, event)
    }
}

#[derive(Clone, Debug)]
pub(super) struct DevelopResult {
    pub reason: String,
    pub turns: u32,
    pub changed: bool,
    pub tool_outputs: Vec<String>,
    pub model_stages: u32,
}

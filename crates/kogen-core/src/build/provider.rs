use super::approval::ApprovedBuild;
use super::config::BuildOptions;
use super::provider_error::provider_error;
use super::provider_prompt::{
    append_transcript, auditor_instructions, builder_instructions, builder_message, now_ms,
    planner_instructions, user_item, workspace_changed, workspace_tree,
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
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(super) struct BuildProvider<'a> {
    approved: &'a ApprovedBuild,
    options: &'a BuildOptions,
    store: &'a RunStore,
    builder_session: Option<BuilderSession>,
    rung_sessions: BTreeMap<String, BuilderSession>,
    usage_paused_ms: u64,
    build_started: Instant,
    home: std::path::PathBuf,
    account: RunAccount,
    http: crate::provider::http::ReqwestPort,
    clock: SystemClock,
}

struct BuilderSession {
    context: RequestContext,
    turns: u32,
    empty_finish_count: u8,
    budget_note_added: bool,
    recipe_direct: bool,
}

impl<'a> BuildProvider<'a> {
    pub fn new(
        project: &'a ProjectResolution,
        approved: &'a ApprovedBuild,
        options: &'a BuildOptions,
        store: &'a RunStore,
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
            builder_session: None,
            rung_sessions: BTreeMap::new(),
            usage_paused_ms: 0,
            build_started: Instant::now(),
            home,
            account,
            http,
            clock: SystemClock::default(),
        })
    }

    pub fn account(&self) -> &RunAccount {
        &self.account
    }

    pub fn record_event(
        &mut self,
        snapshot: &mut RunSnapshot,
        event: &RunEvent,
    ) -> Result<(), CoreError> {
        self.record(snapshot, event)
    }

    pub fn plan(
        &mut self,
        snapshot: &mut RunSnapshot,
        run_dir: &Path,
        files: &str,
    ) -> Result<(String, u64), CoreError> {
        let input = format!(
            "Approved Intent:\n{}\n\nRepository files:\n{}",
            String::from_utf8_lossy(&self.approved.intent_bytes),
            files
        );
        let mut context = self.request_context(
            run_dir,
            "plan",
            "planner",
            "",
            &self.options.planner_model,
            &self.options.planner_effort,
            planner_instructions(),
            vec![user_item(&input)],
            Vec::new(),
            Vec::new(),
            false,
        )?;
        let before = Instant::now();
        let call = self.call(snapshot, &mut context, "planner", "plan", None)?;
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
            snapshot,
            &RunEvent::new("plan", now_ms())
                .with("difficulty", json!(difficulty))
                .with("wall_ms", json!(elapsed)),
        )?;
        self.record_call(snapshot, "plan", "", &call, elapsed)?;
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
        snapshot: &mut RunSnapshot,
        run_dir: &Path,
        rung: &str,
        request: &crate::run::orchestration::BuildAuditRequest,
    ) -> Result<String, CoreError> {
        let mut context = self.request_context(
            run_dir,
            "audit",
            "test-auditor",
            rung,
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
        let call = self.call(
            snapshot,
            &mut context,
            "auditor",
            "audit",
            Some(self.options.wall_ms),
        )?;
        let elapsed = before.elapsed().as_millis() as u64;
        self.record_call(snapshot, "audit", rung, &call, elapsed)?;
        append_transcript(
            self.store,
            json!({
                "stage":"audit",
                "rung":rung,
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
        snapshot: &mut RunSnapshot,
        run_dir: &Path,
        workspace: &Path,
        baseline_tree: &str,
        excluded_paths: &[PathBuf],
        plan: &str,
        base_acceptance: &str,
        process: &dyn crate::run::ProcessPort,
        environment: crate::run::ChildEnvironment,
        protected: &crate::gate::ProtectedWorkspace,
        recipe_direct: bool,
    ) -> Result<DevelopResult, CoreError> {
        let (_, plan) = plan.split_once('\0').unwrap_or(("easy", plan));
        let first = builder_message(&self.approved.intent_bytes, base_acceptance, plan);
        self.develop_on_rung(
            snapshot,
            run_dir,
            workspace,
            "R1",
            &self.options.builder_model,
            &self.options.builder_effort,
            &first,
            None,
            baseline_tree,
            excluded_paths,
            process,
            environment,
            protected,
            recipe_direct,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn repair(
        &mut self,
        snapshot: &mut RunSnapshot,
        run_dir: &Path,
        workspace: &Path,
        baseline_tree: &str,
        excluded_paths: &[PathBuf],
        process: &dyn crate::run::ProcessPort,
        environment: crate::run::ChildEnvironment,
        protected: &crate::gate::ProtectedWorkspace,
        feedback: &str,
        deadline: Option<Instant>,
    ) -> Result<DevelopResult, CoreError> {
        let first_message = builder_message(&self.approved.intent_bytes, "", "");
        let recipe_direct = self
            .builder_session
            .as_ref()
            .is_some_and(|session| session.recipe_direct);
        self.develop_on_rung(
            snapshot,
            run_dir,
            workspace,
            "R1",
            &self.options.builder_model,
            &self.options.builder_effort,
            &first_message,
            Some(feedback),
            baseline_tree,
            excluded_paths,
            process,
            environment,
            protected,
            recipe_direct,
            deadline,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn develop_on_rung(
        &mut self,
        snapshot: &mut RunSnapshot,
        run_dir: &Path,
        workspace: &Path,
        rung: &str,
        model: &str,
        effort: &str,
        first_message: &str,
        feedback: Option<&str>,
        baseline_tree: &str,
        excluded_paths: &[PathBuf],
        process: &dyn crate::run::ProcessPort,
        environment: crate::run::ChildEnvironment,
        protected: &crate::gate::ProtectedWorkspace,
        recipe_direct: bool,
        deadline: Option<Instant>,
    ) -> Result<DevelopResult, CoreError> {
        let progress_baseline = workspace_tree(workspace, excluded_paths).map_err(|error| {
            super::environment_error("candidate_snapshot_failed", error.to_string())
        })?;
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
        let existing = if rung == "R1" {
            self.builder_session.take()
        } else {
            self.rung_sessions.remove(rung)
        };
        let mut session = if let Some(session) = existing {
            session
        } else {
            let context = self.request_context(
                run_dir,
                "develop",
                "builder",
                rung,
                model,
                effort,
                builder_instructions(recipe_direct),
                vec![user_item(first_message)],
                schemas,
                callable.iter().map(|name| (*name).to_owned()).collect(),
                true,
            )?;
            BuilderSession {
                context,
                turns: 0,
                empty_finish_count: 0,
                budget_note_added: false,
                recipe_direct,
            }
        };
        if let Some(feedback) = feedback {
            let mut history = ConversationHistory::new(std::mem::take(&mut session.context.input));
            history.append_user(format!(
                "Kogen's controller reported this failure. Continue the same session and fix it:\n\n{feedback}"
            ));
            session.context.input = history.items().to_vec();
        }
        let result = self.drive_session(
            snapshot,
            &mut session,
            rung,
            run_dir,
            workspace,
            baseline_tree,
            &progress_baseline,
            excluded_paths,
            process,
            environment,
            protected,
            deadline,
        );
        if rung == "R1" {
            self.builder_session = Some(session);
        } else {
            self.rung_sessions.insert(rung.to_owned(), session);
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn drive_session(
        &mut self,
        snapshot: &mut RunSnapshot,
        session: &mut BuilderSession,
        rung: &str,
        run_dir: &Path,
        workspace: &Path,
        baseline_tree: &str,
        progress_baseline: &str,
        excluded_paths: &[PathBuf],
        process: &dyn crate::run::ProcessPort,
        environment: crate::run::ChildEnvironment,
        protected: &crate::gate::ProtectedWorkspace,
        deadline: Option<Instant>,
    ) -> Result<DevelopResult, CoreError> {
        let role = if session.recipe_direct {
            crate::provider::tools::ToolRole::BuilderDirect
        } else {
            crate::provider::tools::ToolRole::BuilderShell
        };
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return self.develop_result(
                    session,
                    workspace,
                    baseline_tree,
                    progress_baseline,
                    excluded_paths,
                    "landing_allowance_spent",
                    Vec::new(),
                    0,
                );
            }
            if self.builder_wall_budget() == 0 {
                return self.develop_result(
                    session,
                    workspace,
                    baseline_tree,
                    progress_baseline,
                    excluded_paths,
                    "budget",
                    Vec::new(),
                    0,
                );
            }
            if deadline.is_none() && session.turns >= 60 {
                return self.develop_result(
                    session,
                    workspace,
                    baseline_tree,
                    progress_baseline,
                    excluded_paths,
                    "turn_cap",
                    Vec::new(),
                    0,
                );
            }
            session.turns = session.turns.saturating_add(1);
            let before = Instant::now();
            let call = loop {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return self.develop_result(
                        session,
                        workspace,
                        baseline_tree,
                        progress_baseline,
                        excluded_paths,
                        "landing_allowance_spent",
                        Vec::new(),
                        0,
                    );
                }
                let wall_budget_ms = deadline
                    .map(|deadline| {
                        deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis()
                            .max(1)
                            .min(u64::MAX as u128) as u64
                    })
                    .unwrap_or_else(|| self.builder_wall_budget())
                    .min(self.builder_wall_budget());
                if wall_budget_ms == 0 {
                    return self.develop_result(
                        session,
                        workspace,
                        baseline_tree,
                        progress_baseline,
                        excluded_paths,
                        "budget",
                        Vec::new(),
                        0,
                    );
                }
                match self.call(
                    snapshot,
                    &mut session.context,
                    "builder",
                    "develop",
                    Some(wall_budget_ms),
                ) {
                    Ok(call) => break call,
                    Err(error)
                        if error.class == crate::error::ErrorClass::Provider
                            && matches!(error.reason.as_str(), "usage_limit" | "login")
                            && self.usage_paused_ms < 86_400_000 =>
                    {
                        self.usage_paused_ms =
                            self.usage_paused_ms.saturating_add(self.scaled_ms(300_000));
                    }
                    Err(error) => return Err(error),
                }
            };
            self.record_call(
                snapshot,
                "develop",
                rung,
                &call,
                before.elapsed().as_millis() as u64,
            )?;
            append_transcript(
                self.store,
                json!({
                    "stage":"develop",
                    "turn":session.turns,
                    "model":session.context.model,
                    "input":&session.context.input,
                    "raw_items":&call.response.raw_items,
                    "text":&call.response.text,
                }),
            )?;
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return self.develop_result(
                    session,
                    workspace,
                    baseline_tree,
                    progress_baseline,
                    excluded_paths,
                    "landing_allowance_spent",
                    Vec::new(),
                    0,
                );
            }
            if self.builder_wall_budget() == 0 {
                return self.develop_result(
                    session,
                    workspace,
                    baseline_tree,
                    progress_baseline,
                    excluded_paths,
                    "budget",
                    Vec::new(),
                    1,
                );
            }
            if call.response.tool_calls.is_empty() {
                let mut history =
                    ConversationHistory::new(std::mem::take(&mut session.context.input));
                history.append_all(call.response.raw_items.clone());
                history.append_user("Continue the entire approved Intent with the next useful tool call. Brief progress text does not finish the Build; call finish alone with {} when implementation and targeted verification are complete.");
                super::provider_prompt::append_turn_budget_note(
                    &mut history,
                    session.turns,
                    &mut session.budget_note_added,
                );
                session.context.input = history.items().to_vec();
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
                            .is_some_and(|value| value.is_empty())
                    {
                        outputs.push(crate::provider::tools::ToolError::FinishGuard.render());
                        continue;
                    }
                    let changed = workspace_changed(workspace, baseline_tree, excluded_paths)
                        .map_err(|error| {
                            super::environment_error("candidate_snapshot_failed", error.to_string())
                        })?;
                    if !changed && session.empty_finish_count == 0 {
                        session.empty_finish_count = 1;
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
                        snapshot,
                        &RunEvent::new("protected_restored", now_ms())
                            .with("rung", json!(rung))
                            .with("path", json!(path)),
                    )?;
                    outputs.push(format!("You changed {path}; acceptance tests and the Intent are read-only and have been restored. Make the implementation satisfy them."));
                }
            }
            let mut history = ConversationHistory::new(std::mem::take(&mut session.context.input));
            history.append_all(call.response.raw_items.clone());
            history.append_tool_results(&calls, &outputs);
            super::provider_prompt::append_turn_budget_note(
                &mut history,
                session.turns,
                &mut session.budget_note_added,
            );
            session.context.input = history.items().to_vec();
            if finish {
                return self.develop_result(
                    session,
                    workspace,
                    baseline_tree,
                    progress_baseline,
                    excluded_paths,
                    "finish",
                    outputs,
                    session.turns,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn develop_result(
        &self,
        session: &BuilderSession,
        workspace: &Path,
        baseline_tree: &str,
        progress_baseline: &str,
        excluded_paths: &[PathBuf],
        reason: &str,
        tool_outputs: Vec<String>,
        model_stages: u32,
    ) -> Result<DevelopResult, CoreError> {
        let current_tree = workspace_tree(workspace, excluded_paths).map_err(|error| {
            super::environment_error("candidate_snapshot_failed", error.to_string())
        })?;
        Ok(DevelopResult {
            reason: reason.to_owned(),
            turns: session.turns,
            changed: current_tree != baseline_tree,
            progressed: current_tree != progress_baseline,
            tool_outputs,
            model_stages,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn request_context(
        &self,
        run_dir: &Path,
        stage: &str,
        attempt: &str,
        rung: &str,
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
        binding.rung = rung.to_owned();
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

    fn builder_wall_budget(&self) -> u64 {
        let elapsed = self.build_started.elapsed().as_millis() as u64;
        let total = self.scaled_ms(self.options.wall_ms);
        let stage_cap = self.scaled_ms(1_800_000);
        total
            .saturating_sub(elapsed.saturating_sub(self.usage_paused_ms))
            .min(stage_cap)
    }

    fn scaled_ms(&self, milliseconds: u64) -> u64 {
        ((milliseconds as f64 * self.time_scale()).round() as u64).max(1)
    }

    fn time_scale(&self) -> f64 {
        std::env::var("KOGEN_TIME_SCALE")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0)
    }

    fn call(
        &mut self,
        snapshot: &mut RunSnapshot,
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
                self.record_provider_events(snapshot, mode, &failure.events)?;
                let detail = failure.failure.message.clone();
                Err(provider_error(failure.failure.kind, detail))
            }
        }
    }

    fn record_provider_events(
        &mut self,
        snapshot: &mut RunSnapshot,
        stage: &str,
        events: &[RequestEvent],
    ) -> Result<(), CoreError> {
        super::provider_journal::record_provider_events(self.store, snapshot, stage, events)
    }

    fn record_call(
        &mut self,
        snapshot: &mut RunSnapshot,
        stage: &str,
        rung: &str,
        call: &ProviderCall,
        wall_ms: u64,
    ) -> Result<(), CoreError> {
        super::provider_journal::record_call(self.store, snapshot, stage, rung, call, wall_ms)
    }

    fn record(&mut self, snapshot: &mut RunSnapshot, event: &RunEvent) -> Result<(), CoreError> {
        super::provider_journal::record_event(self.store, snapshot, event)
    }
}

#[derive(Clone, Debug)]
pub(super) struct DevelopResult {
    pub reason: String,
    pub turns: u32,
    pub changed: bool,
    pub progressed: bool,
    pub tool_outputs: Vec<String>,
    pub model_stages: u32,
}

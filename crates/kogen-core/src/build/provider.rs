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
    shared_tools: Vec<Value>,
    shared_context: String,
    usage_paused_ms: u64,
    stage_error_can_resume: bool,
    build_started: Instant,
    home: std::path::PathBuf,
    account: RunAccount,
    http: crate::provider::http::ReqwestPort,
    clock: SystemClock,
}

struct BuilderSession {
    context: RequestContext,
    turns: u32,
    previous_input_items: usize,
    empty_finish_count: u8,
    protected_restores: u8,
    budget_note_added: bool,
    recipe_direct: bool,
}

const PROTECTED_RESTORE_LIMIT: u8 = 4;

// Usage-limit waits and missing owned-login errors can resume a stage. A login
// error after sending a provider request has already had its forced refresh.
fn should_retry_stage_provider_error(
    reason: &str,
    attempts: usize,
    credential_source: &str,
) -> bool {
    reason == "usage_limit"
        || (reason == "login" && attempts == 0 && credential_source != "injected")
}

fn protected_restore_limit_reached(restores: u8) -> bool {
    restores >= PROTECTED_RESTORE_LIMIT
}

impl<'a> BuildProvider<'a> {
    pub fn new(
        project: &'a ProjectResolution,
        approved: &'a ApprovedBuild,
        options: &'a BuildOptions,
        store: &'a RunStore,
        base_sha: &str,
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
        let tool_role = if matches!(options.recipe.as_str(), "direct" | "direct-escalate") {
            crate::provider::tools::ToolRole::BuilderDirect
        } else {
            crate::provider::tools::ToolRole::BuilderShell
        };
        let http = crate::provider::http::ReqwestPort::new()
            .map_err(|failure| provider_error(failure.kind, failure.message))?;
        Ok(Self {
            approved,
            options,
            store,
            builder_session: None,
            rung_sessions: BTreeMap::new(),
            usage_paused_ms: 0,
            stage_error_can_resume: false,
            build_started: Instant::now(),
            home,
            account,
            http,
            clock: SystemClock::default(),
            shared_tools: shared_build_tool_schemas(tool_role),
            shared_context: shared_build_context(project, approved, options, base_sha)?,
        })
    }

    pub fn account(&self) -> &RunAccount {
        &self.account
    }

    pub fn fork(&self) -> Result<Self, CoreError> {
        let http = crate::provider::http::ReqwestPort::new()
            .map_err(|failure| provider_error(failure.kind, failure.message))?;
        Ok(Self {
            approved: self.approved,
            options: self.options,
            store: self.store,
            builder_session: None,
            rung_sessions: BTreeMap::new(),
            shared_tools: self.shared_tools.clone(),
            shared_context: self.shared_context.clone(),
            usage_paused_ms: self.usage_paused_ms,
            stage_error_can_resume: false,
            build_started: self.build_started,
            home: self.home.clone(),
            account: self.account.clone(),
            http,
            clock: SystemClock::default(),
        })
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
            false,
        )?;
        let (call, elapsed) = loop {
            let before = Instant::now();
            match self.call(snapshot, &mut context, "planner", "plan", None) {
                Ok(call) => break (call, before.elapsed().as_millis() as u64),
                Err(error)
                    if error.class == crate::error::ErrorClass::Provider
                        && self.stage_error_can_resume
                        && self.usage_paused_ms < 86_400_000 =>
                {
                    self.usage_paused_ms =
                        self.usage_paused_ms.saturating_add(self.scaled_ms(300_000));
                }
                Err(error) => return Err(error),
            }
        };
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
        self.record_call(
            snapshot,
            "plan",
            "",
            &call,
            elapsed,
            super::provider_journal::RequestIdentity {
                cache_key: &context.cache_key,
                thread_id: &context.thread_id,
                previous_input_items: 0,
            },
        )?;
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
        self.audit_with_prompt(
            snapshot,
            run_dir,
            rung,
            "audit",
            auditor_instructions(),
            request,
        )
    }

    pub fn witness_audit(
        &mut self,
        snapshot: &mut RunSnapshot,
        run_dir: &Path,
        rung: &str,
        request: &crate::run::orchestration::BuildAuditRequest,
    ) -> Result<String, CoreError> {
        self.audit_with_prompt(
            snapshot,
            run_dir,
            rung,
            "witness_audit",
            super::provider_prompt::witness_auditor_instructions(),
            request,
        )
    }

    fn audit_with_prompt(
        &mut self,
        snapshot: &mut RunSnapshot,
        run_dir: &Path,
        rung: &str,
        stage: &str,
        instructions: String,
        request: &crate::run::orchestration::BuildAuditRequest,
    ) -> Result<String, CoreError> {
        let mut context = audit_request_context(
            self.options,
            &self.shared_tools,
            run_dir,
            stage,
            rung,
            instructions,
            vec![user_item(&request.user_message())],
        )?;
        context.set_shared_context(&self.shared_context);
        let before = Instant::now();
        let call = self.call(
            snapshot,
            &mut context,
            "auditor",
            stage,
            Some(self.options.wall_ms),
        )?;
        let elapsed = before.elapsed().as_millis() as u64;
        self.record_call(
            snapshot,
            stage,
            rung,
            &call,
            elapsed,
            super::provider_journal::RequestIdentity {
                cache_key: &context.cache_key,
                thread_id: &context.thread_id,
                previous_input_items: 0,
            },
        )?;
        append_transcript(
            self.store,
            json!({
                "stage":stage,
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
                callable.iter().map(|name| (*name).to_owned()).collect(),
                true,
            )?;
            BuilderSession {
                context,
                turns: 0,
                previous_input_items: 0,
                empty_finish_count: 0,
                protected_restores: 0,
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
                            && self.stage_error_can_resume
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
                super::provider_journal::RequestIdentity {
                    cache_key: &session.context.cache_key,
                    thread_id: &session.context.thread_id,
                    previous_input_items: session.previous_input_items,
                },
            )?;
            session.previous_input_items = call
                .attempts
                .last()
                .and_then(|wire| serde_json::from_slice::<Value>(&wire.body).ok())
                .and_then(|body| body.get("input").and_then(Value::as_array).map(Vec::len))
                .unwrap_or(session.context.input.len());
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
            let mut restoration_notes = Vec::new();
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
                    session.protected_restores = session.protected_restores.saturating_add(1);
                    self.record(
                        snapshot,
                        &RunEvent::new("protected_restored", now_ms())
                            .with("rung", json!(rung))
                            .with("path", json!(path)),
                    )?;
                    restoration_notes.push(format!("You changed {path}; acceptance tests and the Intent are read-only and have been restored. Make the implementation satisfy them."));
                }
            }
            session.context.input = append_tool_batch_history(
                std::mem::take(&mut session.context.input),
                call.response.raw_items.clone(),
                &calls,
                &outputs,
                &restoration_notes,
            );
            let mut history = ConversationHistory::new(std::mem::take(&mut session.context.input));
            super::provider_prompt::append_turn_budget_note(
                &mut history,
                session.turns,
                &mut session.budget_note_added,
            );
            session.context.input = history.items().to_vec();
            if protected_restore_limit_reached(session.protected_restores) {
                return self.develop_result(
                    session,
                    workspace,
                    baseline_tree,
                    progress_baseline,
                    excluded_paths,
                    "protected_restore_limit",
                    outputs,
                    session.turns,
                );
            }
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
        context.set_shared_context(&self.shared_context);
        configure_build_tools(&mut context, &self.shared_tools, callable_tools);
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
        self.stage_error_can_resume = false;
        let mut credential = match auth::credential_for_request(&self.home, &self.account) {
            Ok(credential) => credential,
            Err(error) => {
                self.stage_error_can_resume = error.class == crate::error::ErrorClass::Provider
                    && should_retry_stage_provider_error(
                        &error.reason,
                        0,
                        self.account.credential_source,
                    );
                return Err(error);
            }
        };
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
                self.stage_error_can_resume = should_retry_stage_provider_error(
                    failure.failure.kind.as_str(),
                    failure.attempts.len(),
                    self.account.credential_source,
                );
                self.record_provider_events(snapshot, mode, &failure.events)?;
                self.record(
                    snapshot,
                    &RunEvent::new("provider_attempts", now_ms())
                        .with("stage", json!(mode))
                        .with("attempt_usage", json!(failure.usages)),
                )?;
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
        identity: super::provider_journal::RequestIdentity<'_>,
    ) -> Result<(), CoreError> {
        super::provider_journal::record_call(
            self.store, snapshot, stage, rung, call, wall_ms, identity,
        )
    }

    fn record(&mut self, snapshot: &mut RunSnapshot, event: &RunEvent) -> Result<(), CoreError> {
        super::provider_journal::record_event(self.store, snapshot, event)
    }
}

fn append_tool_batch_history(
    existing: Vec<Value>,
    raw_items: Vec<Value>,
    calls: &[crate::provider::ModelToolCall],
    outputs: &[String],
    restoration_notes: &[String],
) -> Vec<Value> {
    let mut history = ConversationHistory::new(existing);
    history.append_all(raw_items);
    history.append_tool_results(calls, outputs);
    for note in restoration_notes {
        history.append_user(note.clone());
    }
    history.items().to_vec()
}

fn shared_build_tool_schemas(_role: crate::provider::tools::ToolRole) -> Vec<Value> {
    crate::provider::tools::canonical_tool_schemas()
}

fn configure_build_tools(
    context: &mut RequestContext,
    shared_tools: &[Value],
    callable_tools: Vec<String>,
) {
    context.tools = shared_tools.to_vec();
    context.callable_tools = callable_tools;
}

fn shared_build_context(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    base_sha: &str,
) -> Result<String, CoreError> {
    let files = if matches!(
        options.recipe.as_str(),
        "direct" | "direct-escalate" | "direct-shell" | "escalate-shell"
    ) {
        "No repository file list was supplied.".to_owned()
    } else {
        crate::git::GitRepo::new(&project.origin)
            .list_paths(base_sha)
            .map_err(|error| super::environment_error("base_files_unavailable", error.to_string()))?
            .join("\n")
    };
    Ok(format!(
        "Approved Intent:\n{}\n\nApproved acceptance-test source:\n{}\n\nRepository files at the Build base:\n{}",
        String::from_utf8_lossy(&approved.intent_bytes),
        String::from_utf8_lossy(&approved.acceptance_bytes),
        files,
    ))
}

fn audit_request_context(
    options: &BuildOptions,
    shared_tools: &[Value],
    run_dir: &Path,
    stage: &str,
    rung: &str,
    instructions: String,
    input: Vec<Value>,
) -> Result<RequestContext, CoreError> {
    let mut binding = ConversationBinding::new(run_dir, stage);
    binding.attempt = "test-auditor".to_owned();
    binding.rung = rung.to_owned();
    let mut context = RequestContext::for_conversation(
        &binding,
        &options.auditor_model,
        &options.auditor_effort,
        instructions,
        input,
    )
    .map_err(|error| super::environment_error("conversation_identity_failed", error.to_string()))?;
    configure_build_tools(&mut context, shared_tools, Vec::new());
    context.tool_choice = "none".to_owned();
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::{
        append_tool_batch_history, audit_request_context, configure_build_tools,
        protected_restore_limit_reached, shared_build_tool_schemas,
        should_retry_stage_provider_error,
    };
    use crate::build::config::BuildOptions;
    use crate::project::{ProjectConfig, ProjectResolution};
    use crate::provider::auth::{InjectedCredential, RequestCredential};
    use crate::provider::http::{RequestContext, ResponseMode, WireConfig, build_wire_request};
    use crate::provider::session::ConversationBinding;
    use serde_json::{Value, json};
    use serde_yaml::Value as YamlValue;
    use url::Url;

    #[test]
    fn restored_protected_paths_are_appended_after_tool_results() {
        let history = append_tool_batch_history(
            vec![json!({"role":"user","content":[{"type":"input_text","text":"original"}]})],
            vec![json!({"type":"function_call","call_id":"call-1"})],
            &[crate::provider::ModelToolCall {
                id: "item-1".to_owned(),
                name: "shell".to_owned(),
                arguments: json!({"cmd":"edit"}),
            }],
            &["exit 0".to_owned()],
            &["You changed test/acceptance/greet.t.sh; acceptance tests and the Intent are read-only and have been restored. Make the implementation satisfy them.".to_owned()],
        );

        assert_eq!(history[2]["type"], "function_call_output");
        assert_eq!(history[2]["output"], "exit 0");
        assert_eq!(history[3]["role"], "user");
        assert!(
            history[3]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("test/acceptance/greet.t.sh")
        );
    }

    #[test]
    fn fourth_protected_restore_ends_the_rung() {
        assert!(!protected_restore_limit_reached(3));
        assert!(protected_restore_limit_reached(4));
    }

    #[test]
    fn stage_retries_waitable_errors_but_not_a_provider_rejected_login() {
        assert!(should_retry_stage_provider_error(
            "usage_limit",
            1,
            "injected"
        ));
        assert!(should_retry_stage_provider_error("login", 0, "owned"));
        assert!(!should_retry_stage_provider_error("login", 0, "injected"));
        assert!(!should_retry_stage_provider_error("login", 2, "owned"));
        assert!(!should_retry_stage_provider_error("timeout", 1, "owned"));
    }

    #[test]
    fn toolless_build_requests_keep_the_canonical_schemas_and_disable_calls() {
        let root = std::env::temp_dir().join(format!(
            "kogen-build-tools-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).expect("create conversation root");
        let binding = ConversationBinding::new(&root, "audit");
        let mut context = RequestContext::for_conversation(
            &binding,
            "gpt-6.1-sol",
            "high",
            "instructions",
            vec![json!({"role":"user","content":[{"type":"input_text","text":"audit"}]})],
        )
        .expect("create request context");
        let expected = shared_build_tool_schemas(crate::provider::tools::ToolRole::BuilderShell);
        configure_build_tools(&mut context, &expected, Vec::new());

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
        context.tool_choice = "none".to_owned();
        let wire = build_wire_request(&context, &auth, &config).expect("encode request");
        let body: Value = serde_json::from_slice(&wire.body).expect("parse request body");
        let expected_names = expected
            .iter()
            .filter_map(|schema| schema.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert_eq!(context.tools, expected);
        assert!(context.callable_tools.is_empty());
        assert_eq!(body["tool_choice"], "none");
        assert_eq!(body["tools"], json!(expected));
        assert_eq!(
            expected_names,
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
        std::fs::remove_dir_all(root).expect("remove conversation root");
    }

    #[test]
    fn audit_request_uses_the_configured_auditor_model_and_effort() {
        let root = std::env::temp_dir().join(format!(
            "kogen-build-auditor-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).expect("create conversation root");
        let project = ProjectResolution {
            checkout: root.clone(),
            origin: root.clone(),
            base: "main".to_owned(),
            state_root: root.clone(),
            config: Some(ProjectConfig {
                name: "fixture".to_owned(),
                base: None,
                raw: serde_yaml::from_str(
                    "build:\n  roles:\n    auditor:\n      model: gpt-6-luna\n      effort: medium\n",
                )
                .expect("parse project config"),
            }),
        };
        let machine: Option<YamlValue> = Some(
            serde_yaml::from_str("roles:\n  auditor:\n    model: machine-model\n    effort: low\n")
                .expect("parse machine config"),
        );
        let options =
            BuildOptions::load_with_machine(&project, &machine).expect("load build options");
        let expected = shared_build_tool_schemas(crate::provider::tools::ToolRole::BuilderShell);
        let context = audit_request_context(
            &options,
            &expected,
            &root,
            "audit",
            "R1",
            "instructions".to_owned(),
            vec![json!({"role":"user","content":[{"type":"input_text","text":"audit"}]})],
        )
        .expect("create audit request context");

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
        let wire = build_wire_request(&context, &auth, &config).expect("encode audit request");
        let body: Value = serde_json::from_slice(&wire.body).expect("parse request body");

        assert_eq!(options.auditor_model, "gpt-6-luna");
        assert_eq!(options.auditor_effort, "medium");
        assert_eq!(body["model"], "gpt-6-luna");
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert_eq!(body["tool_choice"], "none");
        assert_eq!(context.tools, expected);
        assert!(context.callable_tools.is_empty());
        std::fs::remove_dir_all(root).expect("remove conversation root");
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

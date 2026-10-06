//! Shaping pass orchestration. The policy stays in core; the CLI only renders.

use super::super::commands::ShapeCommands;
use super::super::prompts;
use super::super::provider::{
    ShapeProvider, ShapeSession, ShapeSessionSpec, dispatch_shaper_tools,
};
use super::super::validation::ValidationFailure;
use super::config::{domains, gate_paths, role_config, selected_account};
use super::files::{
    create_run_dir, io_error, project_error, remove_stale, repair_limit_error, request_is_empty,
    secure_file, shape_error,
};
use super::validate::validate_pass;
use super::{ShapeOptions, ShapeReport};
use crate::error::{CoreError, ErrorClass};
use crate::project::{ProjectOptions as CoreProjectOptions, ProjectResolution, valid_slug};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

const SHAPER_DEFAULT: (&str, &str) = ("gpt-6.1-sol", "high");

pub(super) fn run(options: ShapeOptions) -> Result<ShapeReport, CoreError> {
    if !valid_slug(&options.slug) {
        return Err(shape_error(
            ErrorClass::Intent,
            "invalid_slug",
            format!("invalid slug {}", options.slug),
            crate::ExitCode::Usage,
        ));
    }
    if request_is_empty(&options.request) {
        return Err(shape_error(
            ErrorClass::Intent,
            "request_unavailable",
            "request is empty",
            crate::ExitCode::Usage,
        ));
    }
    let project = ProjectResolution::resolve(&CoreProjectOptions {
        cwd: Some(options.cwd.clone()),
        project: options.project.clone(),
        origin: options.origin.clone(),
        base: options.base.clone(),
        home: Some(options.home.clone()),
    })
    .map_err(project_error)?;
    let config = project.config.as_ref();
    let intent_rel = format!(".kogen/intents/{}/intent.md", options.slug);
    let intent_path = project.checkout.join(&intent_rel);
    let acceptance_ext = config
        .and_then(|config| config.raw["acceptance"]["ext"].as_str())
        .unwrap_or(".t.sh");
    let acceptance_rel = format!(".kogen/acceptance/{}{acceptance_ext}", options.slug);
    let acceptance_path = project.checkout.join(&acceptance_rel);
    let candidate_rel = format!(
        "{}/{}{acceptance_ext}",
        config
            .and_then(|config| config.raw["acceptance"]["candidate_dir"].as_str())
            .unwrap_or("test/acceptance"),
        options.slug
    );
    let intent_dir = intent_path.parent().expect("intent path has parent");
    fs::create_dir_all(intent_dir).map_err(|error| io_error("shape_output_unavailable", error))?;
    fs::create_dir_all(
        acceptance_path
            .parent()
            .expect("acceptance path has parent"),
    )
    .map_err(|error| io_error("shape_output_unavailable", error))?;
    remove_stale(&intent_dir.join("ledger.json"))?;
    remove_stale(&intent_dir.join("shape-warnings.json"))?;

    let run_dir = create_run_dir(&options.home, &options.slug)?;
    let transcript_path = run_dir.join("transcript.jsonl");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&transcript_path)
        .map_err(|error| io_error("shape_scratch_unavailable", error))?;
    secure_file(&transcript_path)?;

    let commands = ShapeCommands::new(&project.checkout, &run_dir, config)?;
    commands.setup(&project.checkout, config)?;
    let account = selected_account(&options.home, &project)?;
    let provider = ShapeProvider::new(&options.home, account)?;
    let domains = domains(config);
    let gate_paths = gate_paths(config);
    let initial = prompts::first_message(
        &options.slug,
        &domains,
        &gate_paths,
        &options.request,
        &intent_rel,
        &acceptance_rel,
    );
    let shaper_role = role_config(config, "shaper", SHAPER_DEFAULT);
    let fallback_role = role_config(config, "fallback_shaper", SHAPER_DEFAULT);
    let auditor_role = role_config(config, "auditor", SHAPER_DEFAULT);
    let result_tokens = config
        .and_then(|config| config.raw["build"]["tool_result_tokens"].as_u64())
        .unwrap_or(2_000);

    let request = options.request.clone();
    let mut state = RunState {
        options,
        checkout: project.checkout,
        config: project.config,
        run_dir,
        transcript_path,
        intent_path,
        acceptance_path,
        intent_rel,
        acceptance_rel,
        candidate_rel,
        gate_paths,
        request,
        initial,
        commands,
        provider,
        shaper_role,
        fallback_role,
        auditor_role,
        result_tokens,
        session: None,
        calls: Vec::new(),
        warnings: Vec::new(),
        progress: Vec::new(),
        last_failure: None,
        coverage_repaired: false,
        audit_repaired: false,
        concern_seen: BTreeSet::new(),
    };
    let mut conversation_style_repairs = 0;
    for pass in 1..=6 {
        state
            .warnings
            .retain(|warning| warning.code == "feasibility_concern");
        if pass == 1 || pass == 4 {
            conversation_style_repairs = 0;
        }
        let role = if pass <= 3 {
            "shaper"
        } else {
            "fallback_shaper"
        };
        let (model, effort) = if role == "shaper" {
            state.shaper_role.clone()
        } else {
            state.fallback_role.clone()
        };
        if pass == 1 || pass == 4 {
            let first = if pass == 4 {
                state
                    .progress
                    .push(format!("shaper pass={pass} role={role} fallback_started"));
                prompts::fallback_message(
                    &state.initial,
                    state
                        .last_failure
                        .as_ref()
                        .expect("fallback has last failure"),
                )
            } else {
                state
                    .progress
                    .push(format!("shaper pass={pass} role={role} started"));
                state.initial.clone()
            };
            state.session = Some(state.provider.session(ShapeSessionSpec {
                run_dir: &state.run_dir,
                stage: role,
                attempt: &format!("pass-{pass}"),
                output_role: role,
                model: &model,
                effort: &effort,
                instructions: prompts::SHAPER_SYSTEM,
                tools: true,
                initial_user: first,
            })?);
        } else {
            state
                .progress
                .push(format!("shaper pass={pass} role={role} started"));
            if let Some(failure) = &state.last_failure {
                state
                    .session
                    .as_mut()
                    .expect("shaper conversation exists")
                    .append_user(prompts::repair_message(
                        &state.intent_path,
                        &state.acceptance_path,
                        failure,
                    ));
            }
        }

        let validation = loop {
            let final_text = state.drive_shaper(role, &model, &effort, pass)?;
            let result = validate_pass(
                &mut state,
                pass,
                role,
                final_text.as_str(),
                conversation_style_repairs,
            )?;
            match result {
                PassResult::StyleRepair(findings) => {
                    debug_assert!(conversation_style_repairs < 2);
                    state
                        .progress
                        .push(format!("shaper pass={pass} role={role} style_repair"));
                    state
                        .session
                        .as_mut()
                        .expect("shaper conversation exists")
                        .append_user(prompts::style_message(&findings));
                    conversation_style_repairs += 1;
                }
                PassResult::Failure(failure) => break Err(failure),
                PassResult::Complete => break Ok(()),
            }
        };
        let turns = state.session.as_ref().map_or(0, ShapeSession::turn_count);
        state.progress.push(format!(
            "shaper pass={pass} role={role} complete turns={turns}"
        ));
        if let Err(failure) = validation {
            if matches!(
                failure.reason,
                "acceptance_check_unavailable" | "acceptance_adapter_unavailable"
            ) {
                return Err(CoreError::new(
                    ErrorClass::Environment,
                    failure.reason,
                    failure.detail,
                    crate::ExitCode::Environment,
                ));
            }
            state.progress.push(format!(
                "shaper pass={pass} role={role} validation_failed reason={}",
                failure.reason
            ));
            if pass == 6 {
                return Err(repair_limit_error(failure, pass, state.calls.len()));
            }
            state.last_failure = Some(failure);
            continue;
        }
        state
            .progress
            .push(format!("shaper pass={pass} role={role} validation_passed"));
        return Ok(ShapeReport {
            intent_path: state.intent_path,
            acceptance_path: state.acceptance_path,
            transcript_path: state.transcript_path,
            rounds: pass,
            warnings: state.warnings,
            calls: state.calls,
            progress: state.progress,
        });
    }
    unreachable!("the six-pass loop returns or errors")
}

pub(super) struct RunState {
    pub(super) options: ShapeOptions,
    pub(super) checkout: PathBuf,
    pub(super) config: Option<crate::project::ProjectConfig>,
    pub(super) run_dir: PathBuf,
    pub(super) transcript_path: PathBuf,
    pub(super) intent_path: PathBuf,
    pub(super) acceptance_path: PathBuf,
    pub(super) intent_rel: String,
    pub(super) acceptance_rel: String,
    pub(super) candidate_rel: String,
    pub(super) gate_paths: Vec<String>,
    pub(super) request: Vec<u8>,
    initial: String,
    pub(super) commands: ShapeCommands,
    provider: ShapeProvider,
    shaper_role: (String, String),
    fallback_role: (String, String),
    pub(super) auditor_role: (String, String),
    result_tokens: u64,
    session: Option<ShapeSession>,
    calls: Vec<super::super::ShapeModelCall>,
    pub(super) warnings: Vec<super::super::ShapeWarning>,
    pub(super) progress: Vec<String>,
    pub(super) last_failure: Option<ValidationFailure>,
    pub(super) coverage_repaired: bool,
    pub(super) audit_repaired: bool,
    pub(super) concern_seen: BTreeSet<String>,
}

pub(super) enum PassResult {
    Complete,
    Failure(ValidationFailure),
    StyleRepair(Vec<String>),
}

impl RunState {
    fn drive_shaper(
        &mut self,
        role: &str,
        model: &str,
        effort: &str,
        _pass: usize,
    ) -> Result<String, CoreError> {
        loop {
            let turn = self
                .provider
                .turn(self.session.as_mut().expect("shaper session exists"))?;
            self.record_call(&turn.call);
            if !turn.response.tool_calls.is_empty() {
                let outputs = dispatch_shaper_tools(
                    &turn,
                    &self.checkout,
                    &self.run_dir,
                    [&self.intent_rel, &self.acceptance_rel],
                    &self.commands.environment,
                    &self.commands.process,
                    self.result_tokens,
                );
                self.session
                    .as_mut()
                    .expect("shaper session exists")
                    .append_tool_results(&turn.response.tool_calls, &outputs);
                continue;
            }
            let missing = [self.intent_path.as_path(), self.acceptance_path.as_path()]
                .into_iter()
                .any(|path| fs::read(path).is_err());
            if missing {
                self.session
                    .as_mut()
                    .expect("shaper session exists")
                    .append_user(prompts::missing_guard(
                        &self.intent_path,
                        &self.acceptance_path,
                    ));
                continue;
            }
            let _ = (role, model, effort);
            return Ok(turn.response.text);
        }
    }

    pub(super) fn drive_auditor(
        &mut self,
        stage: &str,
        instructions: &str,
        message: String,
    ) -> Result<String, CoreError> {
        let (model, effort) = self.auditor_role.clone();
        let mut session = self.provider.session(ShapeSessionSpec {
            run_dir: &self.run_dir,
            stage,
            attempt: stage,
            output_role: "auditor",
            model: &model,
            effort: &effort,
            instructions,
            tools: false,
            initial_user: message,
        })?;
        let turn = self.provider.turn(&mut session)?;
        self.record_call(&turn.call);
        Ok(turn.response.text)
    }

    fn record_call(&mut self, call: &super::super::ShapeModelCall) {
        self.calls.push(call.clone());
        let value = json!({
            "kind": "model_call",
            "role": call.role,
            "model": call.model,
            "effort": call.effort,
            "usage": call.usage,
            "wall_ms": call.wall_ms
        });
        if let Ok(mut file) = OpenOptions::new().append(true).open(&self.transcript_path) {
            let _ = writeln!(file, "{}", value);
        }
    }
}

//! Shaping pass orchestration. The policy stays in core; the CLI only renders.

use super::super::commands::ShapeCommands;
use super::super::prompts;
use super::super::provider::{
    ShapeProvider, ShapeSession, ShapeSessionSpec, dispatch_shaper_tools,
};
use super::super::validation::ValidationFailure;
use super::accounting::ShapeAccounting;
use super::config::{domains, gate_paths, role_config, selected_account};
use super::files::{
    create_run_dir, io_error, project_error, remove_stale, repair_limit_error, request_is_empty,
    secure_file, shape_error,
};
use super::validate::validate_pass;
use super::{ShapeOptions, ShapeReport};
use crate::error::{CoreError, ErrorClass};
use crate::project::{
    CheckoutLock, ProjectOptions as CoreProjectOptions, ProjectResolution, valid_slug,
};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

const SHAPER_DEFAULT: (&str, &str) = ("gpt-6.1-sol", "high");

pub(super) fn run(options: ShapeOptions) -> Result<ShapeReport, CoreError> {
    let started = Instant::now();
    if !valid_slug(&options.slug) {
        return Err(shape_error(
            ErrorClass::Intent,
            "invalid_slug",
            "Slug must use lowercase letters, digits, and dashes.",
            crate::ExitCode::Usage,
        ));
    }
    let run_dir = create_run_dir(&options.home, &options.slug)?;
    let mut accounting = ShapeAccounting::new(&run_dir, started);
    let result = run_in_directory(options, run_dir, &mut accounting);
    finish_with_accounting(&mut accounting, result)
}

fn finish_with_accounting<T>(
    accounting: &mut ShapeAccounting,
    result: Result<T, CoreError>,
) -> Result<T, CoreError> {
    accounting.finish(&result);
    match accounting.write() {
        Ok(()) => result,
        Err(write_error) => match result {
            Ok(_) => Err(write_error),
            Err(error) => Err(error),
        },
    }
}

fn run_in_directory(
    options: ShapeOptions,
    run_dir: PathBuf,
    accounting: &mut ShapeAccounting,
) -> Result<ShapeReport, CoreError> {
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
    let (acceptance_rel, candidate_rel) =
        ShapeCommands::acceptance_paths(config, &project.checkout, &options.slug);
    let acceptance_path = project.checkout.join(&acceptance_rel);
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

    let transcript_path = run_dir.join("transcript.jsonl");
    run_with_scratch(
        options,
        project,
        ShapeScratch {
            intent_path,
            acceptance_path,
            intent_rel,
            acceptance_rel,
            candidate_rel,
            run_dir,
            transcript_path,
        },
        accounting,
    )
}

struct ShapeScratch {
    intent_path: PathBuf,
    acceptance_path: PathBuf,
    intent_rel: String,
    acceptance_rel: String,
    candidate_rel: String,
    run_dir: PathBuf,
    transcript_path: PathBuf,
}

fn run_with_scratch(
    options: ShapeOptions,
    project: ProjectResolution,
    scratch: ShapeScratch,
    accounting: &mut ShapeAccounting,
) -> Result<ShapeReport, CoreError> {
    let ShapeScratch {
        intent_path,
        acceptance_path,
        intent_rel,
        acceptance_rel,
        candidate_rel,
        run_dir,
        transcript_path,
    } = scratch;
    let config = project.config.as_ref();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&transcript_path)
        .map_err(|error| io_error("shape_scratch_unavailable", error))?;
    secure_file(&transcript_path)?;

    let commands = ShapeCommands::new(&project.checkout, &run_dir, config)?;
    commands.setup(&project.checkout, config)?;
    let account = selected_account(&options.home, &project)?;
    accounting.set_provider(&account.provider);
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
    // §3.2 resolves the fresh fallback conversation from the effective shaper
    // profile; provider-specific selection is preserved.
    let fallback_role = shaper_role.clone();
    let auditor_role = role_config(config, "auditor", SHAPER_DEFAULT);
    accounting.set_role("shaper", "shaper", &shaper_role.0, &shaper_role.1);
    accounting.set_role(
        "fallback_shaper",
        "fallback_shaper",
        &fallback_role.0,
        &fallback_role.1,
    );
    accounting.set_role("auditor", "auditor", &auditor_role.0, &auditor_role.1);
    let result_tokens = config
        .and_then(|config| config.raw["build"]["tool_result_tokens"].as_u64())
        .unwrap_or(2_000);

    let request = options.request.clone();
    let state_root = project.state_root.clone();
    let mut state = RunState {
        options,
        checkout: project.checkout,
        state_root,
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
        accounting,
    };
    let mut conversation_style_repairs = 0;
    let mut pass = 1;
    'passes: while pass <= 6 {
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
                let feedback = prompts::validation_feedback(
                    state
                        .last_failure
                        .as_ref()
                        .expect("fallback has last failure"),
                );
                state.record_feedback(pass, "validation", &feedback);
                state.accounting.repair(repair_kind(
                    state
                        .last_failure
                        .as_ref()
                        .expect("fallback has last failure")
                        .reason,
                ));
                prompts::fallback_message(&state.initial, &feedback)
            } else {
                state
                    .progress
                    .push(format!("shaper pass={pass} role={role} started"));
                state.initial.clone()
            };
            let session = state.provider.session(ShapeSessionSpec {
                run_dir: &state.run_dir,
                stage: role,
                attempt: &format!("pass-{pass}"),
                output_role: role,
                model: &model,
                effort: &effort,
                instructions: prompts::SHAPER_SYSTEM,
                tools: true,
                initial_user: first,
            })?;
            state.accounting.record_session(
                session.conversation_id(),
                role,
                role,
                session.effective_model(),
                session.effective_effort(),
            );
            state.session = Some(session);
        } else {
            state
                .progress
                .push(format!("shaper pass={pass} role={role} started"));
            if let Some(feedback) = state
                .last_failure
                .as_ref()
                .map(prompts::validation_feedback)
            {
                state.record_feedback(pass, "validation", &feedback);
                state.accounting.repair(repair_kind(
                    state
                        .last_failure
                        .as_ref()
                        .expect("feedback has failure")
                        .reason,
                ));
                state
                    .session
                    .as_mut()
                    .expect("shaper conversation exists")
                    .append_user(prompts::repair_message(
                        &state.intent_path,
                        &state.acceptance_path,
                        &feedback,
                    ));
            }
        }

        let validation = loop {
            let final_text = match state.drive_shaper(role, &model, &effort, pass) {
                Ok(final_text) => final_text,
                Err(error) => {
                    if pass <= 3 && error.reason == "shape_turn_limit" {
                        let failure =
                            primary_turn_limit_fallback(pass, &error, state.last_failure.take())
                                .expect("primary turn-limit error starts fallback");
                        state.last_failure = Some(failure);
                        pass = 4;
                        continue 'passes;
                    }
                    return Err(error);
                }
            };
            let result = match validate_pass(
                &mut state,
                pass,
                role,
                final_text.as_str(),
                conversation_style_repairs,
            ) {
                Ok(result) => result,
                Err(error) => {
                    state.accounting.validation_pass();
                    return Err(error);
                }
            };
            match result {
                PassResult::StyleRepair(findings) => {
                    debug_assert!(conversation_style_repairs < 2);
                    state
                        .progress
                        .push(format!("shaper pass={pass} role={role} style_repair"));
                    let feedback = prompts::style_message(&findings);
                    state.record_feedback(pass, "style", &feedback);
                    state.accounting.repair("style");
                    state
                        .session
                        .as_mut()
                        .expect("shaper conversation exists")
                        .append_user(feedback);
                    conversation_style_repairs += 1;
                }
                PassResult::Failure(failure) => {
                    state.accounting.validation_pass();
                    break Err(failure);
                }
                PassResult::Complete => {
                    state.accounting.validation_pass();
                    break Ok(());
                }
            }
        };
        let turns = state.session.as_ref().map_or(0, ShapeSession::turn_count);
        state.progress.push(format!(
            "shaper pass={pass} role={role} complete turns={turns}"
        ));
        if let Err(failure) = validation {
            if matches!(
                failure.reason,
                "acceptance_check_unavailable" | "tool_missing"
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
                return Err(repair_limit_error(
                    failure,
                    state.accounting.validation_passes(),
                    state.calls.len(),
                ));
            }
            state.last_failure = Some(failure);
            pass += 1;
            continue;
        }
        state
            .progress
            .push(format!("shaper pass={pass} role={role} validation_passed"));
        let feasibility = if state
            .config
            .as_ref()
            .is_some_and(|config| config.raw["shaping"]["proof"].as_str() == Some("witness"))
        {
            let project = ProjectResolution::resolve(&CoreProjectOptions {
                cwd: Some(state.options.cwd.clone()),
                project: state.options.project.clone(),
                origin: state.options.origin.clone(),
                base: state.options.base.clone(),
                home: Some(state.options.home.clone()),
            })
            .map_err(project_error)?;
            let intent = fs::read(&state.intent_path)
                .map_err(|error| io_error("shape_output_unavailable", error))?;
            let acceptance = fs::read(&state.acceptance_path)
                .map_err(|error| io_error("shape_output_unavailable", error))?;
            let witness = crate::build::run_witness_build(
                &project,
                &state.options.slug,
                intent,
                state.acceptance_rel.clone(),
                acceptance,
            )?;
            state.warnings.extend(witness.warnings);
            if witness.proven {
                if state
                    .warnings
                    .iter()
                    .any(|warning| warning.code == "feasibility_concern")
                {
                    "PROVEN WITH CONCERNS".to_owned()
                } else {
                    "PROVEN".to_owned()
                }
            } else {
                "UNPROVEN".to_owned()
            }
        } else {
            "not checked".to_owned()
        };
        let rounds = state.accounting.validation_passes();
        return Ok(ShapeReport {
            intent_path: state.intent_path,
            acceptance_path: state.acceptance_path,
            transcript_path: state.transcript_path,
            rounds,
            feasibility,
            warnings: state.warnings,
            calls: state.calls,
            progress: state.progress,
        });
    }
    unreachable!("the six-pass loop returns or errors")
}

fn repair_kind(reason: &str) -> &'static str {
    match reason {
        "coverage_gap" => "coverage",
        "audit_over_strict" => "test_audit",
        "shape_turn_limit" => "turn_limit",
        _ => "validation",
    }
}

fn primary_turn_limit_fallback(
    pass: usize,
    error: &CoreError,
    last_failure: Option<ValidationFailure>,
) -> Option<ValidationFailure> {
    if pass > 3 || error.reason != "shape_turn_limit" {
        return None;
    }
    Some(last_failure.unwrap_or(ValidationFailure {
        reason: "shape_turn_limit",
        detail: error.detail.clone(),
    }))
}

pub(super) struct RunState<'a> {
    pub(super) options: ShapeOptions,
    pub(super) checkout: PathBuf,
    pub(super) state_root: PathBuf,
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
    accounting: &'a mut ShapeAccounting,
}

pub(super) enum PassResult {
    Complete,
    Failure(ValidationFailure),
    StyleRepair(Vec<String>),
}

impl RunState<'_> {
    fn record_feedback(&self, pass_index: usize, kind: &str, feedback: &str) {
        let value = super::super::journal::feedback_value(pass_index, kind, feedback);
        super::super::journal::append(&self.transcript_path, &value);
    }

    fn drive_shaper(
        &mut self,
        role: &str,
        model: &str,
        effort: &str,
        _pass: usize,
    ) -> Result<String, CoreError> {
        loop {
            let (conversation_id, turn_result) = {
                let session = self.session.as_mut().expect("shaper session exists");
                let conversation_id = session.conversation_id().to_owned();
                (conversation_id, self.provider.turn(session))
            };
            let turn = match turn_result {
                Ok(turn) => {
                    self.accounting.record_success(
                        &conversation_id,
                        role,
                        &turn.call.model,
                        &turn.call.effort,
                        turn.http_attempts,
                        &turn.call.usage,
                    );
                    turn
                }
                Err(failure) => {
                    self.accounting.record_failure(
                        &conversation_id,
                        role,
                        model,
                        effort,
                        failure.http_attempts,
                        failure.usage.as_ref(),
                    );
                    return Err(failure.error);
                }
            };
            self.record_call(&turn.call);
            if !turn.response.tool_calls.is_empty() {
                let _lock = CheckoutLock::acquire(&self.state_root, &self.checkout)?;
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
                self.accounting.finish_guard();
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
        let conversation_id = session.conversation_id().to_owned();
        self.accounting.record_session(
            &conversation_id,
            "auditor",
            "auditor",
            session.effective_model(),
            session.effective_effort(),
        );
        let turn = match self.provider.turn(&mut session) {
            Ok(turn) => {
                self.accounting.record_success(
                    &conversation_id,
                    "auditor",
                    &turn.call.model,
                    &turn.call.effort,
                    turn.http_attempts,
                    &turn.call.usage,
                );
                turn
            }
            Err(failure) => {
                self.accounting.record_failure(
                    &conversation_id,
                    "auditor",
                    session.effective_model(),
                    session.effective_effort(),
                    failure.http_attempts,
                    failure.usage.as_ref(),
                );
                return Err(failure.error);
            }
        };
        self.record_call(&turn.call);
        Ok(turn.response.text)
    }

    fn record_call(&mut self, call: &super::super::ShapeModelCall) {
        self.calls.push(call.clone());
        let value = call.transcript_value();
        if let Ok(mut file) = OpenOptions::new().append(true).open(&self.transcript_path) {
            let _ = writeln!(file, "{}", value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{primary_turn_limit_fallback, run};
    use crate::ExitCode;
    use crate::error::{CoreError, ErrorClass};
    use crate::intent::shaping::prompts;
    use crate::intent::shaping::runner::ShapeOptions;
    use crate::intent::shaping::validation::ValidationFailure;
    use serde_json::Value;
    use std::fs;

    #[test]
    fn primary_turn_limit_starts_fallback_and_keeps_the_last_validation_failure() {
        let turn_limit = CoreError::new(
            ErrorClass::Candidate,
            "shape_turn_limit",
            "Shaper exhausted its turn limit.",
            ExitCode::Negative,
        );
        let last_failure = ValidationFailure {
            reason: "undeclared_gate_path",
            detail: "Remove `.kogen/project.yaml` from the change list.".to_owned(),
        };
        let fallback = primary_turn_limit_fallback(2, &turn_limit, Some(last_failure.clone()))
            .expect("primary turn exhaustion starts fallback");
        let message = prompts::fallback_message(
            "initial shaping request",
            &prompts::validation_feedback(&fallback),
        );
        assert!(message.contains("Last validation failure:"));
        assert!(message.contains("candidate/undeclared_gate_path"));
        assert!(message.contains(".kogen/project.yaml"));

        let no_validation = primary_turn_limit_fallback(1, &turn_limit, None)
            .expect("fallback receives a turn-limit failure when validation never completed");
        assert_eq!(no_validation.reason, "shape_turn_limit");
        assert_eq!(no_validation.detail, "Shaper exhausted its turn limit.");
        assert!(primary_turn_limit_fallback(4, &turn_limit, None).is_none());

        let provider_error = CoreError::new(
            ErrorClass::Provider,
            "overload",
            "temporarily overloaded",
            ExitCode::Provider,
        );
        assert!(primary_turn_limit_fallback(2, &provider_error, None).is_none());
    }

    #[test]
    fn empty_request_failure_has_an_accounting_receipt() {
        let home = std::env::temp_dir().join(format!(
            "kogen-shape-exit-receipt-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let error = run(ShapeOptions {
            cwd: home.clone(),
            home: home.clone(),
            project: None,
            origin: None,
            base: None,
            slug: "receipt-test".to_owned(),
            request: b" \n".to_vec(),
        })
        .expect_err("empty request is rejected");
        assert_eq!(error.reason, "request_unavailable");

        let run_dir = fs::read_dir(home.join(".kogen/runs/shaping/receipt-test"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let receipt: Value =
            serde_json::from_slice(&fs::read(run_dir.join("shape-accounting.json")).unwrap())
                .unwrap();
        assert_eq!(receipt["outcome"], "failure");
        assert_eq!(receipt["terminal_reason"], "request_unavailable");
        assert_eq!(
            receipt["diagnostic"],
            "intent/request_unavailable: request is empty"
        );
        fs::remove_dir_all(home).unwrap();
    }
}

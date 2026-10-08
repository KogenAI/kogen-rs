//! Durable accounting for one Shape command.

use crate::error::CoreError;
use crate::provider::ModelUsage;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const PROFILE: &str = "shape-v1.3";

#[derive(Default)]
struct TokenTotals {
    input: Option<u64>,
    cached_input: Option<u64>,
    cache_write: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
}

impl TokenTotals {
    fn add(&mut self, usage: &ModelUsage) {
        add_count(&mut self.input, usage.input);
        add_count(&mut self.cached_input, usage.cached_input);
        add_count(&mut self.cache_write, usage.cache_write);
        add_count(&mut self.output, usage.output);
        add_count(&mut self.reasoning, usage.reasoning);
    }

    fn value(&self) -> Value {
        json!({
            "input": self.input,
            "cached_input": self.cached_input,
            "cache_write": self.cache_write,
            "output": self.output,
            "reasoning": self.reasoning,
        })
    }
}

#[derive(Default)]
struct RoleTotals {
    assigned_role: String,
    assigned_model: String,
    assigned_effort: String,
    effective_role: String,
    effective_model: String,
    effective_effort: String,
    logical_turns: usize,
    http_attempts: usize,
    tokens: TokenTotals,
}

struct Conversation {
    id: String,
    assigned_role: String,
    assigned_model: String,
    assigned_effort: String,
    effective_role: String,
    effective_model: String,
    effective_effort: String,
    logical_turns: usize,
    http_attempts: usize,
}

pub(super) struct ShapeAccounting {
    path: PathBuf,
    started: Instant,
    provider: Option<String>,
    roles: BTreeMap<String, RoleTotals>,
    conversations: Vec<Conversation>,
    tokens: TokenTotals,
    http_attempts: usize,
    unknown_usage_attempts: usize,
    validation_passes: usize,
    finish_guards: usize,
    repairs_by_kind: BTreeMap<String, usize>,
    outcome: Option<&'static str>,
    terminal_reason: Option<String>,
    diagnostic: Option<String>,
}

impl ShapeAccounting {
    pub(super) fn new(run_dir: &Path, started: Instant) -> Self {
        Self {
            path: run_dir.join("shape-accounting.json"),
            started,
            provider: None,
            roles: BTreeMap::new(),
            conversations: Vec::new(),
            tokens: TokenTotals::default(),
            http_attempts: 0,
            unknown_usage_attempts: 0,
            validation_passes: 0,
            finish_guards: 0,
            repairs_by_kind: BTreeMap::new(),
            outcome: None,
            terminal_reason: None,
            diagnostic: None,
        }
    }

    pub(super) fn set_provider(&mut self, provider: &str) {
        self.provider = Some(provider.to_owned());
    }

    pub(super) fn set_role(&mut self, role: &str, assigned_role: &str, model: &str, effort: &str) {
        let totals = self.roles.entry(role.to_owned()).or_default();
        totals.assigned_role = assigned_role.to_owned();
        totals.assigned_model = model.to_owned();
        totals.assigned_effort = effort.to_owned();
        totals.effective_role = assigned_role.to_owned();
        totals.effective_model = model.to_owned();
        totals.effective_effort = effort.to_owned();
    }

    pub(super) fn record_session(
        &mut self,
        id: &str,
        role: &str,
        assigned_role: &str,
        model: &str,
        effort: &str,
    ) {
        self.set_role(role, assigned_role, model, effort);
        self.conversations.push(Conversation {
            id: id.to_owned(),
            assigned_role: assigned_role.to_owned(),
            assigned_model: model.to_owned(),
            assigned_effort: effort.to_owned(),
            effective_role: role.to_owned(),
            effective_model: model.to_owned(),
            effective_effort: effort.to_owned(),
            logical_turns: 0,
            http_attempts: 0,
        });
    }

    pub(super) fn record_success(
        &mut self,
        id: &str,
        role: &str,
        model: &str,
        effort: &str,
        attempts: usize,
        usages: &[ModelUsage],
    ) {
        self.record_turn(id, role, model, effort, attempts, usages);
    }

    pub(super) fn record_failure(
        &mut self,
        id: &str,
        role: &str,
        model: &str,
        effort: &str,
        attempts: usize,
        usages: &[ModelUsage],
    ) {
        self.record_turn(id, role, model, effort, attempts, usages);
    }

    fn record_turn(
        &mut self,
        id: &str,
        role: &str,
        model: &str,
        effort: &str,
        attempts: usize,
        usages: &[ModelUsage],
    ) {
        let role_totals = self.roles.entry(role.to_owned()).or_default();
        role_totals.effective_role = role.to_owned();
        role_totals.effective_model = model.to_owned();
        role_totals.effective_effort = effort.to_owned();
        role_totals.http_attempts = role_totals.http_attempts.saturating_add(attempts);
        if attempts > 0 {
            role_totals.logical_turns = role_totals.logical_turns.saturating_add(1);
        }

        if let Some(conversation) = self.conversations.iter_mut().find(|item| item.id == id) {
            conversation.effective_role = role.to_owned();
            conversation.effective_model = model.to_owned();
            conversation.effective_effort = effort.to_owned();
            conversation.http_attempts = conversation.http_attempts.saturating_add(attempts);
            if attempts > 0 {
                conversation.logical_turns = conversation.logical_turns.saturating_add(1);
            }
        }

        self.http_attempts = self.http_attempts.saturating_add(attempts);
        for usage in usages.iter().take(attempts) {
            role_totals.tokens.add(usage);
            self.tokens.add(usage);
            if usage_is_unknown(usage) {
                self.unknown_usage_attempts = self.unknown_usage_attempts.saturating_add(1);
            }
        }
        self.unknown_usage_attempts = self
            .unknown_usage_attempts
            .saturating_add(attempts.saturating_sub(usages.len()));
    }

    pub(super) fn validation_pass(&mut self) {
        self.validation_passes = self.validation_passes.saturating_add(1);
    }

    pub(super) fn validation_passes(&self) -> usize {
        self.validation_passes
    }

    pub(super) fn finish_guard(&mut self) {
        self.finish_guards = self.finish_guards.saturating_add(1);
    }

    pub(super) fn repair(&mut self, kind: &str) {
        let count = self.repairs_by_kind.entry(kind.to_owned()).or_default();
        *count = count.saturating_add(1);
    }

    pub(super) fn finish(&mut self, result: &Result<impl Sized, CoreError>) {
        match result {
            Ok(_) => {
                self.outcome = Some("success");
                self.terminal_reason = Some("validated".to_owned());
                self.diagnostic = None;
            }
            Err(error) => {
                self.outcome = Some("failure");
                self.terminal_reason = Some(error.reason.clone());
                self.diagnostic = Some(error.render_stdout().trim_end().to_owned());
            }
        }
    }

    pub(super) fn write(&self) -> Result<(), CoreError> {
        let roles = self
            .roles
            .iter()
            .map(|(role, totals)| {
                (
                    role.clone(),
                    json!({
                        "assigned_role": totals.assigned_role,
                        "assigned_model": totals.assigned_model,
                        "assigned_effort": totals.assigned_effort,
                        "effective_role": totals.effective_role,
                        "effective_model": totals.effective_model,
                        "effective_effort": totals.effective_effort,
                        "logical_turns": totals.logical_turns,
                        "http_attempts": totals.http_attempts,
                        "tokens": totals.tokens.value(),
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let conversations = self
            .conversations
            .iter()
            .map(|conversation| {
                json!({
                    "conversation_id": conversation.id,
                    "assigned_role": conversation.assigned_role,
                    "assigned_model": conversation.assigned_model,
                    "assigned_effort": conversation.assigned_effort,
                    "effective_role": conversation.effective_role,
                    "effective_model": conversation.effective_model,
                    "effective_effort": conversation.effective_effort,
                    "logical_turns": conversation.logical_turns,
                    "http_attempts": conversation.http_attempts,
                })
            })
            .collect::<Vec<_>>();
        let value = json!({
            "schema": 1,
            "profile": PROFILE,
            "provider": self.provider,
            "outcome": self.outcome.unwrap_or("failure"),
            "conversation_ids": conversations.iter().map(|item| item["conversation_id"].clone()).collect::<Vec<_>>(),
            "conversations": conversations,
            "roles": roles,
            "http_attempts": {
                "total": self.http_attempts,
                "by_role": self.roles.iter().map(|(role, totals)| (role.clone(), json!(totals.http_attempts))).collect::<serde_json::Map<_, _>>(),
                "unknown_usage_attempts": self.unknown_usage_attempts,
            },
            "tokens": self.tokens.value(),
            "validation_passes": self.validation_passes,
            "finish_guards": self.finish_guards,
            "repairs_by_kind": self.repairs_by_kind,
            "elapsed_ms": self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            "terminal_reason": self.terminal_reason,
            "diagnostic": self.diagnostic,
        });
        super::files::write_json(&self.path, &value)?;
        super::files::secure_file(&self.path)
    }
}

fn add_count(total: &mut Option<u64>, count: Option<u64>) {
    if let Some(count) = count {
        *total = Some(total.unwrap_or_default().saturating_add(count));
    }
}

fn usage_is_unknown(usage: &ModelUsage) -> bool {
    usage.input.is_none()
        && usage.cached_input.is_none()
        && usage.cache_write.is_none()
        && usage.output.is_none()
        && usage.reasoning.is_none()
}

#[cfg(test)]
mod tests {
    use super::ShapeAccounting;
    use crate::ExitCode;
    use crate::error::{CoreError, ErrorClass};
    use crate::provider::ModelUsage;
    use serde_json::Value;
    use std::fs;
    use std::time::Instant;

    #[test]
    fn failure_receipt_keeps_counters_passes_reason_and_rendered_diagnostic() {
        let root = std::env::temp_dir().join(format!(
            "kogen-shape-accounting-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut accounting = ShapeAccounting::new(&root, Instant::now());
        accounting.set_provider("chatgpt");
        accounting.record_session("thread-1", "shaper", "shaper", "gpt-6.1-sol", "high");
        accounting.record_success(
            "thread-1",
            "shaper",
            "gpt-6.1-sol",
            "high",
            2,
            &[
                ModelUsage {
                    input: Some(3),
                    ..ModelUsage::default()
                },
                ModelUsage {
                    input: Some(10),
                    output: Some(4),
                    ..ModelUsage::default()
                },
            ],
        );
        accounting.record_failure(
            "thread-1",
            "shaper",
            "gpt-6.1-sol",
            "high",
            2,
            &[
                ModelUsage::default(),
                ModelUsage {
                    input: Some(5),
                    ..ModelUsage::default()
                },
            ],
        );
        accounting.validation_pass();
        accounting.repair("validation");
        let error = CoreError::new(
            ErrorClass::Candidate,
            "undeclared_gate_path",
            "Kogen project configuration is protected.\nRemove `.kogen/project.yaml` from the change list.",
            ExitCode::Negative,
        );
        accounting.finish(&Err::<(), _>(error));
        accounting.write().unwrap();

        let receipt: Value =
            serde_json::from_slice(&fs::read(root.join("shape-accounting.json")).unwrap()).unwrap();
        assert_eq!(receipt["schema"], 1);
        assert_eq!(receipt["profile"], "shape-v1.3");
        assert_eq!(receipt["outcome"], "failure");
        assert_eq!(receipt["validation_passes"], 1);
        assert_eq!(receipt["roles"]["shaper"]["logical_turns"], 2);
        assert_eq!(receipt["http_attempts"]["total"], 4);
        assert_eq!(receipt["http_attempts"]["unknown_usage_attempts"], 1);
        assert_eq!(receipt["tokens"]["input"], 18);
        assert_eq!(receipt["terminal_reason"], "undeclared_gate_path");
        assert_eq!(
            receipt["diagnostic"],
            "candidate/undeclared_gate_path: Kogen project configuration is protected.\n  Remove `.kogen/project.yaml` from the change list."
        );
        fs::remove_dir_all(root).unwrap();
    }
}

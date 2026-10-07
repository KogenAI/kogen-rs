//! Executable Shape allowances and attempt-level accounting, including failures.
use crate::provider::ModelUsage;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

pub(crate) type SharedAccounting = Rc<RefCell<Accounting>>;

#[derive(Default, Serialize)]
pub(crate) struct Accounting {
    conversations: Vec<Conversation>,
    roles: BTreeMap<String, Role>,
    http_attempts: usize,
    validation_passes: usize,
    pub finish_guards: usize,
    repairs: BTreeMap<String, usize>,
    tokens: BTreeMap<String, u64>,
    unknown_usage_attempts: usize,
}
#[derive(Serialize)]
struct Conversation {
    conversation_id: String,
    role: String,
    model: String,
    effort: String,
    logical_turns: usize,
    validation_passes: usize,
    style_repairs: usize,
}
#[derive(Default, Serialize)]
struct Role {
    logical_turns: usize,
    http_attempts: usize,
}
impl Accounting {
    pub fn start_conversation(&mut self, id: &str, role: &str, model: &str, effort: &str) {
        self.conversations.push(Conversation {
            conversation_id: id.to_owned(),
            role: role.to_owned(),
            model: model.to_owned(),
            effort: effort.to_owned(),
            logical_turns: 0,
            validation_passes: 0,
            style_repairs: 0,
        });
    }
    pub fn turn(&mut self, id: &str, role: &str) {
        self.roles.entry(role.to_owned()).or_default().logical_turns += 1;
        if let Some(conversation) = self
            .conversations
            .iter_mut()
            .find(|conversation| conversation.conversation_id == id)
        {
            conversation.logical_turns += 1;
        }
    }
    pub fn attempts(&mut self, role: &str, usages: &[ModelUsage]) {
        self.roles.entry(role.to_owned()).or_default().http_attempts += usages.len();
        self.http_attempts += usages.len();
        for usage in usages {
            let fields = [
                ("input", usage.input),
                ("cached_input", usage.cached_input),
                ("output", usage.output),
                ("reasoning", usage.reasoning),
            ];
            self.unknown_usage_attempts +=
                usize::from(fields.iter().any(|(_, value)| value.is_none()));
            for (key, value) in fields {
                *self.tokens.entry(key.to_owned()).or_default() += value.unwrap_or(0);
            }
        }
    }
    pub fn validation(&mut self) {
        self.validation_passes += 1;
        if let Some(conversation) = self.conversations.last_mut() {
            conversation.validation_passes += 1;
        }
    }
    pub fn repair(&mut self, kind: &str) {
        *self.repairs.entry(kind.to_owned()).or_default() += 1;
        if kind == "style"
            && let Some(conversation) = self.conversations.last_mut()
        {
            conversation.style_repairs += 1;
        }
    }
}

pub(super) struct Receipt {
    pub accounting: SharedAccounting,
    pub success: bool,
    pub path: PathBuf,
    pub started: Instant,
}
impl Receipt {
    pub fn publish(&self) -> Result<(), crate::error::CoreError> {
        let mut value = serde_json::to_value(&*self.accounting.borrow()).expect("Shape accounting");
        for key in ["input", "cached_input", "output", "reasoning"] {
            if value["tokens"].get(key).is_none() {
                value["tokens"][key] = 0.into();
            }
        }
        value["spec"] = "v1.3-draft".into();
        value["adapter"] = "kogen-responses-v1.3".into();
        value["prompt"] = "kogen-static-v1.3".into();
        value["schema"] = 1.into();
        value["profile"] = "shape-v1.3".into();
        value["outcome"] = if self.success { "success" } else { "failure" }.into();
        value["elapsed_ms"] = (self.started.elapsed().as_millis() as u64).into();
        let bytes = serde_json::to_vec(&value).expect("Shape accounting");
        crate::safe_fs::atomic_replace(
            self.path.parent().expect("receipt directory"),
            std::path::Path::new("shape-accounting.json"),
            &bytes,
        )
        .map_err(|error| super::files::io_error("shape_accounting_unavailable", error))
    }
}
impl Drop for Receipt {
    fn drop(&mut self) {
        if let Err(error) = self.publish() {
            eprintln!("{}: {}", error.reason, error.detail);
        }
    }
}

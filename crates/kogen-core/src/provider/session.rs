//! Build cache affinity, conversation identity, and append-only history.

pub mod replay;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::ModelToolCall;

pub const CONTINUATION_INSTRUCTION: &str = "The response stream was interrupted. Continue the same turn from the received progress above. Preserve its findings and constraints; do not restart the task or repeat completed work. Proposed tool calls above were not executed; reissue any still needed.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationBinding {
    pub run_dir: PathBuf,
    pub stage: String,
    pub attempt: String,
    pub rung: String,
    pub epoch: String,
}

impl ConversationBinding {
    #[must_use]
    pub fn new(run_dir: impl Into<PathBuf>, stage: impl Into<String>) -> Self {
        Self {
            run_dir: run_dir.into(),
            stage: stage.into(),
            attempt: "builder".to_owned(),
            rung: "builder".to_owned(),
            epoch: "initial".to_owned(),
        }
    }

    pub fn cache_key(&self) -> io::Result<String> {
        derive_cache_key(&self.run_dir)
    }

    pub fn thread_id(&self) -> io::Result<String> {
        derive_thread_id(self)
    }

    pub fn lite_session_id(&self) -> io::Result<String> {
        derive_lite_session_id(&self.run_dir)
    }
}

/// Stable run affinity. The expanded path is hashed, never sent on the wire.
pub fn derive_cache_key(run_dir: &Path) -> io::Result<String> {
    let run_dir = expanded_run_dir(run_dir)?;
    Ok(hash_fields(&[
        b"kogen:responses:cache:v3",
        run_dir.as_os_str().as_encoded_bytes(),
    ]))
}

/// Conversation identity changes with stage/attempt/rung/epoch, but not model.
pub fn derive_thread_id(binding: &ConversationBinding) -> io::Result<String> {
    let run_dir = expanded_run_dir(&binding.run_dir)?;
    Ok(hash_fields(&[
        b"kogen:responses:v2",
        run_dir.as_os_str().as_encoded_bytes(),
        binding.stage.as_bytes(),
        binding.attempt.as_bytes(),
        binding.rung.as_bytes(),
        binding.epoch.as_bytes(),
    ]))
}

/// Lite has a separate stable protocol session header by contract.
pub fn derive_lite_session_id(run_dir: &Path) -> io::Result<String> {
    let run_dir = expanded_run_dir(run_dir)?;
    Ok(hash_fields(&[
        b"kogen:responses:lite:v1",
        run_dir.as_os_str().as_encoded_bytes(),
    ]))
}

fn expanded_run_dir(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

fn hash_fields(fields: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            digest.update([0]);
        }
        digest.update(field);
    }
    format!("{:x}", digest.finalize())
}

/// Ordered request history. Existing items are never normalized or rewritten.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConversationHistory {
    items: Vec<Value>,
}

impl ConversationHistory {
    #[must_use]
    pub fn new(items: Vec<Value>) -> Self {
        Self { items }
    }

    #[must_use]
    pub fn items(&self) -> &[Value] {
        &self.items
    }

    pub fn append(&mut self, item: Value) {
        self.items.push(item);
    }

    pub fn append_all(&mut self, items: impl IntoIterator<Item = Value>) {
        self.items.extend(items);
    }

    pub fn append_user(&mut self, text: impl Into<String>) {
        self.items.push(json!({
            "role": "user",
            "content": [{"type": "input_text", "text": text.into()}]
        }));
    }

    pub fn append_tool_results(&mut self, results: &[ModelToolCall], outputs: &[String]) {
        for (call, output) in results.iter().zip(outputs) {
            self.items.push(json!({
                "type": "function_call_output",
                "call_id": call.id,
                "output": output,
            }));
        }
    }

    pub fn append_continuation(&mut self, received: impl IntoIterator<Item = Value>) {
        self.append_all(received);
        self.append_user(CONTINUATION_INSTRUCTION);
    }

    /// Keep prior items in place, removing only encrypted payloads that cannot
    /// be replayed to another model. Non-encrypted summaries remain available.
    pub fn discard_encrypted_reasoning(&mut self) {
        for item in &mut self.items {
            if item.get("type").and_then(Value::as_str) == Some("reasoning")
                && let Some(object) = item.as_object_mut()
            {
                object.remove("encrypted_content");
            }
        }
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&self.items)
    }
}

#[cfg(test)]
mod tests {
    use super::{ConversationBinding, ConversationHistory, derive_cache_key, derive_thread_id};
    use serde_json::json;

    #[test]
    fn build_affinity_is_stable_and_thread_id_changes_independently() {
        let dir = tempfile_dir();
        let first = ConversationBinding::new(&dir, "develop");
        let cache = derive_cache_key(&dir).unwrap();
        let thread = derive_thread_id(&first).unwrap();
        let mut second = first.clone();
        second.rung = "2".to_owned();
        assert_eq!(cache, derive_cache_key(&dir).unwrap());
        assert_ne!(thread, derive_thread_id(&second).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_appends_without_rewriting_and_continuation_uses_user_item() {
        let original = json!({"type":"reasoning","encrypted_content":"opaque"});
        let mut history = ConversationHistory::new(vec![original.clone()]);
        history.append_continuation([json!({"type":"message","content":[]})]);
        assert_eq!(history.items()[0], original);
        assert_eq!(history.items().len(), 3);
        assert_eq!(history.items()[2]["role"], "user");
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kogen-session-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

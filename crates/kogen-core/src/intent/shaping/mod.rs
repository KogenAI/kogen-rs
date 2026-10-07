//! Intent shaping policy and its production ChatGPT execution port.

mod audit;
mod commands;
mod journal;
mod prompts;
mod provider;
mod runner;
mod snapshot;
mod validation;

pub use runner::{ShapeOptions, ShapeReport, shape};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ShapeWarning {
    pub code: String,
    pub item_ids: Vec<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShapeModelCall {
    pub role: String,
    pub model: String,
    pub effort: String,
    pub usage: crate::provider::ModelUsage,
    pub wall_ms: u64,
    pub request: ShapeRequestJournal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShapeRequestJournal {
    pub cache_key: String,
    pub thread_id: String,
    pub endpoint_host: String,
    pub endpoint_path: String,
    pub routing_headers: Vec<String>,
    pub body_bytes: usize,
    pub body_prefix_sha256: Option<String>,
    pub previous_input_items: usize,
    pub previous_input_prefix_sha256: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
}

impl ShapeModelCall {
    pub(crate) fn transcript_value(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "model_call",
            "role": self.role,
            "model": self.model,
            "effort": self.effort,
            "usage": {
                "input": self.usage.input,
                "cached_input": self.usage.cached_input,
                "cache_write": self.usage.cache_write,
                "output": self.usage.output,
                "reasoning": self.usage.reasoning,
            },
            "wall_ms": self.wall_ms,
            "started_at_ms": self.request.started_at_ms,
            "ended_at_ms": self.request.ended_at_ms,
            "prompt_cache_key": self.request.cache_key,
            "cache_key": self.request.cache_key,
            "thread_id": self.request.thread_id,
            "conversation_id": self.request.thread_id,
            "endpoint_host": self.request.endpoint_host,
            "endpoint_path": self.request.endpoint_path,
            "routing_headers": self.request.routing_headers,
            "body_bytes": self.request.body_bytes,
            "body_prefix_sha256": self.request.body_prefix_sha256,
            "previous_input_items": self.request.previous_input_items,
            "previous_input_prefix_sha256": self.request.previous_input_prefix_sha256,
        })
    }
}

//! Intent shaping policy and its production ChatGPT execution port.

mod audit;
mod commands;
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
}

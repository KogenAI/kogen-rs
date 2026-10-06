//! Public core entry point for `intent shape`.

mod config;
mod execute;
mod files;
mod validate;

use super::ShapeWarning;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ShapeOptions {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub project: Option<PathBuf>,
    pub origin: Option<PathBuf>,
    pub base: Option<String>,
    pub slug: String,
    pub request: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ShapeReport {
    pub intent_path: PathBuf,
    pub acceptance_path: PathBuf,
    pub transcript_path: PathBuf,
    pub rounds: usize,
    pub warnings: Vec<ShapeWarning>,
    pub calls: Vec<super::ShapeModelCall>,
    pub progress: Vec<String>,
}

pub fn shape(options: ShapeOptions) -> Result<ShapeReport, crate::error::CoreError> {
    execute::run(options)
}

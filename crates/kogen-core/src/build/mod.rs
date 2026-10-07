//! Public queue dispatch and the pluggable single-rung Build core.

mod approval;
mod config;
mod interrupt;
mod provider;
mod provider_error;
mod provider_journal;
mod provider_prompt;
mod queue;
mod single_rung;
mod support;

pub use queue::{queue_start, queue_stop};

use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};

fn controller_error(reason: &str, detail: impl Into<String>) -> CoreError {
    CoreError::new(ErrorClass::Controller, reason, detail, ExitCode::Bug)
}

fn environment_error(reason: &str, detail: impl Into<String>) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        detail,
        ExitCode::Environment,
    )
}

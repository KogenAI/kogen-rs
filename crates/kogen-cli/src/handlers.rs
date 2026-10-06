use kogen_core::ExitCode;
use kogen_core::error::{CliOutput, CoreError, ErrorClass};

use crate::request::Command;

pub fn dispatch(command: Command) -> CliOutput {
    match command {
        Command::Version => CliOutput::success(version_line()),
        _ => CoreError::new(
            ErrorClass::Controller,
            "internal_error",
            "command handler is not available",
            ExitCode::Bug,
        )
        .into_cli_output(),
    }
}

fn version_line() -> String {
    let dirty = if env!("KOGEN_UNCOMMITTED") == "true" {
        ", uncommitted changes"
    } else {
        ""
    };
    format!(
        "kogen {} ({}{dirty})\n",
        env!("KOGEN_SOURCE_SHA"),
        env!("KOGEN_SOURCE_DATE")
    )
}

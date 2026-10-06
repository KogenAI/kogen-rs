use kogen_core::ExitCode;
use kogen_core::error::{CliOutput, CoreError, ErrorClass};
use kogen_core::provider::chatgpt;
use std::io::Write as _;
use std::path::Path;

use crate::request::Command;

pub fn dispatch(command: Command) -> CliOutput {
    match command {
        Command::Version => CliOutput::success(version_line()),
        Command::ProviderList => provider_output(with_home(chatgpt::list)),
        Command::ProviderLogin { provider } if provider == "chatgpt" => {
            let Some(home) = home_dir() else {
                return home_error();
            };
            let output = chatgpt::login(&home, |line| {
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(line.as_bytes());
                let _ = stdout.flush();
            });
            provider_output(output)
        }
        Command::ProviderLogout { provider } if provider == "chatgpt" => {
            provider_output(with_home(chatgpt::logout))
        }
        Command::ProviderUse {
            provider,
            label,
            project,
        } if provider == "chatgpt" => {
            let Some(home) = home_dir() else {
                return home_error();
            };
            provider_output(chatgpt::use_account(&home, &label, project.as_deref()))
        }
        Command::ProviderLogin { provider }
        | Command::ProviderLogout { provider }
        | Command::ProviderUse { provider, .. } => CoreError::new(
            ErrorClass::Provider,
            "unsupported_provider",
            format!("{provider} account commands are not available in this build"),
            ExitCode::Provider,
        )
        .into_cli_output(),
        _ => CoreError::new(
            ErrorClass::Controller,
            "internal_error",
            "command handler is not available",
            ExitCode::Bug,
        )
        .into_cli_output(),
    }
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}

fn home_error() -> CliOutput {
    CoreError::new(
        ErrorClass::Environment,
        "home_unavailable",
        "HOME is not set",
        ExitCode::Environment,
    )
    .into_cli_output()
}

fn with_home<T>(handler: impl FnOnce(&Path) -> Result<T, CoreError>) -> Result<T, CoreError> {
    let Some(home) = home_dir() else {
        return Err(CoreError::new(
            ErrorClass::Environment,
            "home_unavailable",
            "HOME is not set",
            ExitCode::Environment,
        ));
    };
    handler(&home)
}

fn provider_output(result: Result<String, CoreError>) -> CliOutput {
    match result {
        Ok(output) => CliOutput::success(output),
        Err(error) => error.into_cli_output(),
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

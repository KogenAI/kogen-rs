use kogen_core::ExitCode;
use kogen_core::error::{CliOutput, CoreError, ErrorClass};
use kogen_core::provider::chatgpt;
use kogen_core::provider::grok;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use crate::request::Command;

mod status;

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
        Command::ProviderLogin { provider } if provider == "grok" => {
            let Some(home) = home_dir() else {
                return home_error();
            };
            let output = grok::login(&home, |line| {
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(line.as_bytes());
                let _ = stdout.flush();
            });
            provider_output(output)
        }
        Command::ProviderLogout { provider } if provider == "chatgpt" => {
            provider_output(with_home(chatgpt::logout))
        }
        Command::ProviderLogout { provider } if provider == "grok" => {
            provider_output(with_home(grok::logout))
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
        Command::ProviderUse {
            provider,
            label,
            project,
        } if provider == "grok" => {
            let Some(home) = home_dir() else {
                return home_error();
            };
            provider_output(grok::use_account(&home, &label, project.as_deref()))
        }
        Command::IntentShape {
            slug,
            request,
            project,
        } => shape_output(shape_command(slug, request, project)),
        Command::ProviderLogin { provider }
        | Command::ProviderLogout { provider }
        | Command::ProviderUse { provider, .. } => CoreError::new(
            ErrorClass::Provider,
            "unsupported_provider",
            format!("{provider} account commands are not available in this build"),
            ExitCode::Provider,
        )
        .into_cli_output(),
        Command::IntentApprove {
            slug,
            hash,
            by,
            project,
        } => match resolve_project(project) {
            Ok(project) => {
                kogen_core::approval::approve(&project, &slug, hash.as_deref(), by.as_deref())
            }
            Err(error) => error.into_cli_output(),
        },
        Command::IntentRemove {
            slug,
            force,
            project,
        } => match resolve_project(project) {
            Ok(project) => kogen_core::approval::remove(&project, &slug, force),
            Err(error) => error.into_cli_output(),
        },
        Command::Status {
            slug,
            watch,
            json,
            project,
        } => match resolve_project(project) {
            Ok(project) => status::command(&project, slug.as_deref(), watch, json),
            Err(error) => error.into_cli_output(),
        },
        _ => CoreError::new(
            ErrorClass::Controller,
            "internal_error",
            "command handler is not available",
            ExitCode::Bug,
        )
        .into_cli_output(),
    }
}

fn shape_command(
    slug: String,
    request: String,
    project: crate::request::ProjectOptions,
) -> Result<kogen_core::intent::shaping::ShapeReport, CoreError> {
    let Some(home) = home_dir() else {
        return Err(CoreError::new(
            ErrorClass::Environment,
            "home_unavailable",
            "HOME is not set",
            ExitCode::Environment,
        ));
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (request_bytes, request_label) = if request == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes).map_err(|error| {
            CoreError::new(
                ErrorClass::Intent,
                "request_unavailable",
                format!("stdin: {error}"),
                ExitCode::Usage,
            )
        })?;
        (bytes, "stdin".to_owned())
    } else {
        let path = PathBuf::from(&request);
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        let bytes = std::fs::read(&path).map_err(|error| {
            CoreError::new(
                ErrorClass::Intent,
                "request_unavailable",
                format!("{}: {error}", path.display()),
                ExitCode::Usage,
            )
        })?;
        (bytes, path.display().to_string())
    };
    if request_is_whitespace(&request_bytes) {
        return Err(CoreError::new(
            ErrorClass::Intent,
            "request_unavailable",
            format!("{request_label}: empty"),
            ExitCode::Usage,
        ));
    }
    let crate::request::ProjectOptions {
        project: project_path,
        origin,
        base,
    } = project;
    kogen_core::intent::shaping::shape(kogen_core::intent::shaping::ShapeOptions {
        cwd,
        home,
        project: Some(project_path),
        origin,
        base,
        slug,
        request: request_bytes,
    })
}

fn shape_output(result: Result<kogen_core::intent::shaping::ShapeReport, CoreError>) -> CliOutput {
    let report = match result {
        Ok(report) => report,
        Err(error) => return error.into_cli_output(),
    };
    let mut stdout = format!(
        "Intent: {}\nAcceptance test: {}\nValidated after {} round(s).\nFeasibility: not checked\n",
        report.intent_path.display(),
        report.acceptance_path.display(),
        report.rounds,
    );
    if !report.warnings.is_empty() {
        stdout.push_str("Warnings\n");
        for warning in &report.warnings {
            let ids = if warning.item_ids.is_empty() {
                "-".to_owned()
            } else {
                warning.item_ids.join(", ")
            };
            stdout.push_str(&format!(
                "  - {}: {} — {}\n",
                warning.code, ids, warning.message
            ));
        }
    }
    for call in &report.calls {
        stdout.push_str(&format!(
            "shape {} {}/{} input={} cached={} output={} reasoning={} wall_ms={}\n",
            call.role,
            call.model,
            call.effort,
            optional_count(call.usage.input),
            optional_count(call.usage.cached_input),
            optional_count(call.usage.output),
            optional_count(call.usage.reasoning),
            call.wall_ms,
        ));
    }
    stdout.push_str(&format!(
        "Transcript: {}\nNext: kogen intent approve {}\n",
        report.transcript_path.display(),
        report
            .intent_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("intent"),
    ));
    CliOutput {
        stdout,
        stderr: report
            .progress
            .into_iter()
            .map(|line| format!("{line}\n"))
            .collect(),
        exit_code: ExitCode::Done,
    }
}

fn optional_count(count: Option<u64>) -> String {
    count.map_or_else(|| "null".to_owned(), |count| count.to_string())
}

fn request_is_whitespace(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).map_or_else(
        |_| bytes.iter().all(u8::is_ascii_whitespace),
        |text| text.chars().all(char::is_whitespace),
    )
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

fn resolve_project(
    options: crate::request::ProjectOptions,
) -> Result<kogen_core::project::ProjectResolution, CoreError> {
    let options = kogen_core::project::ProjectOptions {
        cwd: Some(options.project.clone()),
        project: Some(options.project),
        origin: options.origin,
        base: options.base,
        home: None,
    };
    kogen_core::project::ProjectResolution::resolve(&options).map_err(|error| match error {
        kogen_core::project::ProjectError::ProjectUnavailable(path) => CoreError::new(
            ErrorClass::Environment,
            "project_unavailable",
            path.display().to_string(),
            ExitCode::Environment,
        ),
        kogen_core::project::ProjectError::NotGitWorkTree(path) => CoreError::new(
            ErrorClass::Environment,
            "not_a_git_repo",
            path.display().to_string(),
            ExitCode::Environment,
        ),
        kogen_core::project::ProjectError::InvalidConfig(error) => CoreError::new(
            ErrorClass::Environment,
            "project_config_invalid",
            error.to_string(),
            ExitCode::Environment,
        ),
        kogen_core::project::ProjectError::BaseUnavailable(detail) => CoreError::new(
            ErrorClass::Environment,
            "base_unavailable",
            detail,
            ExitCode::Environment,
        ),
    })
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

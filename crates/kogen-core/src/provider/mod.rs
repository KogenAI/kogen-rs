//! Owned provider authentication and account selection.

pub mod accounts;
pub mod auth;
pub mod chatgpt;

use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};

/// A single provider and account choice retained for one Build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunAccount {
    pub provider: String,
    pub label: String,
    pub credential_source: &'static str,
}

/// Resolve provider and account once before a run starts. Request code should
/// retain this value and never search another label after an authentication
/// error.
pub fn resolve_run_account(
    home: &std::path::Path,
    project: &std::path::Path,
    committed_chatgpt_account: Option<&str>,
    bench_provider: Option<&str>,
    bench_account: Option<&str>,
    injected_auth: bool,
) -> Result<RunAccount, CoreError> {
    let account_file = accounts::read(home)?;
    let provider = accounts::select_provider(&account_file, project, bench_provider)?;
    let label = accounts::select_account(
        &account_file,
        &provider,
        project,
        bench_account,
        if provider == "chatgpt" {
            committed_chatgpt_account
        } else {
            None
        },
    )?;
    Ok(RunAccount {
        provider: provider.clone(),
        label,
        credential_source: if provider == "chatgpt" && injected_auth {
            "injected"
        } else {
            "owned"
        },
    })
}

pub(crate) fn provider_error(reason: &str, detail: impl Into<String>) -> CoreError {
    CoreError::new(ErrorClass::Provider, reason, detail, ExitCode::Provider)
}

pub(crate) fn environment_error(reason: &str, detail: impl Into<String>) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        detail,
        ExitCode::Environment,
    )
}

//! Owned provider authentication and account selection.

pub mod accounts;
pub mod auth;
pub mod cache;
pub mod chatgpt;
pub mod grok;
pub mod http;
pub mod session;
pub mod sse;
pub mod tools;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The classification used by request policy and the build journal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Login,
    UsageLimit,
    Overload,
    Timeout,
    Stall,
    Malformed,
    Transport,
    Incomplete,
    Unsupported,
}

impl ProviderErrorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::UsageLimit => "usage_limit",
            Self::Overload => "overload",
            Self::Timeout => "timeout",
            Self::Stall => "stall",
            Self::Malformed => "malformed",
            Self::Transport => "transport",
            Self::Incomplete => "incomplete",
            Self::Unsupported => "unsupported",
        }
    }
}

/// A provider failure. Optional usage remains explicitly null in journals.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderFailure {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub retry_after_ms: Option<u64>,
    pub usage: Option<Box<ModelUsage>>,
}

impl ProviderFailure {
    #[must_use]
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_after_ms: None,
            usage: None,
        }
    }
}

/// Token counts are optional independently; absence is not zero.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelUsage {
    pub input: Option<u64>,
    pub cached_input: Option<u64>,
    pub cache_write: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
}

impl ModelUsage {
    /// Convert Responses API token counts to uncached input and cached input.
    #[must_use]
    pub fn from_responses(
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cached_tokens: Option<u64>,
        cache_write_tokens: Option<u64>,
        reasoning_tokens: Option<u64>,
    ) -> Self {
        let cached_is_valid = match (input_tokens, cached_tokens) {
            (Some(input), Some(cached)) => cached <= input,
            _ => true,
        };
        let cached_input = cached_tokens.filter(|_| cached_is_valid);
        Self {
            input: input_tokens
                .zip(cached_input)
                .map(|(input, cached)| input - cached),
            cached_input,
            cache_write: cache_write_tokens,
            output: output_tokens,
            reasoning: reasoning_tokens,
        }
    }

    #[must_use]
    pub fn from_response_value(usage: Option<&Value>) -> Self {
        let Some(usage) = usage else {
            return Self::default();
        };
        Self::from_responses(
            usage.get("input_tokens").and_then(Value::as_u64),
            usage.get("output_tokens").and_then(Value::as_u64),
            usage
                .get("input_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64),
            usage.get("cache_write_tokens").and_then(Value::as_u64),
            usage
                .get("output_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(Value::as_u64),
        )
    }
}

/// Cache hit rate from records with both uncached and cached input counts.
/// Missing usage contributes no invented zeros; no complete count means null.
#[must_use]
pub fn cache_hit_rate(usages: &[ModelUsage]) -> Option<f64> {
    let (cached, denominator) = usages.iter().fold((0_u64, 0_u64), |totals, usage| {
        match (usage.input, usage.cached_input) {
            (Some(input), Some(cached)) => (
                totals.0.saturating_add(cached),
                totals.1.saturating_add(input).saturating_add(cached),
            ),
            _ => totals,
        }
    });
    (denominator > 0).then_some(cached as f64 / denominator as f64)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelResponse {
    pub id: String,
    pub text: String,
    pub tool_calls: Vec<ModelToolCall>,
    pub usage: ModelUsage,
    pub raw_items: Vec<Value>,
}

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

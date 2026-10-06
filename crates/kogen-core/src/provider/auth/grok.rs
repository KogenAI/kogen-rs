//! Grok device sign-in, private credential storage, and refresh.

use std::path::Path;

use serde::Deserialize;

use super::{GrokCredential, now_seconds};
use crate::provider::{ProviderErrorKind, ProviderFailure, provider_error};

#[path = "grok/oauth.rs"]
mod oauth;
#[path = "grok/vault.rs"]
mod vault;
pub(crate) use vault::{delete, get};
use vault::{missing_login, put, unavailable_refresh, validate_label};

#[derive(Debug, Deserialize)]
pub(super) struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

pub(crate) fn login(
    home: &Path,
    progress: impl FnMut(&str),
) -> Result<GrokCredential, crate::error::CoreError> {
    oauth::login(home, progress)
}

pub(super) fn credential_for_request(
    home: &Path,
    label: &str,
) -> Result<GrokCredential, crate::error::CoreError> {
    let credential = get(home, label)?.ok_or_else(missing_login)?;
    if credential.access_token.is_empty()
        || !oauth::valid_saved_endpoint(&credential.token_endpoint)
    {
        return Err(missing_login());
    }
    if credential.expires_at <= now_seconds().saturating_add(300) {
        return refresh(home, label, None);
    }
    Ok(credential)
}

pub(super) fn refresh_after_401(
    home: &Path,
    label: &str,
    rejected: &GrokCredential,
) -> Result<GrokCredential, ProviderFailure> {
    refresh(home, label, Some(&rejected.access_token))
        .map_err(|error| refresh_failure(&error.detail))
}

fn refresh(
    home: &Path,
    label: &str,
    rejected_access_token: Option<&str>,
) -> Result<GrokCredential, crate::error::CoreError> {
    refresh_with(
        home,
        label,
        rejected_access_token,
        std::env::var_os("KOGEN_AUTH_URL").is_some(),
    )
}

fn refresh_with(
    home: &Path,
    label: &str,
    rejected_access_token: Option<&str>,
    allow_local_auth: bool,
) -> Result<GrokCredential, crate::error::CoreError> {
    validate_label(label)?;
    let _guard = super::refresh::acquire_lock(home, "grok", label)?;
    let current = get(home, label)?.ok_or_else(missing_login)?;
    if rejected_access_token.is_some_and(|rejected| current.access_token != rejected) {
        return Ok(current);
    }
    if rejected_access_token.is_none() && current.expires_at > now_seconds().saturating_add(300) {
        return Ok(current);
    }
    if current.refresh_token.is_empty() {
        return Err(unavailable_refresh());
    }
    oauth::validate_auth_endpoint(&current.token_endpoint, allow_local_auth)
        .map_err(|_| unavailable_refresh())?;
    let client = oauth::auth_client().map_err(|_| unavailable_refresh())?;
    let response = client
        .post(&current.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", current.client_id.as_str()),
            ("refresh_token", current.refresh_token.as_str()),
        ])
        .send()
        .map_err(refresh_network_error)?;
    if !response.status().is_success() {
        return Err(unavailable_refresh());
    }
    let tokens: TokenResponse = response.json().map_err(|_| unavailable_refresh())?;
    if tokens.access_token.is_empty() || tokens.expires_in == 0 {
        return Err(unavailable_refresh());
    }
    let updated = GrokCredential {
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .filter(|token| !token.is_empty())
            .unwrap_or_else(|| current.refresh_token.clone()),
        expires_at: now_seconds().saturating_add(tokens.expires_in.min(i64::MAX as u64) as i64),
        scopes: tokens
            .scope
            .map(|scope| scope.split_ascii_whitespace().map(str::to_owned).collect())
            .unwrap_or_else(|| current.scopes.clone()),
        email: tokens
            .email
            .or_else(|| tokens.id_token.as_deref().and_then(email_claim))
            .or(current.email),
        ..current
    };
    put(home, label, &updated)?;
    crate::provider::grok::record_refresh(home, label, &updated)?;
    Ok(updated)
}

fn refresh_network_error(error: reqwest::Error) -> crate::error::CoreError {
    if error.is_timeout() {
        provider_error("login", "Grok session refresh timed out.")
    } else if error.is_connect() {
        provider_error(
            "login",
            "Grok session could not refresh. Check the network and sign in again.",
        )
    } else {
        unavailable_refresh()
    }
}

fn refresh_failure(detail: &str) -> ProviderFailure {
    let message = match detail {
        "Grok session refresh timed out."
        | "Grok session could not refresh. Check the network and sign in again." => detail,
        _ => "Grok login is unavailable; run `kogen provider login grok`.",
    };
    ProviderFailure::new(ProviderErrorKind::Login, message)
}

fn email_claim(token: &str) -> Option<String> {
    super::unverified_claims(token)?
        .get("email")?
        .as_str()
        .map(str::to_owned)
}

fn invalid_token_response() -> crate::error::CoreError {
    provider_error("login", "Grok returned an invalid sign-in token response.")
}

#[cfg(test)]
#[path = "grok/tests.rs"]
mod tests;

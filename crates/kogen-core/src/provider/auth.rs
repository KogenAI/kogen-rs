//! Per-request ChatGPT credentials, including the injected JWT test adapter.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ProviderFailure, RunAccount, provider_error};

#[path = "auth/grok.rs"]
pub(crate) mod grok;
#[path = "auth/jwt.rs"]
mod jwt;
#[path = "auth/oauth.rs"]
pub(super) mod oauth;
#[path = "auth/refresh.rs"]
mod refresh;
#[path = "auth/store.rs"]
mod store;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Credential {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: i64,
    pub scopes: Vec<String>,
    pub subject: String,
    pub email: Option<String>,
    pub host_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InjectedCredential {
    pub access_token: String,
    pub account_id: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct GrokCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub scopes: Vec<String>,
    pub email: Option<String>,
    pub client_id: String,
    pub token_endpoint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestCredential {
    Owned(Credential),
    Grok(GrokCredential),
    Injected(InjectedCredential),
}

impl RequestCredential {
    #[must_use]
    pub fn access_token(&self) -> &str {
        match self {
            Self::Owned(credential) => &credential.access_token,
            Self::Grok(credential) => &credential.access_token,
            Self::Injected(credential) => &credential.access_token,
        }
    }

    #[must_use]
    pub fn account_id(&self) -> Option<String> {
        match self {
            Self::Owned(credential) => account_id_from_id_token(&credential.id_token),
            Self::Grok(_) => None,
            Self::Injected(credential) => Some(credential.account_id.clone()),
        }
    }
}

/// Load credentials for one request. Injected auth is re-read from disk on
/// every call; owned credentials refresh under their per-label lock.
pub fn credential_for_request(
    home: &Path,
    account: &RunAccount,
) -> Result<RequestCredential, super::CoreError> {
    let injected_path = std::env::var_os("KOGEN_AUTH_PATH").map(std::path::PathBuf::from);
    credential_for_request_with_injected_path(home, account, injected_path.as_deref())
}

/// Load credentials through Kogen's normal request-authentication path, with
/// an optional injected ChatGPT auth file supplied explicitly by the caller.
/// When `injected_path` is `None`, this function selects the owned login and
/// does not consult `KOGEN_AUTH_PATH`.
pub fn credential_for_request_with_injected_path(
    home: &Path,
    account: &RunAccount,
    injected_path: Option<&Path>,
) -> Result<RequestCredential, super::CoreError> {
    match account.provider.as_str() {
        "chatgpt" => {
            if let Some(path) = injected_path {
                return read_injected(path).map(RequestCredential::Injected);
            }
            let credential = store::get(home, &account.label)?.ok_or_else(|| {
                provider_error(
                    "login",
                    format!(
                        "Selected account {} has no saved login; run kogen provider login chatgpt to sign in",
                        account.label
                    ),
                )
            })?;
            if credential.expires_at <= now_seconds().saturating_add(300) {
                return refresh::refresh(home, &account.label, None).map(RequestCredential::Owned);
            }
            Ok(RequestCredential::Owned(credential))
        }
        "grok" => grok::credential_for_request(home, &account.label).map(RequestCredential::Grok),
        _ => Err(provider_error(
            "unsupported_provider",
            "unsupported provider credentials requested",
        )),
    }
}

/// Refresh after an API 401 only when this is still the token the server
/// rejected. Callers replay the request once with the returned token.
pub fn refresh_after_401(
    home: &Path,
    label: &str,
    rejected_access_token: &str,
) -> Result<Credential, super::CoreError> {
    refresh::refresh(home, label, Some(rejected_access_token))
}

pub(crate) fn refresh_request_after_401(
    home: &Path,
    label: &str,
    credential: &RequestCredential,
) -> Result<Option<RequestCredential>, ProviderFailure> {
    match credential {
        RequestCredential::Owned(current) => {
            refresh::refresh(home, label, Some(&current.access_token))
                .map(RequestCredential::Owned)
                .map(Some)
                .or(Ok(None))
        }
        RequestCredential::Grok(current) => grok::refresh_after_401(home, label, current)
            .map(RequestCredential::Grok)
            .map(Some),
        RequestCredential::Injected(_) => Ok(None),
    }
}

pub fn read_injected(path: &Path) -> Result<InjectedCredential, super::CoreError> {
    let bytes =
        fs::read(path).map_err(|_| provider_error("login", "injected auth is unavailable"))?;
    let doc: Value = serde_json::from_slice(&bytes)
        .map_err(|_| provider_error("login", "injected auth is invalid"))?;
    let token = doc
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| provider_error("login", "injected auth is invalid"))?;
    let account_id = doc
        .get("tokens")
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| provider_error("login", "injected auth is invalid"))?;
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| provider_error("login", "injected access token has no expiration"))?;
    let claims_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| provider_error("login", "injected access token is invalid"))?;
    let claims: Value = serde_json::from_slice(&claims_bytes)
        .map_err(|_| provider_error("login", "injected access token is invalid"))?;
    let expires_at = claims
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or_else(|| provider_error("login", "injected access token has no expiration"))?;
    if expires_at <= now_seconds() {
        return Err(provider_error("login", "injected access token has expired"));
    }
    Ok(InjectedCredential {
        access_token: token.to_owned(),
        account_id: account_id.to_owned(),
        expires_at,
    })
}

pub(crate) fn login_owned(
    home: &Path,
    previous_client_id: Option<&str>,
    progress: impl FnMut(&str),
) -> Result<(Credential, String, Option<String>, Option<String>), super::CoreError> {
    let result = oauth::login(home, previous_client_id, progress)?;
    Ok((
        result.credential,
        result.identity.subject,
        result.identity.email,
        result.identity.plan_usage,
    ))
}

#[cfg(test)]
pub(crate) fn login_owned_with_browser(
    home: &Path,
    previous_client_id: Option<&str>,
    progress: impl FnMut(&str),
    auth_url: Option<&str>,
    callback_port: u16,
    browser: impl FnMut(&str) -> Result<(), super::CoreError>,
) -> Result<(Credential, String, Option<String>, Option<String>), super::CoreError> {
    let result = oauth::login_with_browser(
        home,
        previous_client_id,
        progress,
        auth_url,
        callback_port,
        browser,
    )?;
    Ok((
        result.credential,
        result.identity.subject,
        result.identity.email,
        result.identity.plan_usage,
    ))
}

pub(crate) fn revoke_owned(credential: &Credential) -> bool {
    oauth::revoke(credential)
}

pub(crate) fn get_login_credential(
    home: &Path,
    label: &str,
) -> Result<store::LoginCredential<Credential>, super::CoreError> {
    store::get_for_login(home, "chatgpt", label)
}

pub(crate) fn put_login_credential(
    home: &Path,
    label: &str,
    credential: &Credential,
) -> Result<(), super::CoreError> {
    store::put_for_login(home, "chatgpt", label, credential)
}

pub(crate) fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(crate) fn account_id_from_id_token(id_token: &str) -> Option<String> {
    let claims = unverified_claims(id_token)?;
    claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(str::to_owned)
}

pub(crate) fn unverified_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

pub(crate) use store::{delete as delete_credential, get as get_credential, put as put_credential};

#[cfg(test)]
mod tests {
    use std::path::Path;

    use base64::Engine as _;

    use super::{RequestCredential, credential_for_request_with_injected_path, read_injected};
    use crate::provider::RunAccount;

    #[test]
    fn rejects_expired_injected_auth_without_network_access() {
        let path = std::env::temp_dir().join(format!(
            "kogen-expired-auth-{}-{}.json",
            std::process::id(),
            super::now_seconds()
        ));
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!("{{\"exp\":{}}}", super::now_seconds() - 1));
        let document = format!(
            "{{\"tokens\":{{\"access_token\":\"a.{encoded}.s\",\"account_id\":\"acct\"}}}}"
        );
        std::fs::write(&path, document).unwrap();
        let result = read_injected(Path::new(&path));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(result.unwrap_err().reason, "login");
    }

    #[test]
    fn explicit_injected_path_uses_kogens_request_authentication_path() {
        let path = std::env::temp_dir().join(format!(
            "kogen-explicit-auth-{}-{}.json",
            std::process::id(),
            super::now_seconds()
        ));
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!("{{\"exp\":{}}}", super::now_seconds() + 3600));
        let document = format!(
            "{{\"tokens\":{{\"access_token\":\"a.{encoded}.s\",\"account_id\":\"acct\"}}}}"
        );
        std::fs::write(&path, document).unwrap();
        let account = RunAccount {
            provider: "chatgpt".to_owned(),
            label: "default".to_owned(),
            credential_source: "injected",
        };

        let result = credential_for_request_with_injected_path(
            Path::new("/unused-home"),
            &account,
            Some(&path),
        );
        std::fs::remove_file(&path).unwrap();

        assert!(matches!(result, Ok(RequestCredential::Injected(_))));
    }
}

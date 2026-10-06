//! OpenID discovery, ChatGPT PKCE login, token refresh and revocation.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use super::super::provider_error;
use super::jwt::{Identity, verify_id_token};
use super::{Credential, now_seconds};

const REQUIRED_SCOPE: &str = "chatgpt.tokens.use.direct";
const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

#[path = "callback.rs"]
mod callback;
#[path = "local.rs"]
mod local;

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    id_token: String,
    expires_in: u64,
    scope: String,
}

pub(super) struct LoginResult {
    pub credential: Credential,
    pub identity: Identity,
}

pub(super) fn login(
    home: &Path,
    previous_client_id: Option<&str>,
    mut progress: impl FnMut(&str),
) -> Result<LoginResult, super::super::CoreError> {
    let client = http_client()?;
    let discovery = discover(&client)?;
    let host_id = local::host_id(home)?;
    let client_id = previous_client_id.unwrap_or("dynamic_agent_client");
    let (verifier, challenge, state, nonce) = local::pkce_values();
    let mut authorize = Url::parse(&discovery.authorization_endpoint)
        .map_err(|_| provider_error("login", "ChatGPT authorization endpoint is invalid"))?;
    {
        let mut query = authorize.query_pairs_mut();
        query.append_pair("client_id", client_id);
        query.append_pair("ext_agent_host_id", &host_id);
        query.append_pair("response_type", "code");
        query.append_pair("redirect_uri", REDIRECT_URI);
        query.append_pair("scope", SCOPE);
        query.append_pair("resource", "https://api.openai.com/v1");
        query.append_pair("state", &state);
        query.append_pair("nonce", &nonce);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("code_challenge", &challenge);
        if client_id == "dynamic_agent_client" {
            query.append_pair("agent_name_hint", "Kogen");
        }
    }
    let listener = callback::bind()?;
    progress("Continue with ChatGPT\n");
    progress(&format!("{}\n", authorize.as_str()));
    open_browser(authorize.as_str())?;
    let returned = callback::wait(listener, &state, local::login_wait())?;
    let code = returned.code.ok_or_else(|| {
        provider_error(
            "login",
            "ChatGPT callback did not include an authorization code",
        )
    })?;
    let returned_client_id = returned.client_id.as_deref().unwrap_or(client_id);
    let response = client
        .post(&discovery.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", returned_client_id),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .map_err(|_| provider_error("login", "ChatGPT token exchange failed"))?;
    let tokens = token_response(response)?;
    let jwks = get_json(&client, &discovery.jwks_uri)?;
    let identity = verify_id_token(&tokens.id_token, &jwks, returned_client_id, Some(&nonce))?;
    require_scope(&tokens.scope)?;
    let expires_at = now_seconds().saturating_add(tokens.expires_in as i64);
    let credential = Credential {
        client_id: returned_client_id.to_owned(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token.unwrap_or_default(),
        id_token: tokens.id_token,
        expires_at,
        scopes: split_scopes(&tokens.scope),
        subject: identity.subject.clone(),
        email: identity.email.clone(),
        host_id,
    };
    Ok(LoginResult {
        credential,
        identity,
    })
}

pub(super) fn refresh(
    credential: &Credential,
) -> Result<(Credential, Identity), super::super::CoreError> {
    let client = http_client()?;
    let discovery = discover(&client)?;
    let response = client
        .post(&discovery.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", credential.client_id.as_str()),
            ("refresh_token", credential.refresh_token.as_str()),
        ])
        .send()
        .map_err(|_| provider_error("login", "ChatGPT token refresh failed"))?;
    let tokens = token_response(response)?;
    let jwks = get_json(&client, &discovery.jwks_uri)?;
    let identity = verify_id_token(&tokens.id_token, &jwks, &credential.client_id, None)?;
    if identity.subject != credential.subject {
        return Err(provider_error(
            "login",
            "ChatGPT account subject changed during refresh",
        ));
    }
    require_scope(&tokens.scope)?;
    let updated = Credential {
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .unwrap_or_else(|| credential.refresh_token.clone()),
        id_token: tokens.id_token,
        expires_at: now_seconds().saturating_add(tokens.expires_in as i64),
        scopes: split_scopes(&tokens.scope),
        email: identity.email.clone().or_else(|| credential.email.clone()),
        ..credential.clone()
    };
    Ok((updated, identity))
}

pub(super) fn revoke(credential: &Credential) -> bool {
    let Ok(client) = Client::builder().timeout(Duration::from_secs(20)).build() else {
        return false;
    };
    let Ok(discovery) = discover(&client) else {
        return false;
    };
    let Some(endpoint) = discovery.revocation_endpoint else {
        return false;
    };
    client
        .post(endpoint)
        .form(&[
            ("token", credential.refresh_token.as_str()),
            ("token_type_hint", "refresh_token"),
            ("client_id", credential.client_id.as_str()),
        ])
        .send()
        .is_ok_and(|response| response.status().is_success())
}

fn discover(client: &Client) -> Result<Discovery, super::super::CoreError> {
    let base =
        std::env::var("KOGEN_AUTH_URL").unwrap_or_else(|_| "https://auth.openai.com".to_owned());
    let url = format!(
        "{}/.well-known/openid-configuration",
        base.trim_end_matches('/')
    );
    let response = client
        .get(url)
        .send()
        .map_err(|_| provider_error("login", "ChatGPT OpenID discovery failed"))?;
    let config: Discovery = json_response(response)?;
    if config.issuer != "https://auth.openai.com" {
        return Err(provider_error("login", "ChatGPT OpenID issuer is invalid"));
    }
    validate_endpoint(&config.authorization_endpoint)?;
    validate_endpoint(&config.token_endpoint)?;
    validate_endpoint(&config.jwks_uri)?;
    if let Some(endpoint) = &config.revocation_endpoint {
        validate_endpoint(endpoint)?;
    }
    Ok(config)
}

fn validate_endpoint(endpoint: &str) -> Result<(), super::super::CoreError> {
    let url = Url::parse(endpoint)
        .map_err(|_| provider_error("login", "ChatGPT OpenID endpoint is invalid"))?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(provider_error(
            "login",
            "ChatGPT OpenID endpoint is invalid",
        ));
    }
    if url.scheme() == "http" && std::env::var_os("KOGEN_AUTH_URL").is_none() {
        return Err(provider_error(
            "login",
            "ChatGPT OpenID endpoint must use HTTPS",
        ));
    }
    Ok(())
}

fn get_json(client: &Client, url: &str) -> Result<Value, super::super::CoreError> {
    let response = client
        .get(url)
        .send()
        .map_err(|_| provider_error("login", "ChatGPT OpenID request failed"))?;
    json_response(response)
}

fn json_response<T: serde::de::DeserializeOwned>(
    response: Response,
) -> Result<T, super::super::CoreError> {
    if !response.status().is_success() {
        return Err(provider_error("login", "ChatGPT OpenID request failed"));
    }
    response
        .json()
        .map_err(|_| provider_error("login", "ChatGPT OpenID response is invalid"))
}

fn token_response(response: Response) -> Result<TokenResponse, super::super::CoreError> {
    if !response.status().is_success() {
        return Err(provider_error(
            "login",
            "ChatGPT token request was rejected",
        ));
    }
    json_response(response)
}

fn require_scope(scope: &str) -> Result<(), super::super::CoreError> {
    if scope
        .split_ascii_whitespace()
        .any(|item| item == REQUIRED_SCOPE)
    {
        Ok(())
    } else {
        Err(provider_error(
            "login",
            "ChatGPT token lacks chatgpt.tokens.use.direct",
        ))
    }
}

fn split_scopes(scope: &str) -> Vec<String> {
    scope.split_ascii_whitespace().map(str::to_owned).collect()
}

fn open_browser(url: &str) -> Result<(), super::super::CoreError> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| provider_error("login", "could not open a browser for ChatGPT sign-in"))
}

fn http_client() -> Result<Client, super::super::CoreError> {
    Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| provider_error("login", "could not initialize ChatGPT authentication"))
}

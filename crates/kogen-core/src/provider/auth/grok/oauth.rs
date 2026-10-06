//! xAI OpenID discovery and device-code polling.

use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::Deserialize;
use url::Url;

use super::{GrokCredential, TokenResponse, email_claim, invalid_token_response, now_seconds, put};
use crate::provider::provider_error;

const ISSUER: &str = "https://auth.x.ai";
const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, Deserialize)]
struct Discovery {
    issuer: String,
    token_endpoint: String,
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
}

pub(crate) fn login(
    home: &std::path::Path,
    mut progress: impl FnMut(&str),
) -> Result<GrokCredential, crate::error::CoreError> {
    let client = auth_client()?;
    let discovery = discover(&client)?;
    let issuer = Url::parse(&discovery.issuer).map_err(|_| invalid_discovery())?;
    let device_endpoint = discovery.device_authorization_endpoint.unwrap_or_else(|| {
        format!(
            "{}/oauth2/device/code",
            issuer.as_str().trim_end_matches('/')
        )
    });
    let allow_local_http = std::env::var_os("KOGEN_AUTH_URL").is_some();
    validate_auth_endpoint(&device_endpoint, allow_local_http)?;
    validate_auth_endpoint(&discovery.token_endpoint, allow_local_http)?;

    let response = client
        .post(&device_endpoint)
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])
        .send()
        .map_err(sign_in_network_error)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(provider_error(
            "login",
            format!("Grok device sign-in failed (HTTP {status})."),
        ));
    }
    let device: DeviceResponse = response.json().map_err(|_| invalid_device_response())?;
    if device.device_code.is_empty() || device.user_code.is_empty() || device.expires_in == 0 {
        return Err(invalid_device_response());
    }
    let open_url = device
        .verification_uri_complete
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            device
                .verification_uri
                .as_deref()
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(invalid_device_response)?;
    if !valid_verification_uri(open_url) {
        return Err(invalid_device_response());
    }
    progress(&format!("Grok sign-in code: {}\n", device.user_code));
    progress(&format!("Open: {open_url}\n"));

    let tokens = poll_device(&client, &discovery.token_endpoint, &device)?;
    if tokens.access_token.is_empty()
        || tokens.refresh_token.as_deref().is_none_or(str::is_empty)
        || tokens.expires_in == 0
    {
        return Err(invalid_token_response());
    }
    let email = tokens
        .email
        .or_else(|| tokens.id_token.as_deref().and_then(email_claim));
    let scope = tokens.scope.unwrap_or_else(|| SCOPE.to_owned());
    let credential = GrokCredential {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token.unwrap_or_default(),
        expires_at: now_seconds().saturating_add(tokens.expires_in.min(i64::MAX as u64) as i64),
        scopes: scope.split_ascii_whitespace().map(str::to_owned).collect(),
        email,
        client_id: CLIENT_ID.to_owned(),
        token_endpoint: discovery.token_endpoint,
    };
    put(home, "default", &credential)?;
    Ok(credential)
}

fn poll_device(
    client: &Client,
    token_endpoint: &str,
    device: &DeviceResponse,
) -> Result<TokenResponse, crate::error::CoreError> {
    let start = Instant::now();
    let deadline = scaled_duration(device.expires_in.saturating_mul(1000));
    let mut interval = device.interval.unwrap_or(5).max(1);
    loop {
        let wait = scaled_duration(interval.saturating_mul(1000));
        if start.elapsed().saturating_add(wait) >= deadline || start.elapsed() >= deadline {
            return Err(expired_code());
        }
        thread::sleep(wait);
        if start.elapsed() >= deadline {
            return Err(expired_code());
        }
        let response = client
            .post(token_endpoint)
            .form(&[
                ("grant_type", DEVICE_GRANT),
                ("device_code", device.device_code.as_str()),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .map_err(sign_in_network_error)?;
        if start.elapsed() >= deadline {
            return Err(expired_code());
        }
        if response.status().is_success() {
            return response
                .json::<TokenResponse>()
                .map_err(|_| invalid_token_response());
        }
        let status = response.status().as_u16();
        let body = response.json::<OAuthError>().ok();
        match body.as_ref().map(|error| error.error.as_str()) {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval = interval.saturating_add(5);
                continue;
            }
            Some("expired_token") => return Err(expired_code()),
            Some("access_denied") => {
                return Err(provider_error("login", "Grok sign-in was cancelled."));
            }
            _ => {
                return Err(provider_error(
                    "login",
                    format!("Grok sign-in polling failed (HTTP {status})."),
                ));
            }
        }
    }
}

fn discover(client: &Client) -> Result<Discovery, crate::error::CoreError> {
    let issuer = std::env::var("KOGEN_AUTH_URL").unwrap_or_else(|_| ISSUER.to_owned());
    let base = Url::parse(&issuer).map_err(|_| invalid_endpoint())?;
    let allow_local_http = std::env::var_os("KOGEN_AUTH_URL").is_some();
    if !valid_endpoint(&base, allow_local_http) {
        return Err(invalid_endpoint());
    }
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let response = client.get(url).send().map_err(sign_in_network_error)?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(provider_error(
            "login",
            format!("Grok sign-in discovery failed (HTTP {status})."),
        ));
    }
    let discovery: Discovery = response.json().map_err(|_| invalid_discovery())?;
    let doc_issuer = Url::parse(&discovery.issuer).map_err(|_| invalid_discovery())?;
    if doc_issuer.as_str().trim_end_matches('/') != ISSUER || discovery.token_endpoint.is_empty() {
        return Err(invalid_discovery());
    }
    Ok(discovery)
}

pub(super) fn auth_client() -> Result<Client, crate::error::CoreError> {
    Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| provider_error("login", "Grok sign-in timed out."))
}

pub(super) fn validate_auth_endpoint(
    endpoint: &str,
    allow_local_http: bool,
) -> Result<(), crate::error::CoreError> {
    let url = Url::parse(endpoint).map_err(|_| invalid_endpoint())?;
    if valid_endpoint(&url, allow_local_http) {
        Ok(())
    } else {
        Err(invalid_endpoint())
    }
}

pub(super) fn valid_saved_endpoint(endpoint: &str) -> bool {
    let allow_local_http = std::env::var_os("KOGEN_AUTH_URL").is_some();
    Url::parse(endpoint).is_ok_and(|url| valid_endpoint(&url, allow_local_http))
}

fn valid_endpoint(url: &Url, allow_local_http: bool) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" || !allow_local_http {
        return false;
    }
    matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

fn valid_verification_uri(uri: &str) -> bool {
    Url::parse(uri).is_ok_and(|url| {
        matches!(url.scheme(), "https" | "http")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn sign_in_network_error(error: reqwest::Error) -> crate::error::CoreError {
    if error.is_timeout() {
        provider_error("login", "Grok sign-in timed out.")
    } else if error.is_connect() {
        provider_error("login", "Grok sign-in could not connect to xAI.")
    } else {
        invalid_discovery()
    }
}

fn scaled_duration(milliseconds: u64) -> Duration {
    let scale = std::env::var("KOGEN_TIME_SCALE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(1.0);
    Duration::from_millis((milliseconds as f64 * scale).floor().max(1.0) as u64)
}

fn invalid_endpoint() -> crate::error::CoreError {
    provider_error("login", "Grok returned an invalid sign-in endpoint.")
}

fn invalid_discovery() -> crate::error::CoreError {
    provider_error(
        "login",
        "Grok returned an invalid sign-in discovery document.",
    )
}

fn invalid_device_response() -> crate::error::CoreError {
    provider_error("login", "Grok returned an invalid device sign-in response.")
}

fn expired_code() -> crate::error::CoreError {
    provider_error(
        "login",
        "Grok sign-in code expired; run `kogen provider login grok` again.",
    )
}

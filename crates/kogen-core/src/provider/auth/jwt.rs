//! RS256 OpenID id-token verification against the discovered JWKS.

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde_json::Value;

use super::super::provider_error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Identity {
    pub subject: String,
    pub email: Option<String>,
    pub expires_at: i64,
    pub account_id: Option<String>,
    pub plan_usage: Option<String>,
}

pub(super) fn verify_id_token(
    token: &str,
    jwks: &Value,
    client_id: &str,
    expected_nonce: Option<&str>,
) -> Result<Identity, super::super::CoreError> {
    let header = decode_header(token)
        .map_err(|_| provider_error("login", "ChatGPT returned an invalid id_token"))?;
    if header.alg != Algorithm::RS256 {
        return Err(provider_error("login", "ChatGPT id_token must use RS256"));
    }
    let kid = header
        .kid
        .as_deref()
        .ok_or_else(|| provider_error("login", "ChatGPT id_token has no key id"))?;
    let key = jwks
        .get("keys")
        .and_then(Value::as_array)
        .and_then(|keys| {
            keys.iter().find(|key| {
                key.get("kid").and_then(Value::as_str) == Some(kid)
                    && key.get("kty").and_then(Value::as_str) == Some("RSA")
                    && key
                        .get("alg")
                        .and_then(Value::as_str)
                        .is_none_or(|alg| alg == "RS256")
            })
        })
        .ok_or_else(|| provider_error("login", "ChatGPT signing key was not found"))?;
    let modulus = key
        .get("n")
        .and_then(Value::as_str)
        .ok_or_else(|| provider_error("login", "ChatGPT signing key is invalid"))?;
    let exponent = key
        .get("e")
        .and_then(Value::as_str)
        .ok_or_else(|| provider_error("login", "ChatGPT signing key is invalid"))?;
    let decoding_key = DecodingKey::from_rsa_components(modulus, exponent)
        .map_err(|_| provider_error("login", "ChatGPT signing key is invalid"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_aud = false;
    validation.set_issuer(&["https://auth.openai.com"]);
    let data = decode::<Value>(token, &decoding_key, &validation)
        .map_err(|_| provider_error("login", "ChatGPT id_token signature or claims are invalid"))?;
    let claims = data.claims;
    let audience_matches = match claims.get("aud") {
        Some(Value::String(value)) => value == client_id,
        Some(Value::Array(values)) => values.iter().any(|value| value.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience_matches {
        return Err(provider_error(
            "login",
            "ChatGPT id_token audience is invalid",
        ));
    }
    let subject = claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|subject| !subject.is_empty())
        .ok_or_else(|| provider_error("login", "ChatGPT id_token subject is missing"))?
        .to_owned();
    let expires_at = claims
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or_else(|| provider_error("login", "ChatGPT id_token expiration is invalid"))?;
    if expires_at <= super::now_seconds() {
        return Err(provider_error("login", "ChatGPT id_token has expired"));
    }
    if let Some(nonce) = expected_nonce
        && claims.get("nonce").and_then(Value::as_str) != Some(nonce)
    {
        return Err(provider_error("login", "ChatGPT id_token nonce is invalid"));
    }
    let auth = claims.get("https://api.openai.com/auth");
    Ok(Identity {
        subject,
        email: claims
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_owned),
        expires_at,
        account_id: auth
            .and_then(|value| value.get("chatgpt_account_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        plan_usage: auth
            .and_then(|value| value.get("chatgpt_plan_type"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

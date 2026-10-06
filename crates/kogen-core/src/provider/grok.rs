//! Grok device login, account choice, and local logout.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::{accounts, auth, chatgpt, environment_error, provider_error};
use auth::GrokCredential;

const LABEL: &str = "default";
pub const DEFAULT_MODEL: &str = "grok-4.6";
pub const DEFAULT_EFFORT: &str = "high";

pub fn list(home: &Path) -> Result<String, super::CoreError> {
    accounts::list(home)
}

/// Complete device-code OAuth; progress is flushed by the CLI before polling.
pub fn login(home: &Path, mut progress: impl FnMut(&str)) -> Result<String, super::CoreError> {
    let credential = auth::grok::login(home, &mut progress)?;
    let mut profiles = chatgpt::read_profiles(home)?;
    set_profile(&mut profiles, LABEL, &credential, true);
    chatgpt::write_profiles(home, &profiles)?;
    Ok(signed_in_line(&credential))
}

/// Grok logout removes only local state; xAI tokens are not revoked.
pub fn logout(home: &Path) -> Result<String, super::CoreError> {
    let credential = auth::grok::get(home, LABEL)?;
    auth::grok::delete(home, LABEL)?;
    let mut profiles = chatgpt::read_profiles(home)?;
    match credential {
        Some(credential) => set_profile(&mut profiles, LABEL, &credential, false),
        None => set_signed_out(&mut profiles, LABEL),
    }
    chatgpt::write_profiles(home, &profiles)?;
    Ok("grok:default signed out locally\n".to_owned())
}

pub fn use_account(
    home: &Path,
    label: &str,
    project: Option<&Path>,
) -> Result<String, super::CoreError> {
    if !accounts::valid_label(label) {
        return Err(provider_error("login", "Invalid Grok account label."));
    }
    accounts::read(home)?;
    let project_path = project
        .map(|project| {
            fs::canonicalize(project).map_err(|_| {
                environment_error(
                    "project_not_found",
                    format!("project path {} does not exist", project.display()),
                )
            })
        })
        .transpose()?;
    if auth::grok::get(home, label)?.is_none() {
        return Err(provider_error(
            "login",
            format!(
                "Selected account {label} has no saved login; run kogen provider login <provider> to sign in"
            ),
        ));
    }
    accounts::set_provider_use(home, "grok", label, project_path.as_deref())?;
    match project_path {
        Some(project) => Ok(format!(
            "grok:{label} is the account for {}\n",
            project.display()
        )),
        None => Ok(format!("grok:{label} is the default account\n")),
    }
}

pub(crate) fn record_refresh(
    home: &Path,
    label: &str,
    credential: &GrokCredential,
) -> Result<(), super::CoreError> {
    let mut profiles = chatgpt::read_profiles(home)?;
    set_profile(&mut profiles, label, credential, true);
    chatgpt::write_profiles(home, &profiles)
}

fn signed_in_line(credential: &GrokCredential) -> String {
    match credential.email.as_deref() {
        Some(email) => format!("grok:{LABEL} signed in ({email})\n"),
        None => format!("grok:{LABEL} signed in\n"),
    }
}

fn set_profile(profiles: &mut Value, label: &str, credential: &GrokCredential, signed_in: bool) {
    let profiles = profiles.as_object_mut().expect("validated profile object");
    let grok = profiles
        .entry("grok".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("Grok profile map");
    let mut record = grok
        .get(label)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    record.insert(
        "email".to_owned(),
        credential
            .email
            .as_ref()
            .map_or(Value::Null, |email| Value::String(email.clone())),
    );
    record.insert("expires_at".to_owned(), Value::from(credential.expires_at));
    record.insert("signed_in".to_owned(), Value::Bool(signed_in));
    grok.insert(label.to_owned(), Value::Object(record));
}

fn set_signed_out(profiles: &mut Value, label: &str) {
    let Some(record) = profiles
        .get_mut("grok")
        .and_then(Value::as_object_mut)
        .and_then(|grok| grok.get_mut(label))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    record.insert("signed_in".to_owned(), Value::Bool(false));
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_EFFORT, DEFAULT_MODEL, set_profile};
    use crate::provider::auth::GrokCredential;
    use serde_json::json;

    #[test]
    fn grok_profile_row_contains_only_provider_fields() {
        let credential = GrokCredential {
            access_token: "secret".to_owned(),
            refresh_token: "refresh".to_owned(),
            expires_at: 123,
            scopes: Vec::new(),
            email: Some("g@example.test".to_owned()),
            client_id: "client".to_owned(),
            token_endpoint: "https://auth.x.ai/token".to_owned(),
        };
        let mut profiles = json!({});
        set_profile(&mut profiles, "default", &credential, true);
        assert_eq!(
            profiles,
            json!({"grok":{"default":{
                "email":"g@example.test","expires_at":123,"signed_in":true
            }}})
        );
        assert_eq!(DEFAULT_MODEL, "grok-4.6");
        assert_eq!(DEFAULT_EFFORT, "high");
    }
}

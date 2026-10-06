//! ChatGPT login, logout, account listing and account selection.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::{accounts, auth, environment_error, provider_error};

const LABEL: &str = "default";

pub fn list(home: &Path) -> Result<String, super::CoreError> {
    accounts::list(home)
}

/// Complete ChatGPT's PKCE flow. The progress callback is flushed by the CLI
/// before the browser opens so users can see and copy the authorization URL.
pub fn login(home: &Path, mut progress: impl FnMut(&str)) -> Result<String, super::CoreError> {
    let profiles = read_profiles(home)?;
    let old_profile = profile(&profiles, "chatgpt", LABEL).cloned();
    let show_notice = old_profile
        .as_ref()
        .and_then(|record| record.get("notice_shown"))
        .and_then(Value::as_bool)
        != Some(true);
    let old_credential = auth::get_credential(home, LABEL)?;
    let previous_client = old_credential
        .as_ref()
        .map(|credential| credential.client_id.as_str())
        .or_else(|| {
            old_profile
                .as_ref()
                .and_then(|record| record.get("client_id"))
                .and_then(Value::as_str)
        });
    let (credential, subject, email, plan_usage) =
        auth::login_owned(home, previous_client, &mut progress)?;
    if let Some(previous_subject) = old_profile
        .as_ref()
        .and_then(|record| record.get("subject"))
        .and_then(Value::as_str)
        && previous_subject != subject
    {
        return Err(provider_error(
            "login",
            "ChatGPT account subject changed for the default label; sign out before changing accounts",
        ));
    }
    auth::put_credential(home, LABEL, &credential)?;
    let mut profiles = profiles;
    update_profile(
        &mut profiles,
        LABEL,
        &credential,
        &subject,
        email.as_deref(),
        plan_usage.as_deref(),
        true,
        false,
        show_notice.then_some(true),
    );
    write_profiles(home, &profiles)?;
    if show_notice {
        progress("You're using your ChatGPT plan\n");
    }
    Ok(match email {
        Some(email) => format!("chatgpt:default signed in ({email})\n"),
        None => "chatgpt:default signed in\n".to_owned(),
    })
}

pub fn logout(home: &Path) -> Result<String, super::CoreError> {
    let mut profiles = read_profiles(home)?;
    let credential = auth::get_credential(home, LABEL)?;
    let remote_revoked = credential.as_ref().is_some_and(auth::revoke_owned);
    auth::delete_credential(home, LABEL)?;
    if let Some(credential) = credential.as_ref() {
        let subject = credential.subject.clone();
        let email = credential.email.clone();
        update_profile(
            &mut profiles,
            LABEL,
            credential,
            &subject,
            email.as_deref(),
            None,
            false,
            remote_revoked,
            None,
        );
    } else {
        update_profile_signed_out(&mut profiles, LABEL, remote_revoked);
    }
    write_profiles(home, &profiles)?;
    if remote_revoked {
        Ok("chatgpt:default signed out\n".to_owned())
    } else {
        Ok("chatgpt:default signed out locally; remote revocation was not confirmed. You can disconnect Kogen in ChatGPT Settings if needed.\n".to_owned())
    }
}

pub fn use_account(
    home: &Path,
    label: &str,
    project: Option<&Path>,
) -> Result<String, super::CoreError> {
    if !accounts::valid_label(label) {
        return Err(provider_error(
            "invalid_account_label",
            "invalid account label",
        ));
    }
    // Refuse malformed machine state before checking whether this label has a
    // credential, so `use` never hides a broken accounts.yaml.
    accounts::read(home)?;
    if let Some(project) = project {
        fs::canonicalize(project).map_err(|_| {
            environment_error(
                "project_not_found",
                format!("project path {} does not exist", project.display()),
            )
        })?;
    }
    if auth::get_credential(home, label)?.is_none() {
        return Err(provider_error(
            "login",
            format!(
                "Selected account {label} has no saved login; run kogen provider login chatgpt to sign in"
            ),
        ));
    }
    accounts::set_provider_use(home, "chatgpt", label, project)?;
    if let Some(project) = project {
        let path = fs::canonicalize(project).map_err(|_| {
            environment_error(
                "project_not_found",
                format!("project path {} does not exist", project.display()),
            )
        })?;
        Ok(format!(
            "chatgpt:{label} is the account for {}\n",
            path.display()
        ))
    } else {
        Ok(format!("chatgpt:{label} is the default account\n"))
    }
}

pub(crate) fn record_refresh(
    home: &Path,
    label: &str,
    credential: &auth::Credential,
) -> Result<(), super::CoreError> {
    let mut profiles = read_profiles(home)?;
    let current = profile(&profiles, "chatgpt", label)
        .cloned()
        .unwrap_or_default();
    let subject = current
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or(&credential.subject)
        .to_owned();
    let email = credential.email.clone();
    update_profile(
        &mut profiles,
        label,
        credential,
        &subject,
        email.as_deref(),
        current.get("plan_usage").and_then(Value::as_str),
        true,
        false,
        None,
    );
    write_profiles(home, &profiles)
}

pub(super) fn read_profiles(home: &Path) -> Result<Value, super::CoreError> {
    let path = accounts::profiles_path(home);
    match fs::read(&path) {
        Ok(bytes) => {
            let doc: Value = serde_json::from_slice(&bytes).map_err(|_| {
                environment_error(
                    "invalid_profiles_file",
                    format!("{} is not valid", path.display()),
                )
            })?;
            if doc.is_object() {
                Ok(doc)
            } else {
                Err(environment_error(
                    "invalid_profiles_file",
                    format!("{} is not valid", path.display()),
                ))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(_) => Err(environment_error(
            "invalid_profiles_file",
            format!("{} is not valid", path.display()),
        )),
    }
}

fn profile<'a>(profiles: &'a Value, provider: &str, label: &str) -> Option<&'a Value> {
    profiles.get(provider)?.get(label)
}

#[allow(clippy::too_many_arguments)]
fn update_profile(
    profiles: &mut Value,
    label: &str,
    credential: &auth::Credential,
    subject: &str,
    email: Option<&str>,
    plan_usage: Option<&str>,
    signed_in: bool,
    remote_revoked: bool,
    notice_shown: Option<bool>,
) {
    let profiles = profiles.as_object_mut().expect("validated profile object");
    let provider = profiles
        .entry("chatgpt".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("ChatGPT profile map");
    let previous = provider
        .get(label)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut record = previous;
    record.insert(
        "client_id".to_owned(),
        Value::String(credential.client_id.clone()),
    );
    record.insert("subject".to_owned(), Value::String(subject.to_owned()));
    record.insert(
        "email".to_owned(),
        email.map_or(Value::Null, |email| Value::String(email.to_owned())),
    );
    record.insert("expires_at".to_owned(), Value::from(credential.expires_at));
    record.insert("signed_in".to_owned(), Value::Bool(signed_in));
    record.insert(
        "plan_usage".to_owned(),
        plan_usage.map_or(Value::Null, |usage| Value::String(usage.to_owned())),
    );
    if let Some(notice_shown) = notice_shown {
        record.insert("notice_shown".to_owned(), Value::Bool(notice_shown));
    }
    record.insert("remote_revoked".to_owned(), Value::Bool(remote_revoked));
    provider.insert(label.to_owned(), Value::Object(record));
}

fn update_profile_signed_out(profiles: &mut Value, label: &str, remote_revoked: bool) {
    let Some(record) = profiles
        .get_mut("chatgpt")
        .and_then(Value::as_object_mut)
        .and_then(|provider| provider.get_mut(label))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    record.insert("signed_in".to_owned(), Value::Bool(false));
    record.insert("remote_revoked".to_owned(), Value::Bool(remote_revoked));
}

pub(super) fn write_profiles(home: &Path, profiles: &Value) -> Result<(), super::CoreError> {
    let path = accounts::profiles_path(home);
    let bytes = serde_json::to_vec(profiles)
        .map_err(|_| environment_error("profiles_write_failed", "could not encode profiles"))?;
    write_private_atomic(&path, &bytes).map_err(|_| {
        environment_error(
            "profiles_write_failed",
            format!("could not write {}", path.display()),
        )
    })
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().expect("profiles path has parent");
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp: PathBuf = parent.join(format!(".profiles-{}-{stamp}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temp, path)
}

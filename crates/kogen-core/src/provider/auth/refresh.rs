//! Serialized refresh and the 300 second expiry window.

use std::path::Path;

use super::super::provider_error;
use super::{Credential, get_credential, now_seconds, put_credential};

#[path = "lock.rs"]
mod lock;

pub(super) fn refresh(
    home: &Path,
    label: &str,
    rejected_access_token: Option<&str>,
) -> Result<Credential, super::super::CoreError> {
    if !super::super::accounts::valid_label(label) {
        return Err(provider_error(
            "invalid_account_label",
            "invalid account label",
        ));
    }
    let _lock = lock::RefreshLock::acquire(home, label)?;
    let current = get_credential(home, label)?.ok_or_else(|| {
        provider_error(
            "login",
            format!("Selected account {label} has no saved login; run kogen provider login chatgpt to sign in"),
        )
    })?;
    if rejected_access_token.is_some_and(|rejected| current.access_token != rejected) {
        return Ok(current);
    }
    if rejected_access_token.is_none() && current.expires_at > now_seconds().saturating_add(300) {
        return Ok(current);
    }
    let (updated, identity) = super::oauth::refresh(&current)?;
    if identity.subject != current.subject {
        return Err(provider_error(
            "login",
            "ChatGPT account subject changed during refresh",
        ));
    }
    put_credential(home, label, &updated)?;
    super::super::chatgpt::record_refresh(home, label, &updated)?;
    Ok(updated)
}

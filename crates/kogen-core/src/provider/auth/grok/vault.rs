//! Grok credential paths share ChatGPT's encrypted/file store abstraction.

use std::path::Path;

use super::super::{GrokCredential, provider_error};
use crate::provider::accounts;

pub(crate) fn get(
    home: &Path,
    label: &str,
) -> Result<Option<GrokCredential>, crate::error::CoreError> {
    super::super::store::get_for(home, "grok", label)
}

pub(super) fn put(
    home: &Path,
    label: &str,
    credential: &GrokCredential,
) -> Result<(), crate::error::CoreError> {
    super::super::store::put_for(home, "grok", label, credential)
}

pub(crate) fn delete(home: &Path, label: &str) -> Result<(), crate::error::CoreError> {
    super::super::store::delete_for(home, "grok", label)
}

pub(super) fn validate_label(label: &str) -> Result<(), crate::error::CoreError> {
    if accounts::valid_label(label) {
        Ok(())
    } else {
        Err(provider_error("login", "Invalid Grok account label."))
    }
}

pub(super) fn missing_login() -> crate::error::CoreError {
    provider_error(
        "login",
        "Grok login is missing or invalid; run `kogen provider login grok`.",
    )
}

pub(super) fn unavailable_refresh() -> crate::error::CoreError {
    provider_error(
        "login",
        "Grok login is unavailable; run `kogen provider login grok`.",
    )
}

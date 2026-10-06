use super::error::LandingError;
use crate::git::GitRepo;

pub(super) fn validate_run_id(run_id: &str) -> Result<(), LandingError> {
    if run_id.len() == 32
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(LandingError::invalid(
            "validate run id",
            "run id must be 32 lowercase hexadecimal characters",
        ))
    }
}

pub(super) fn base_ref(branch: &str) -> Result<String, LandingError> {
    let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    if branch.is_empty() || branch.starts_with('-') || branch.contains(['\0', '\n', '\r']) {
        return Err(LandingError::invalid(
            "validate base branch",
            "base branch is invalid",
        ));
    }
    let reference = format!("refs/heads/{branch}");
    GitRepo::new(".")
        .output(&["check-ref-format", &reference])
        .map_err(|_| LandingError::invalid("validate base branch", "base branch is invalid"))?;
    Ok(reference)
}

pub(super) fn incoming_ref(run_id: &str) -> String {
    format!("refs/kogen/incoming/{run_id}")
}

pub(super) fn check_commit(value: &str) -> Result<(), LandingError> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(LandingError::invalid(
            "validate commit id",
            "commit id is not a Git object id",
        ))
    }
}

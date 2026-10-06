use super::error::LandingError;
use super::gitops::arg;
use crate::git::GitRepo;
use std::ffi::OsString;
use std::path::Path;

pub(super) struct SigningConfig {
    pub enabled: bool,
    pub config: Vec<OsString>,
}

pub(super) fn validate_identity(title: &str, slug: &str) -> Result<(), LandingError> {
    if title.trim().is_empty() || title.contains(['\n', '\r', '\0']) {
        return Err(LandingError::invalid(
            "create candidate commit",
            "Intent title must be one non-empty line",
        ));
    }
    if !crate::project::valid_slug(slug) {
        return Err(LandingError::invalid(
            "create candidate commit",
            "Intent slug is invalid",
        ));
    }
    Ok(())
}

pub(super) fn commit_identity(origin: &Path) -> Result<Vec<(OsString, OsString)>, LandingError> {
    let author = GitRepo::new(origin).text(&["var", "GIT_AUTHOR_IDENT"])?;
    let committer = GitRepo::new(origin).text(&["var", "GIT_COMMITTER_IDENT"])?;
    let mut environment = parse_ident("GIT_AUTHOR", &author)?;
    environment.extend(parse_ident("GIT_COMMITTER", &committer)?);
    Ok(environment)
}

fn parse_ident(prefix: &str, identity: &str) -> Result<Vec<(OsString, OsString)>, LandingError> {
    let Some(open) = identity.rfind('<') else {
        return Err(LandingError::invalid(
            "resolve commit identity",
            "Git identity is malformed",
        ));
    };
    let Some(close) = identity.rfind('>') else {
        return Err(LandingError::invalid(
            "resolve commit identity",
            "Git identity is malformed",
        ));
    };
    let name = identity[..open].trim_end();
    let email = identity[open + 1..close].trim();
    let fields = identity[close + 1..].split_whitespace().collect::<Vec<_>>();
    if name.is_empty() || email.is_empty() || fields.len() != 2 {
        return Err(LandingError::invalid(
            "resolve commit identity",
            "Git identity is malformed",
        ));
    }
    let date = format!("@{} {}", fields[0], fields[1]);
    Ok([
        (
            OsString::from(format!("{prefix}_NAME")),
            OsString::from(name),
        ),
        (
            OsString::from(format!("{prefix}_EMAIL")),
            OsString::from(email),
        ),
        (
            OsString::from(format!("{prefix}_DATE")),
            OsString::from(date),
        ),
    ]
    .into_iter()
    .collect())
}

pub(super) fn signing_config(origin: &Path) -> SigningConfig {
    let repo = GitRepo::new(origin);
    let enabled = repo
        .try_text(&["config", "--bool", "--get", "commit.gpgsign"])
        .is_some_and(|value| value == "true");
    let mut config = Vec::new();
    for key in [
        "gpg.format",
        "gpg.program",
        "gpg.ssh.program",
        "user.signingkey",
    ] {
        if let Some(value) = repo.try_text(&["config", "--get", key]) {
            config.push(arg(format!("{key}={value}")));
        }
    }
    SigningConfig { enabled, config }
}

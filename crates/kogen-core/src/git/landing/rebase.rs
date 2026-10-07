use super::error::LandingError;
use super::gitops::{arg, args, git, git_output};
use super::refs::{base_ref, check_commit};
use super::repository::{CandidateCommit, LandingRepository, RebaseAttempt};
use crate::git::GitRepo;
use std::ffi::OsString;
use std::path::Path;

type IdentityEnvironment = Vec<(OsString, OsString)>;
type GitConfig = Vec<OsString>;

pub(super) fn rebase_candidate(
    repository: &LandingRepository,
    candidate: &CandidateCommit,
    branch: &str,
    new_parent: &str,
) -> Result<RebaseAttempt, LandingError> {
    check_commit(new_parent)?;
    let reference = base_ref(branch)?;
    let repo = GitRepo::workspace(repository.workspace());
    git(
        repository.workspace(),
        &args(&[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "origin",
            &reference,
        ]),
        None,
        &[],
    )?;
    if repo.resolve_commit(new_parent).is_err() {
        return Err(LandingError::invalid(
            "fetch moved base",
            "fetched base did not contain the advertised tip",
        ));
    }
    let parent_is_ancestor = git_output(
        repository.workspace(),
        &args(&["merge-base", "--is-ancestor", &candidate.parent, new_parent]),
        None,
        &[],
    )?;
    if !parent_is_ancestor.status.success() {
        if parent_is_ancestor.status.code() == Some(1) {
            return Ok(RebaseAttempt::Impossible {
                detail: "new base is not a descendant of the expected parent".to_owned(),
            });
        }
        return Err(command_error(
            "check moved base ancestry",
            &parent_is_ancestor.stderr,
        ));
    }
    repo.output(&["read-tree", "--reset", &candidate.commit])?;
    repo.output(&["update-ref", "--no-deref", "HEAD", &candidate.commit])?;
    let (identity, config) = rebase_identity(repository.origin())?;
    let rebase_args = args(&["rebase", "--onto", new_parent, &candidate.parent]);
    let output = git_with_identity(repository.workspace(), &rebase_args, &identity, &config)?;
    if output.status.success() {
        Ok(RebaseAttempt::Clean)
    } else {
        let unmerged = repo.output(&["ls-files", "-u", "-z"])?;
        let paths = unmerged_paths(&unmerged);
        if paths.is_empty() {
            Err(command_error("rebase moved base", &output.stderr))
        } else {
            Ok(RebaseAttempt::Conflict {
                paths,
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }
}

pub(super) fn finish_rebase(repository: &LandingRepository) -> Result<(), LandingError> {
    let output = git_output(
        repository.workspace(),
        &args(&["rebase", "--quit"]),
        None,
        &[],
    )?;
    if output.status.success()
        || String::from_utf8_lossy(&output.stderr).contains("no rebase in progress")
    {
        Ok(())
    } else {
        Err(command_error("finish moved-base rebase", &output.stderr))
    }
}

fn git_with_identity(
    cwd: &Path,
    args: &[OsString],
    identity: &[(OsString, OsString)],
    config: &[OsString],
) -> Result<std::process::Output, LandingError> {
    let mut command = std::process::Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.filemode=true",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(
            config
                .iter()
                .flat_map(|value| [OsString::from("-c"), value.clone()]),
        )
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(identity.iter().map(|(key, value)| (key, value)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(any(test, feature = "hermetic-git-tests"))]
    kogen_test_support::configure_git_command(&mut command);
    command.output().map_err(|error| {
        LandingError::from(crate::git::GitError {
            operation: "start git rebase".to_owned(),
            detail: error.to_string(),
        })
    })
}

fn rebase_identity(origin: &Path) -> Result<(IdentityEnvironment, GitConfig), LandingError> {
    let repo = GitRepo::new(origin);
    let author = repo.text(&["var", "GIT_AUTHOR_IDENT"])?;
    let committer = repo.text(&["var", "GIT_COMMITTER_IDENT"])?;
    let mut environment = parse_ident("GIT_AUTHOR", &author)?;
    environment.extend(parse_ident("GIT_COMMITTER", &committer)?);
    let config = repo
        .try_text(&["config", "--get", "gpg.program"])
        .into_iter()
        .map(|value| arg(format!("gpg.program={value}")))
        .collect();
    Ok((environment, config))
}

fn parse_ident(prefix: &str, identity: &str) -> Result<Vec<(OsString, OsString)>, LandingError> {
    let Some(open) = identity.rfind('<') else {
        return Err(LandingError::invalid(
            "resolve rebase identity",
            "Git identity is malformed",
        ));
    };
    let Some(close) = identity.rfind('>') else {
        return Err(LandingError::invalid(
            "resolve rebase identity",
            "Git identity is malformed",
        ));
    };
    let fields = identity[close + 1..].split_whitespace().collect::<Vec<_>>();
    if fields.len() != 2 {
        return Err(LandingError::invalid(
            "resolve rebase identity",
            "Git identity is malformed",
        ));
    }
    let date = format!("@{} {}", fields[0], fields[1]);
    Ok([
        (
            OsString::from(format!("{prefix}_NAME")),
            OsString::from(identity[..open].trim_end()),
        ),
        (
            OsString::from(format!("{prefix}_EMAIL")),
            OsString::from(identity[open + 1..close].trim()),
        ),
        (
            OsString::from(format!("{prefix}_DATE")),
            OsString::from(date),
        ),
    ]
    .into_iter()
    .collect())
}

fn unmerged_paths(output: &[u8]) -> Vec<String> {
    let mut paths = output
        .split(|byte| *byte == 0)
        .filter_map(|row| {
            row.iter()
                .position(|byte| *byte == b'\t')
                .map(|index| &row[index + 1..])
        })
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn command_error(operation: &'static str, detail: &[u8]) -> LandingError {
    LandingError::from(crate::git::GitError {
        operation: operation.to_owned(),
        detail: String::from_utf8_lossy(detail).trim().to_owned(),
    })
}

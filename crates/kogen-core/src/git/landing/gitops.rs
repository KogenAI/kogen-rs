use super::super::{GitCommandOutput, GitError, run_git_command};
use crate::git::landing::error::LandingError;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

const SAFE_CONFIG: &[&str] = &[
    "core.hooksPath=/dev/null",
    "core.fsmonitor=false",
    "core.autocrlf=false",
    "core.filemode=true",
    "core.excludesFile=/dev/null",
    "core.attributesFile=/dev/null",
    "commit.gpgsign=false",
];

pub(super) fn git(
    cwd: &Path,
    args: &[OsString],
    input: Option<&[u8]>,
    env: &[(OsString, OsString)],
) -> Result<Vec<u8>, LandingError> {
    let output = git_output(cwd, args, input, env)?;
    if output.success() {
        Ok(output.stdout)
    } else {
        Err(git_error(args.first(), &output))
    }
}

pub(super) fn git_with_config(
    cwd: &Path,
    config: &[OsString],
    args: &[OsString],
    input: Option<&[u8]>,
    env: &[(OsString, OsString)],
) -> Result<Vec<u8>, LandingError> {
    let mut output_args = Vec::new();
    for value in config {
        output_args.extend([OsString::from("-c"), value.clone()]);
    }
    output_args.extend_from_slice(args);
    let output = git_output(cwd, &output_args, input, env)?;
    if output.success() {
        Ok(output.stdout)
    } else {
        Err(git_error(args.first(), &output))
    }
}

pub(super) fn git_output(
    cwd: &Path,
    args: &[OsString],
    input: Option<&[u8]>,
    env: &[(OsString, OsString)],
) -> Result<GitCommandOutput, LandingError> {
    let mut command = Command::new("git");
    command
        .args(safe_args())
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(env.iter().map(|(key, value)| (key, value)));
    #[cfg(any(test, feature = "hermetic-git-tests"))]
    kogen_test_support::configure_git_command(&mut command);
    run_git_command(command, input).map_err(Into::into)
}

pub(super) fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

pub(super) fn arg(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}

pub(super) fn path_arg(value: &Path) -> OsString {
    value.as_os_str().to_os_string()
}

fn safe_args() -> Vec<OsString> {
    SAFE_CONFIG
        .iter()
        .flat_map(|value| [OsString::from("-c"), OsString::from(*value)])
        .collect()
}

fn git_error(operation: Option<&OsString>, output: &GitCommandOutput) -> LandingError {
    let operation = operation
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "git".to_owned());
    LandingError::from(GitError {
        operation: format!("git {operation}"),
        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

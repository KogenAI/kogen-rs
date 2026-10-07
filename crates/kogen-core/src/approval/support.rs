use super::model::{LedgerFile, ShapeWarning, ShapeWarnings};
use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use crate::git::{GitError, GitRepo};
use crate::intent::{Intent, LintSeverity};
use crate::project::ProjectResolution;
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_RUN: AtomicU64 = AtomicU64::new(0);

pub(super) fn read_source(path: &Path) -> std::io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "source is not a regular file",
        ));
    }
    fs::read(path)
}

pub(super) fn read_acceptance(path: &Path) -> Result<Vec<u8>, CoreError> {
    read_source(path).map_err(|_| {
        intent_error(
            "acceptance_missing",
            format!("{} does not exist", path.display()),
            ExitCode::Usage,
        )
    })
}

pub(super) fn resolve_approver(
    project: &ProjectResolution,
    by: Option<&str>,
) -> Result<String, CoreError> {
    if let Some(value) = by.filter(|value| !value.trim().is_empty()) {
        return Ok(value.to_owned());
    }
    GitRepo::new(&project.checkout)
        .author_identity()
        .map_err(|_| {
            intent_error(
                "approval_identity_unavailable",
                "configure git user.name and user.email",
                ExitCode::Usage,
            )
        })
}

pub(super) fn matching_shape_warnings(
    project: &ProjectResolution,
    slug: &str,
    hash: &str,
) -> Vec<ShapeWarning> {
    let path = project
        .checkout
        .join(format!(".kogen/intents/{slug}/shape-warnings.json"));
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_slice::<ShapeWarnings>(&bytes) else {
        return Vec::new();
    };
    if parsed.approval_sha256 == hash {
        parsed.warnings
    } else {
        Vec::new()
    }
}

pub(super) fn approval_style_warnings(intent: &Intent) -> Vec<ShapeWarning> {
    intent
        .lint()
        .into_iter()
        .filter(|issue| issue.severity == LintSeverity::Style)
        .map(|issue| {
            let item_ids = issue
                .line
                .and_then(|line| intent.acceptance.iter().find(|item| item.line == line))
                .map_or_else(Vec::new, |item| vec![item.id.clone()]);
            ShapeWarning {
                code: format!("lint_{}", issue.rule),
                item_ids,
                message: issue.message,
            }
        })
        .collect()
}

pub(super) fn matching_ledger(
    project: &ProjectResolution,
    slug: &str,
    hash: &str,
) -> Option<Vec<u8>> {
    let bytes = fs::read(
        project
            .checkout
            .join(format!(".kogen/intents/{slug}/ledger.json")),
    )
    .ok()?;
    let ledger: LedgerFile = serde_json::from_slice(&bytes).ok()?;
    (ledger.approval_sha256 == hash).then_some(bytes)
}

pub(super) fn approval_files(
    slug: &str,
    intent: &[u8],
    approval: &[u8],
    ledger: Option<&[u8]>,
    test_path: &str,
    test: &[u8],
) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::from([
        (format!(".kogen/intents/{slug}/intent.md"), intent.to_vec()),
        (
            format!(".kogen/intents/{slug}/approval.json"),
            approval.to_vec(),
        ),
        (test_path.to_owned(), test.to_vec()),
    ]);
    if let Some(ledger) = ledger {
        files.insert(
            format!(".kogen/intents/{slug}/ledger.json"),
            ledger.to_vec(),
        );
    }
    files
}

pub(super) fn approval_message(slug: &str, by: &str, hash: &str, at: &str) -> String {
    format!(
        "Kogen immutable approval package\n\nKogen-Approval: {slug}\nKogen-Approved-By: {by}\nKogen-Approved-Hash: {hash}\nKogen-Approved-At: {at}\n"
    )
}

pub(super) fn acceptance_source_path(project: &ProjectResolution, slug: &str) -> PathBuf {
    project.checkout.join(source_relative_path(project, slug))
}

pub(super) fn source_relative_path(project: &ProjectResolution, slug: &str) -> String {
    format!(".kogen/acceptance/{slug}{}", acceptance_ext(project))
}

pub(super) fn acceptance_candidate_path(project: &ProjectResolution, slug: &str) -> String {
    format!(
        "{}/{}{ext}",
        acceptance_candidate_dir(project),
        slug,
        ext = acceptance_ext(project)
    )
}

pub(super) fn acceptance_ext(project: &ProjectResolution) -> String {
    let acceptance = project
        .config
        .as_ref()
        .and_then(|config| config.raw.as_mapping())
        .and_then(|map| map.get(Value::String("acceptance".to_owned())))
        .and_then(Value::as_mapping);
    if let Some(ext) = acceptance
        .and_then(|map| map.get(Value::String("ext".to_owned())))
        .and_then(Value::as_str)
    {
        return ext.to_owned();
    }
    if project.checkout.join("mix.exs").is_file() {
        "_test.exs".to_owned()
    } else if project.checkout.join("Gemfile").is_file() {
        "_test.rb".to_owned()
    } else {
        ".t.sh".to_owned()
    }
}

fn acceptance_candidate_dir(project: &ProjectResolution) -> String {
    project
        .config
        .as_ref()
        .and_then(|config| config.raw.as_mapping())
        .and_then(|map| map.get(Value::String("acceptance".to_owned())))
        .and_then(Value::as_mapping)
        .and_then(|map| map.get(Value::String("candidate_dir".to_owned())))
        .and_then(Value::as_str)
        .unwrap_or("test/acceptance")
        .to_owned()
}

pub(super) fn approval_run_dir(project: &ProjectResolution, slug: &str) -> PathBuf {
    let id = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    project
        .state_root
        .join("approval-cache/run")
        .join(format!("{slug}-{}-{id}", std::process::id()))
}

pub(super) fn remove_empty_parents(path: &Path, checkout: &Path) {
    let mut current = path.to_path_buf();
    while current.starts_with(checkout) && current != checkout {
        match fs::remove_dir(&current) {
            Ok(()) => {
                current.pop();
            }
            Err(_) => break,
        }
    }
}

pub(super) fn project_uses_witness(project: &ProjectResolution) -> bool {
    project
        .config
        .as_ref()
        .and_then(|config| config.raw.as_mapping())
        .and_then(|map| map.get(Value::String("shaping".to_owned())))
        .and_then(Value::as_mapping)
        .and_then(|map| map.get(Value::String("proof".to_owned())))
        .and_then(Value::as_str)
        == Some("witness")
}

pub(super) fn now_rfc3339() -> String {
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| value.len() >= 20)
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

pub(super) fn hash_mismatch(slug: &str, actual: &str, given: &str) -> CoreError {
    intent_error(
        "hash_mismatch",
        format!(
            "{slug} is now {actual}, not {given}; review it again with kogen intent approve {slug}"
        ),
        ExitCode::Negative,
    )
}

pub(super) fn intent_error(reason: &str, detail: impl Into<String>, exit: ExitCode) -> CoreError {
    CoreError::new(ErrorClass::Intent, reason, detail, exit)
}

pub(super) fn check_error_line(
    reason: &str,
    detail: impl Into<String>,
    exit: ExitCode,
) -> CoreError {
    CoreError::new(ErrorClass::Check, reason, detail, exit)
}

pub(super) fn environment_error(
    reason: &str,
    detail: impl Into<String>,
    exit: ExitCode,
) -> CoreError {
    CoreError::new(ErrorClass::Environment, reason, detail, exit)
}

pub(super) fn controller_error(reason: &str, detail: impl Into<String>) -> CoreError {
    CoreError::new(ErrorClass::Controller, reason, detail, ExitCode::Bug)
}

pub(super) fn environment_git(reason: &str, error: GitError) -> CoreError {
    environment_error(reason, error.to_string(), ExitCode::Environment)
}

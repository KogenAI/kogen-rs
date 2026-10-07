use super::{environment_error, intent_error};
use crate::ExitCode;
use crate::error::CliOutput;
use crate::git::{GitError, GitRepo};
use crate::project::{ProjectResolution, valid_slug};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

pub(super) fn remove_command(project: &ProjectResolution, slug: &str, force: bool) -> CliOutput {
    match remove_inner(project, slug, force) {
        Ok(output) => CliOutput::success(output),
        Err(error) => error.into_cli_output(),
    }
}

fn remove_inner(
    project: &ProjectResolution,
    slug: &str,
    force: bool,
) -> Result<String, crate::error::CoreError> {
    if !valid_slug(slug) {
        return Err(intent_error(
            "invalid_slug",
            "Slug must use lowercase letters, digits, and dashes.",
            ExitCode::Usage,
        ));
    }
    let intent_path = project
        .checkout
        .join(format!(".kogen/intents/{slug}/intent.md"));
    if fs::symlink_metadata(&intent_path).is_err() {
        return Err(intent_error(
            "not_found",
            "Intent does not exist",
            ExitCode::Usage,
        ));
    }
    let origin = GitRepo::new(&project.origin);
    if active_build_for_slug(project, &origin, slug)? {
        return Err(intent_error(
            "remove_blocked",
            "Intent is in an active Build and cannot be removed",
            ExitCode::Usage,
        ));
    }

    let approval_ref = format!("refs/kogen/intents/{slug}");
    let approval = origin
        .ref_target(&approval_ref)
        .map_err(|error| git_error("approval_ref_read_failed", error))?;
    let landed = origin
        .blob_at(
            &origin
                .resolve_commit(&project.base)
                .map_err(|error| git_error("base_unavailable", error))?,
            &format!(".kogen/intents/{slug}/intent.md"),
        )
        .map_err(|error| git_error("base_read_failed", error))?
        .is_some();
    if !force && !landed {
        let reason =
            build_state(project, slug).or_else(|| approval.as_ref().map(|_| "approved".to_owned()));
        if let Some(reason) = reason {
            let detail = match reason.as_str() {
                "failed" => {
                    "Intent still has a failed Build approval; pass --force to discard the approval and remove its files"
                }
                "parked" => {
                    "Intent still has a parked Build approval; pass --force to discard the approval and remove its files"
                }
                "interrupted" => {
                    "Intent still has an approval ref; pass --force to discard the approval and remove its files"
                }
                "approved" => {
                    "Intent approved or queued; pass --force to discard the approval and remove its files"
                }
                _ => {
                    "Intent still has an approval ref; pass --force to discard the approval and remove its files"
                }
            };
            return Err(intent_error(
                "remove_requires_force",
                detail,
                ExitCode::Usage,
            ));
        }
    }

    let source_path = format!(".kogen/acceptance/{slug}{}", acceptance_ext(project));
    let paths = [
        format!(".kogen/intents/{slug}/intent.md"),
        source_path.clone(),
    ];
    let checkout = GitRepo::new(&project.checkout);
    if paths
        .iter()
        .any(|path| !checkout.path_in_index(path).unwrap_or(false))
    {
        return Err(intent_error(
            "remove_requires_commit",
            "Intent files must be tracked to record their removal",
            ExitCode::Usage,
        ));
    }
    let message = format!("Remove Intent {slug}\n");
    let (commit, _) = checkout
        .remove_paths_commit(&paths, &message)
        .map_err(|error| git_error("remove_commit_failed", error))?;
    remove_local_artifacts(project, slug, &source_path);
    if let Some(expected) = approval
        && !origin
            .delete_ref_cas(&approval_ref, &expected)
            .map_err(|error| git_error("approval_ref_delete_failed", error))?
    {
        return Err(crate::error::CoreError::new(
            crate::error::ErrorClass::Controller,
            "approval_ref_changed",
            "approval ref changed while removing the Intent; review the ref before retrying",
            ExitCode::Bug,
        ));
    }
    Ok(format!("removed: {slug}\ncommit: {commit}\n"))
}

fn active_build_for_slug(
    project: &ProjectResolution,
    origin: &GitRepo,
    slug: &str,
) -> Result<bool, crate::error::CoreError> {
    let Some(claim) = origin
        .ref_target("refs/kogen/claim")
        .map_err(|error| git_error("claim_read_failed", error))?
    else {
        return Ok(false);
    };
    let Some(bytes) = origin
        .blob_at(&claim, ".kogen/claim")
        .map_err(|error| git_error("claim_read_failed", error))?
    else {
        return Ok(false);
    };
    let run_id = String::from_utf8_lossy(&bytes).trim().to_owned();
    if run_id.is_empty() {
        return Ok(false);
    }
    let run_path = project
        .state_root
        .join("runs")
        .join(run_id)
        .join("run.json");
    let Ok(bytes) = fs::read(run_path) else {
        return Ok(false);
    };
    let Ok(run) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(false);
    };
    Ok(claim_matches_active_run(&run, slug))
}

fn claim_matches_active_run(run: &Value, slug: &str) -> bool {
    run.get("slug").and_then(Value::as_str) == Some(slug)
        && run.get("status").and_then(Value::as_str) == Some("running")
}

fn build_state(project: &ProjectResolution, slug: &str) -> Option<String> {
    let runs = fs::read_dir(project.state_root.join("runs")).ok()?;
    for entry in runs.flatten() {
        let run_dir = entry.path();
        let path = run_dir.join("run.json");
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if value.get("slug").and_then(Value::as_str) != Some(slug) {
            continue;
        }
        let Some(status) = value.get("status").and_then(Value::as_str) else {
            continue;
        };
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if status == "failed" && (reason == "interrupted" || finished_as_interrupted(&run_dir)) {
            return Some("interrupted".to_owned());
        }
        if matches!(status, "failed" | "parked" | "interrupted") {
            return Some(status.to_owned());
        }
    }
    None
}

fn finished_as_interrupted(run_dir: &std::path::Path) -> bool {
    let Ok(events) = fs::read_to_string(run_dir.join("events.jsonl")) else {
        return false;
    };
    events.lines().any(|line| {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        event.get("event").and_then(Value::as_str) == Some("finished")
            && event.get("reason").and_then(Value::as_str) == Some("interrupted")
    })
}

fn remove_local_artifacts(project: &ProjectResolution, slug: &str, source_path: &str) {
    let intent_dir = project.checkout.join(format!(".kogen/intents/{slug}"));
    let _ = fs::remove_dir_all(intent_dir);
    let source = project.checkout.join(source_path);
    let _ = fs::remove_file(source);
    for directory in [
        project.checkout.join(".kogen/acceptance"),
        project.checkout.join(".kogen/intents"),
    ] {
        let _ = fs::remove_dir(directory);
    }
}

fn acceptance_ext(project: &ProjectResolution) -> String {
    let acceptance = project
        .config
        .as_ref()
        .and_then(|config| config.raw.as_mapping())
        .and_then(|map| map.get(serde_yaml::Value::String("acceptance".to_owned())))
        .and_then(serde_yaml::Value::as_mapping);
    acceptance
        .and_then(|map| map.get(serde_yaml::Value::String("ext".to_owned())))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if project.checkout.join("mix.exs").is_file() {
                "_test.exs".to_owned()
            } else if project.checkout.join("Gemfile").is_file() {
                "_test.rb".to_owned()
            } else {
                ".t.sh".to_owned()
            }
        })
}

fn git_error(reason: &str, error: GitError) -> crate::error::CoreError {
    environment_error(reason, error.to_string(), ExitCode::Environment)
}

#[allow(dead_code)]
fn _path_buf(path: &str) -> PathBuf {
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::claim_matches_active_run;
    use serde_json::json;

    #[test]
    fn claim_blocks_removal_while_the_run_snapshot_is_running() {
        assert!(claim_matches_active_run(
            &json!({"slug":"greet","status":"running"}),
            "greet"
        ));
        assert!(!claim_matches_active_run(
            &json!({"slug":"greet","status":"landed"}),
            "greet"
        ));
        assert!(!claim_matches_active_run(
            &json!({"slug":"farewell","status":"running"}),
            "greet"
        ));
    }
}

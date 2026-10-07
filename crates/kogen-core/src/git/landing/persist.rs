use super::engine::{LandingOutcome, LandingRequest};
use super::error::LandingError;
use super::model::{LandingEvent, LandingModel};
use super::repository::{CandidateCommit, WorktreeUpdate};
use crate::run::{LandingRecord, RunEvent};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn save_landing(
    request: &mut LandingRequest<'_>,
    candidate: &CandidateCommit,
    expected_parent: &str,
    tree: &str,
) -> Result<(), LandingError> {
    request.snapshot.landing = Some(LandingRecord {
        approval_commit: request.snapshot.approval_commit.clone(),
        run_id: request.snapshot.run_id.clone(),
        expected_parent: expected_parent.to_owned(),
        final_tree: tree.to_owned(),
        candidate_commit: candidate.commit.clone(),
        fields: Default::default(),
    });
    let event = RunEvent::new("landing_prepared", now_ms())
        .with("expected_parent", json!(expected_parent))
        .with("final_tree", json!(tree))
        .with("candidate_commit", json!(candidate.commit));
    request.store.record(&event, request.snapshot)?;
    Ok(())
}

pub(super) fn finish_landed(
    request: &mut LandingRequest<'_>,
    model: &mut LandingModel,
    candidate: &CandidateCommit,
    checked_out: &[WorktreeUpdate],
    expected_parent: &str,
) -> Result<LandingOutcome, LandingError> {
    record_event(
        request,
        RunEvent::new("base_cas", now_ms()).with("candidate_commit", json!(candidate.commit)),
    )?;
    let mut warnings = Vec::new();
    let mut cleanup_failures = Vec::new();
    let updates = request.repository.update_checked_out(
        checked_out,
        &candidate.commit,
        expected_parent,
        &request.snapshot.target_branch,
    );
    for update in updates {
        let dirty = update.dirty || !update.updated;
        model.apply(LandingEvent::Worktree(dirty));
        if dirty {
            let warning =
                checkout_warning(&update, &candidate.commit, &request.snapshot.target_branch);
            warnings.push(warning.clone());
            if let Err(error) = record_event(
                request,
                RunEvent::new("landing_warning", now_ms())
                    .with("path", json!(update.path))
                    .with("detail", json!(warning)),
            ) {
                cleanup_failures.push(error.to_string());
            }
        }
    }
    let incoming_deleted = request
        .repository
        .delete_incoming(&request.snapshot.run_id, &candidate.commit);
    let incoming_ok = matches!(incoming_deleted, Ok(true));
    model.apply(LandingEvent::Drop(incoming_ok));
    if !incoming_ok {
        let reason = incoming_deleted.err().map_or_else(
            || "incoming ref did not match the landing commit".to_owned(),
            |error| error.to_string(),
        );
        cleanup_failures.push(reason.clone());
        let _ = record_event(
            request,
            RunEvent::new("cleanup_failure", now_ms())
                .with("operation", json!("delete incoming ref"))
                .with("detail", json!(reason)),
        );
    }
    if let Err(error) = request.repository.release_claim(&request.snapshot.run_id) {
        cleanup_failures.push(error.to_string());
        let _ = record_event(
            request,
            RunEvent::new("cleanup_failure", now_ms())
                .with("operation", json!("release origin claim"))
                .with("detail", json!(error.to_string())),
        );
    }
    request.snapshot.status = "landed".to_owned();
    if let Err(error) = persist_terminal(request, "finished", "") {
        cleanup_failures.push(error.to_string());
    }
    if let Err(error) = request.repository.cleanup_workspace() {
        cleanup_failures.push(error.to_string());
        let _ = record_event(
            request,
            RunEvent::new("cleanup_failure", now_ms())
                .with("operation", json!("remove landing workspace"))
                .with("detail", json!(error.to_string())),
        );
    }
    Ok(LandingOutcome::Landed {
        commit: candidate.commit.clone(),
        tree: candidate.tree.clone(),
        warnings,
        cleanup_failures,
        observation: model.observe(),
    })
}

pub(super) fn park(
    request: &mut LandingRequest<'_>,
    model: &LandingModel,
    candidate: &CandidateCommit,
) -> Result<LandingOutcome, LandingError> {
    request
        .repository
        .park(&request.snapshot.run_id, &candidate.commit)?;
    request.snapshot.status = "parked".to_owned();
    let reason = "not_landable".to_owned();
    persist_terminal(request, "finished", &reason)?;
    let _ = request
        .repository
        .delete_incoming(&request.snapshot.run_id, &candidate.commit);
    if let Err(error) = request.repository.release_claim(&request.snapshot.run_id) {
        let _ = record_event(
            request,
            RunEvent::new("cleanup_failure", now_ms())
                .with("operation", json!("release origin claim"))
                .with("detail", json!(error.to_string())),
        );
    }
    if let Err(error) = request.repository.cleanup_workspace() {
        let _ = record_event(
            request,
            RunEvent::new("cleanup_failure", now_ms())
                .with("operation", json!("remove landing workspace"))
                .with("detail", json!(error.to_string())),
        );
    }
    Ok(LandingOutcome::Parked {
        commit: candidate.commit.clone(),
        reason,
        observation: model.observe(),
    })
}

pub(super) fn persist_terminal(
    request: &LandingRequest<'_>,
    event: &str,
    reason: &str,
) -> Result<(), LandingError> {
    let mut entry = RunEvent::new(event, now_ms()).with("status", json!(request.snapshot.status));
    if !reason.is_empty() {
        entry = entry.with("reason", json!(reason));
    }
    if let Some(verdict) = request.snapshot.fields.get("verdict") {
        entry = entry.with("verdict", verdict.clone());
    }
    for field in ["rung", "advisory_items"] {
        if let Some(value) = request.snapshot.fields.get(field) {
            entry = entry.with(field, value.clone());
        }
    }
    request.store.record(&entry, request.snapshot)?;
    Ok(())
}

pub(super) fn record_event(
    request: &LandingRequest<'_>,
    event: RunEvent,
) -> Result<(), LandingError> {
    request.store.record(&event, request.snapshot)?;
    Ok(())
}

pub(super) fn validate_snapshot(request: &LandingRequest<'_>) -> Result<(), LandingError> {
    if request.snapshot.status != "running"
        || request.snapshot.run_id.len() != 32
        || request.snapshot.slug.is_empty()
        || request.snapshot.target_branch.is_empty()
    {
        return Err(LandingError::invalid(
            "validate landing request",
            "snapshot is not a running Build with a run id, slug, and base branch",
        ));
    }
    Ok(())
}

fn checkout_warning(update: &WorktreeUpdate, commit: &str, branch: &str) -> String {
    format!(
        "land: warning: landed {commit} on {branch}; your checkout at {} has local changes and was not updated; run `git reset --keep {commit}`, or merge it yourself",
        update.path.display()
    )
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

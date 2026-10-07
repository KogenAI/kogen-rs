use super::error::LandingError;
use super::model::{LandingEvent, LandingModel, LandingObservation, RebaseKind, RepairKind};
use super::persist::{
    finish_landed, now_ms, park, persist_terminal, record_event, save_landing, validate_snapshot,
};
use super::repository::{CandidateCommit, LandingRepository, RebaseAttempt};
use crate::run::{RunEvent, RunSnapshot, RunStore};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct LandingRequest<'a> {
    pub repository: &'a LandingRepository,
    pub store: &'a RunStore,
    pub snapshot: &'a mut RunSnapshot,
    pub title: &'a str,
    pub expected_parent: &'a str,
    pub verified_tree: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepairResult {
    Green { verified_tree: String },
    Red,
    Spent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrationResult {
    pub rebase: RebaseKind,
    pub repairs: Vec<RepairResult>,
    pub verified_tree: Option<String>,
}

pub trait IntegrationGate {
    /// Re-run guard and verification on the moved base, repairing within the given deadline.
    fn reverify_and_repair(
        &mut self,
        workspace: &Path,
        new_parent: &str,
        rebase: &RebaseAttempt,
        snapshot: &mut RunSnapshot,
        deadline: Instant,
    ) -> Result<IntegrationResult, LandingError>;
}

pub trait LandingWait {
    fn wait(&mut self, delay: Duration);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LandingPoint {
    RecordDurable,
    IncomingPublished,
    BeforeBaseCas,
    BaseCasSucceeded,
}

pub trait LandingObserver {
    fn reached(
        &mut self,
        point: LandingPoint,
        candidate: &CandidateCommit,
    ) -> Result<(), LandingError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoLandingObserver;

impl LandingObserver for NoLandingObserver {
    fn reached(
        &mut self,
        _point: LandingPoint,
        _candidate: &CandidateCommit,
    ) -> Result<(), LandingError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLandingWait;

impl LandingWait for SystemLandingWait {
    fn wait(&mut self, delay: Duration) {
        std::thread::sleep(delay);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LandingOutcome {
    Landed {
        commit: String,
        tree: String,
        warnings: Vec<String>,
        cleanup_failures: Vec<String>,
        observation: LandingObservation,
    },
    Parked {
        commit: String,
        reason: String,
        observation: LandingObservation,
    },
    Stopped {
        reason: String,
        observation: LandingObservation,
    },
}

pub fn land(
    request: LandingRequest<'_>,
    integration: &mut impl IntegrationGate,
    wait: &mut impl LandingWait,
) -> Result<LandingOutcome, LandingError> {
    land_excluding(request, &[], integration, wait)
}

pub fn land_excluding(
    request: LandingRequest<'_>,
    excluded_paths: &[PathBuf],
    integration: &mut impl IntegrationGate,
    wait: &mut impl LandingWait,
) -> Result<LandingOutcome, LandingError> {
    land_with_observer_excluding(
        request,
        excluded_paths,
        integration,
        wait,
        &mut NoLandingObserver,
    )
}

pub fn land_with_observer(
    request: LandingRequest<'_>,
    integration: &mut impl IntegrationGate,
    wait: &mut impl LandingWait,
    observer: &mut impl LandingObserver,
) -> Result<LandingOutcome, LandingError> {
    land_with_observer_excluding(request, &[], integration, wait, observer)
}

pub fn land_with_observer_excluding(
    mut request: LandingRequest<'_>,
    excluded_paths: &[PathBuf],
    integration: &mut impl IntegrationGate,
    wait: &mut impl LandingWait,
    observer: &mut impl LandingObserver,
) -> Result<LandingOutcome, LandingError> {
    validate_snapshot(&request)?;
    request.repository.verify_claim(&request.snapshot.run_id)?;
    let mut model = LandingModel::new();
    let mut expected_parent = request.expected_parent.to_owned();
    let mut verified_tree = request.verified_tree.to_owned();
    let mut candidate = request.repository.candidate_commit_excluding(
        &expected_parent,
        &verified_tree,
        request.title,
        &request.snapshot.slug,
        excluded_paths,
    )?;
    save_landing(&mut request, &candidate, &expected_parent, &verified_tree)?;
    observer.reached(LandingPoint::RecordDurable, &candidate)?;
    model.apply(LandingEvent::Record);

    loop {
        if request
            .repository
            .base_locked(&request.snapshot.target_branch)?
        {
            model.apply(LandingEvent::Lock);
            match retry_or_move(&mut request, &mut model, wait, "lock")? {
                RetryDecision::Retry => continue,
                RetryDecision::Moved => {
                    let Some(updated) =
                        integrate_moved(&mut request, &mut model, integration, &candidate)?
                    else {
                        return park(&mut request, &model, &candidate);
                    };
                    expected_parent = updated.0;
                    verified_tree = updated.1;
                    candidate = request.repository.candidate_commit_excluding(
                        &expected_parent,
                        &verified_tree,
                        request.title,
                        &request.snapshot.slug,
                        excluded_paths,
                    )?;
                    save_landing(&mut request, &candidate, &expected_parent, &verified_tree)?;
                    continue;
                }
            }
        }

        if let Err(error) =
            request
                .repository
                .verify_candidate(&candidate.commit, &expected_parent, &verified_tree)
        {
            if error.kind == super::error::LandingErrorKind::Controller {
                let kind = if error.detail.contains("not_fast_forward") {
                    "not_fast_forward"
                } else {
                    "tree_mismatch"
                };
                model.apply(LandingEvent::Head(kind));
                request.snapshot.status = "stopped".to_owned();
                let _ = request.repository.release_claim(&request.snapshot.run_id);
                persist_terminal(&request, "finished", &model.observe().reason)?;
                return Ok(LandingOutcome::Stopped {
                    reason: model.observe().reason,
                    observation: model.observe(),
                });
            }
            return Err(error);
        }
        model.apply(LandingEvent::Head("ok"));
        request
            .repository
            .push_incoming(&request.snapshot.run_id, &candidate.commit)?;
        observer.reached(LandingPoint::IncomingPublished, &candidate)?;
        model.apply(LandingEvent::Push);
        let checked_out = request
            .repository
            .inspect_checked_out(&request.snapshot.target_branch)?;
        observer.reached(LandingPoint::BeforeBaseCas, &candidate)?;
        let cas = request.repository.cas_base(
            &request.snapshot.target_branch,
            &candidate.commit,
            &expected_parent,
        )?;
        model.apply(LandingEvent::Cas(cas));
        if cas {
            observer.reached(LandingPoint::BaseCasSucceeded, &candidate)?;
            return finish_landed(
                &mut request,
                &mut model,
                &candidate,
                &checked_out,
                &expected_parent,
            );
        }
        let removed = request
            .repository
            .delete_incoming(&request.snapshot.run_id, &candidate.commit)?;
        if !removed {
            return Err(LandingError::controller(
                "retry landing CAS",
                "incoming ref changed before retry cleanup",
            ));
        }
        match retry_or_move(&mut request, &mut model, wait, "cas")? {
            RetryDecision::Retry => {}
            RetryDecision::Moved => {
                let Some(updated) =
                    integrate_moved(&mut request, &mut model, integration, &candidate)?
                else {
                    return park(&mut request, &model, &candidate);
                };
                expected_parent = updated.0;
                verified_tree = updated.1;
                candidate = request.repository.candidate_commit_excluding(
                    &expected_parent,
                    &verified_tree,
                    request.title,
                    &request.snapshot.slug,
                    excluded_paths,
                )?;
                save_landing(&mut request, &candidate, &expected_parent, &verified_tree)?;
            }
        }
    }
}

enum RetryDecision {
    Retry,
    Moved,
}

fn retry_or_move(
    request: &mut LandingRequest<'_>,
    model: &mut LandingModel,
    wait: &mut impl LandingWait,
    reason: &str,
) -> Result<RetryDecision, LandingError> {
    let observation = model.observe();
    if observation.phase == "retrying" {
        record_event(
            request,
            RunEvent::new("landing_retry", now_ms())
                .with("reason", json!(reason))
                .with("delay_ms", json!(observation.delay)),
        )?;
        wait.wait(Duration::from_millis(observation.delay));
        model.apply(LandingEvent::Again);
        Ok(RetryDecision::Retry)
    } else {
        record_event(
            request,
            RunEvent::new("landing_retry", now_ms())
                .with("reason", json!("rebase_required"))
                .with("delay_ms", json!(0)),
        )?;
        Ok(RetryDecision::Moved)
    }
}

fn integrate_moved(
    request: &mut LandingRequest<'_>,
    model: &mut LandingModel,
    integration: &mut impl IntegrationGate,
    candidate: &CandidateCommit,
) -> Result<Option<(String, String)>, LandingError> {
    let new_parent = request
        .repository
        .current_base(&request.snapshot.target_branch)?;
    let rebase = request.repository.rebase_candidate(
        candidate,
        &request.snapshot.target_branch,
        &new_parent,
    )?;
    if matches!(rebase, RebaseAttempt::Impossible { .. }) {
        model.apply(LandingEvent::Rebase(RebaseKind::Impossible));
        return Ok(None);
    }
    let deadline = Instant::now() + landing_allowance();
    let result = integration.reverify_and_repair(
        request.repository.workspace(),
        &new_parent,
        &rebase,
        request.snapshot,
        deadline,
    )?;
    let observation = model.apply(LandingEvent::Rebase(result.rebase));
    if observation.phase == "parked" {
        request.repository.finish_rebase()?;
        return Ok(None);
    }
    let mut verified_tree = result.verified_tree;
    for repair in result.repairs {
        match repair {
            RepairResult::Green {
                verified_tree: tree,
            } => {
                model.apply(LandingEvent::Repair(RepairKind::Green));
                verified_tree = Some(tree);
            }
            RepairResult::Red => {
                model.apply(LandingEvent::Repair(RepairKind::Red));
            }
            RepairResult::Spent => {
                model.apply(LandingEvent::Repair(RepairKind::Spent));
                request.repository.finish_rebase()?;
                return Ok(None);
            }
        }
    }
    request.repository.finish_rebase()?;
    let observation = model.observe();
    if observation.phase != "recorded" {
        return Err(LandingError::invalid(
            "integrate moved base",
            "integration gate returned without a green verification or spent allowance",
        ));
    }
    let Some(tree) = verified_tree else {
        return Err(LandingError::invalid(
            "integrate moved base",
            "green integration result omitted its verified tree",
        ));
    };
    Ok(Some((new_parent, tree)))
}

fn landing_allowance() -> Duration {
    let scale = std::env::var("KOGEN_TIME_SCALE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(1.0);
    let millis = (crate::run::orchestration::LANDING_ALLOWANCE_MS as f64 * scale)
        .floor()
        .max(1.0) as u64;
    Duration::from_millis(millis)
}

//! Verified candidate commits, incoming refs, base CAS, and moved-base repair.

mod commit;
mod engine;
mod error;
mod gitops;
pub mod model;
mod persist;
mod rebase;
mod refs;
mod repository;
mod worktree;

#[cfg(test)]
mod tests;

pub use engine::{
    IntegrationGate, IntegrationResult, LandingObserver, LandingOutcome, LandingPoint,
    LandingRequest, LandingWait, NoLandingObserver, RepairResult, SystemLandingWait, land,
    land_with_observer,
};
pub use error::{LandingError, LandingErrorKind};
pub use model::{LandingEvent, LandingModel, LandingObservation, RebaseKind};
pub use repository::{CandidateCommit, LandingRepository, RebaseAttempt, WorktreeUpdate};

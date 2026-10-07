//! Reconcile dead Build owners using reachable landing commits and durable journals.

mod project;
mod replay;

pub use project::{RecoveryReport, owner_is_alive, process_started_ms, reconcile};
pub use replay::{
    RecoveryDecision, RecoveryEvent, RecoveryFact, RecoveryModel, RecoveryObservation, RecoveryRun,
    recovery_decision,
};

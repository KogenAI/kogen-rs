//! Status is derived from approvals, reachable landing commits, the owner claim,
//! and durable run records. It is never stored as a mutable Intent flag.

mod agents;
mod model;
mod project;
mod replay;

pub use agents::AgentStatus;
pub use model::{
    DependencyState, IntentFacts, IntentStatus, StatusBoard, StatusKind, StatusRun, classify_facts,
    dependency_reason, derive_board, is_status,
};
pub use project::{StatusReport, inspect};
pub use replay::{StatusEvent, StatusRaw, StatusRow};
pub use replay::{StatusObservation, StatusReplay};

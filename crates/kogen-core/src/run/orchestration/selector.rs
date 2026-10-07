mod candidate;
mod replay;
mod score;

pub use candidate::{
    Candidate, CandidateMetadata, CandidateSelector, SelectionError, SelectionReport,
};
pub use replay::GateReplay;
pub use score::{
    CheckScoreInput, GatePolicy, GateScore, ItemKind, ItemResult, ItemVerdict, VerificationVerdict,
    blocking_count, score_verification,
};

#[cfg(test)]
#[path = "selector_tests.rs"]
mod tests;

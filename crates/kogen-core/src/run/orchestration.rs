//! Build ladder policy, candidate scoring, and report records.
//!
//! The green gate remains in `crate::gate`; this module only decides what to
//! try next and which completed candidate to preserve.

pub mod auditor;
pub mod budget;
pub mod finish;
mod machine;
pub mod recipe;
pub mod replay;
pub mod report;
pub mod selector;

pub use auditor::{
    AuditDisposition, BUILD_AUDITOR_DIFF_LIMIT, BUILD_AUDITOR_MARKER, BuildAuditItem,
    BuildAuditRequest, BuildAuditVerdict, BuildAuditor, decode_build_audit,
};
pub use budget::{BuildBudget, DEFAULT_BUILD_BUDGET_MS, LANDING_ALLOWANCE_MS, MAX_STAGE_WALL_MS};
pub use finish::{FinishAction, FinishPolicy};
pub use machine::{
    BuildEvent, BuildMachine, BuildObservation, BuildPhase, BuildStatus, Effect, step,
};
pub use recipe::{
    BuildRecipe, BuildRecipeError, BuilderModel, Difficulty, InputKind, RecipeKind, RecipeRung,
    RungAttempt, RungSchedule, ToolSet,
};
pub use report::{
    AcceptanceReport, AuditReport, BestCandidateReport, BudgetReport, BuildReport, CheckReport,
    CredentialReport, FailureReport, FindingReport, ModelStageReport, RungReport, SandboxReport,
};
pub use selector::{
    Candidate, CandidateMetadata, CandidateSelector, CheckScoreInput, GatePolicy, GateReplay,
    GateScore, ItemKind, ItemResult, ItemVerdict, SelectionError, SelectionReport,
    VerificationVerdict, blocking_count, score_verification,
};

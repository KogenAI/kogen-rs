//! Candidate acceptance, tree identity, configured checks, and protection.

mod checks;
mod protection;
mod tree;
mod verification;
pub(crate) mod workspace;

pub mod adapters;

pub mod ledger;

pub use ledger::{
    AcceptanceFailure, AcceptanceRunError, CommandAcceptanceRequest, CommandAcceptanceResult,
    LedgerReadError, LedgerRow, LedgerStatus, TreeSnapshotPort, read_ledger_report,
    run_command_acceptance,
};

pub(crate) use checks::is_test_rule;
pub use checks::{
    CheckBaseline, CheckCommand, CheckConfigError, CheckFinding, CheckResult, CheckRunError,
    CheckStatus, FixResult, configured_commands, is_excused, run_check,
};
pub use protection::{
    ABSENT_SHA256, ProtectedEntry, ProtectedFinding, ProtectedWorkspace, ProtectionError,
    install_approved_acceptance,
};
pub use tree::{GitTreeSnapshot, TreeSnapshotError, commit_tree_id, snapshot_tree};
pub use tree::{GitTreeSnapshotWithExclusions, snapshot_tree_excluding};
pub use verification::{
    AcceptancePlan, GateError, GateReport, GateRequest, GateVerdict, VerificationReceipt, run_gate,
};
pub use workspace::WorkspaceError;

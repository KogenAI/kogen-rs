//! Candidate acceptance, tree identity, configured checks, and protection.

mod checks;
mod protection;
mod tree;
mod verification;
mod workspace;

pub mod ledger;

pub use ledger::{
    AcceptanceFailure, AcceptanceRunError, CommandAcceptanceRequest, CommandAcceptanceResult,
    LedgerReadError, LedgerRow, LedgerStatus, TreeSnapshotPort, read_ledger_report,
    run_command_acceptance,
};

pub use checks::{
    CheckBaseline, CheckCommand, CheckConfigError, CheckFinding, CheckResult, CheckRunError,
    CheckStatus, FixResult, configured_commands, is_excused, run_check,
};
pub use protection::{
    ABSENT_SHA256, ProtectedEntry, ProtectedFinding, ProtectedWorkspace, ProtectionError,
    install_approved_acceptance,
};
pub use tree::{GitTreeSnapshot, TreeSnapshotError, commit_tree_id, snapshot_tree};
pub use verification::{
    AcceptancePlan, GateError, GateReport, GateRequest, GateVerdict, VerificationReceipt, run_gate,
};
pub use workspace::WorkspaceError;

//! Git helpers used by the production approval command.

use crate::git::{GitError, GitRepo};

/// Compute the prefix decision from the exact source bytes.
#[must_use]
pub fn prefix_matches(intent: &[u8], acceptance: &[u8], prefix: &str) -> bool {
    crate::intent::approval_sha256(intent, acceptance).starts_with(prefix)
}

/// Write an immutable approval commit using the same snapshot format as the CLI.
pub struct ApprovalPackage<'a> {
    pub slug: &'a str,
    pub intent: &'a [u8],
    pub approval: &'a [u8],
    pub ledger: Option<&'a [u8]>,
    pub test_path: &'a str,
    pub acceptance: &'a [u8],
    pub by: &'a str,
    pub hash: &'a str,
    pub at: &'a str,
    pub parent: Option<&'a str>,
}

pub fn create_approval_commit(
    repo: &GitRepo,
    package: ApprovalPackage<'_>,
) -> Result<String, GitError> {
    let files = super::support::approval_files(
        package.slug,
        package.intent,
        package.approval,
        package.ledger,
        package.test_path,
        package.acceptance,
    );
    let message =
        super::support::approval_message(package.slug, package.by, package.hash, package.at);
    repo.create_commit(&files, package.parent, &message)
}

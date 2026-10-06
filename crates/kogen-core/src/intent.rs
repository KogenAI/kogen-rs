//! Byte-preserving Intent grammar, structural lint, and approval hash inputs.

mod lint;
mod parser;

#[cfg(test)]
mod tests;

pub use crate::project::valid_slug;
pub use lint::{LintIssue, LintSeverity};
pub use parser::{
    AcceptanceItem, Contract, Frontmatter, Intent, IntentParseError, ParseIssue, VerifyItem,
};

use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalByteInputs {
    pub intent: Vec<u8>,
    pub acceptance: Vec<u8>,
}

impl ApprovalByteInputs {
    pub fn read(
        intent_path: impl AsRef<Path>,
        acceptance_path: impl AsRef<Path>,
    ) -> std::io::Result<Self> {
        let intent = std::fs::read(intent_path)?;
        let acceptance = std::fs::read(acceptance_path)?;
        Ok(Self { intent, acceptance })
    }

    pub fn intent_sha256(&self) -> String {
        intent_sha256(&self.intent)
    }
    pub fn approval_sha256(&self) -> String {
        approval_sha256(&self.intent, &self.acceptance)
    }
}

pub fn intent_sha256(intent_bytes: &[u8]) -> String {
    sha256_hex(intent_bytes)
}

pub fn approval_sha256(intent_bytes: &[u8], acceptance_bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(intent_bytes);
    digest.update([0]);
    digest.update(acceptance_bytes);
    hex(&digest.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

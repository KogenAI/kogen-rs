use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ApprovalDocument {
    pub schema: u8,
    pub slug: String,
    pub approval_sha256: String,
    pub intent_sha256: String,
    pub target_branch: String,
    pub base_sha: String,
    pub domains: Vec<String>,
    pub acceptance_paths: Vec<String>,
    pub protected_manifest: BTreeMap<String, String>,
    pub check_baseline: Vec<BaselineRow>,
    pub witness: Option<Witness>,
    pub by: String,
    pub at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct BaselineRow {
    pub name: String,
    pub status: String,
    pub exit_status: Option<i32>,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Finding {
    pub path: String,
    pub rule: String,
    pub symbol: String,
    pub message: String,
    #[serde(skip)]
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Witness {
    pub verdict: String,
    pub commit: String,
    pub diff_sha256: String,
    pub base_sha: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ShapeWarnings {
    pub approval_sha256: String,
    pub warnings: Vec<ShapeWarning>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ShapeWarning {
    pub code: String,
    pub item_ids: Vec<String>,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct LedgerFile {
    pub approval_sha256: String,
    pub rows: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct BaselineCache {
    pub key: String,
    pub rows: Vec<BaselineRow>,
}

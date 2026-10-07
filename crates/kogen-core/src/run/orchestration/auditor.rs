use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const BUILD_AUDITOR_MARKER: &str = "You are Kogen's acceptance test auditor.";
pub const BUILD_AUDITOR_DIFF_LIMIT: usize = 60_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildAuditVerdict {
    Valid,
    OverStrict,
    Contradicts,
}

impl BuildAuditVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::OverStrict => "over_strict",
            Self::Contradicts => "contradicts",
        }
    }

    #[must_use]
    pub const fn demotes(self) -> bool {
        matches!(self, Self::OverStrict | Self::Contradicts)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BuildAuditItem {
    pub id: String,
    pub verdict: BuildAuditVerdict,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildAuditRequest {
    pub ids: Vec<String>,
    pub request: String,
    pub test_source: String,
    pub failure_output: String,
    pub candidate_diff: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditDisposition {
    pub id: String,
    pub verdict: BuildAuditVerdict,
    pub reason: String,
    pub demote: bool,
}

/// Tracks one build-time audit per failed acceptance item, even when the
/// model reply is malformed or the item is upheld.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildAuditor {
    audited: BTreeSet<String>,
    records: Vec<BuildAuditItem>,
}

impl BuildAuditor {
    #[must_use]
    pub fn begin_rung_audit(
        &mut self,
        failed_item_ids: &[String],
        only_acceptance_failures: bool,
        witness_mode: bool,
    ) -> Vec<String> {
        if !only_acceptance_failures || witness_mode {
            return Vec::new();
        }
        failed_item_ids
            .iter()
            .filter(|id| self.audited.insert((*id).clone()))
            .cloned()
            .collect()
    }

    pub fn complete_rung_audit(
        &mut self,
        item_ids: &[String],
        reply: &str,
    ) -> Vec<AuditDisposition> {
        let dispositions = decode_build_audit(reply, item_ids);
        self.records
            .extend(dispositions.iter().map(|item| BuildAuditItem {
                id: item.id.clone(),
                verdict: item.verdict,
                reason: item.reason.clone(),
            }));
        dispositions
    }

    #[must_use]
    pub fn records(&self) -> &[BuildAuditItem] {
        &self.records
    }

    #[must_use]
    pub fn audited_ids(&self) -> &BTreeSet<String> {
        &self.audited
    }
}

impl BuildAuditRequest {
    #[must_use]
    pub fn clipped_diff(&self) -> String {
        self.candidate_diff
            .chars()
            .take(BUILD_AUDITOR_DIFF_LIMIT)
            .collect()
    }

    #[must_use]
    pub fn user_message(&self) -> String {
        format!(
            "Failed acceptance items: {}\n\nVerbatim Request:\n{}\n\nAcceptance test source:\n{}\n\nFailure output:\n{}\n\nCandidate diff (clipped to {BUILD_AUDITOR_DIFF_LIMIT} characters):\n{}",
            self.ids.join(", "),
            self.request,
            self.test_source,
            self.failure_output,
            self.clipped_diff(),
        )
    }
}

/// Decode only judgments for the failed items in this rung. Invalid JSON,
/// missing rows, unknown ids, and unknown verdicts all leave tests upheld.
#[must_use]
pub fn decode_build_audit(reply: &str, failed_ids: &[String]) -> Vec<AuditDisposition> {
    let expected = failed_ids.iter().cloned().collect::<BTreeSet<_>>();
    let parsed = serde_json::from_str::<Value>(reply).ok();
    let mut verdicts = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    if let Some(rows) = parsed
        .as_ref()
        .and_then(|value| value.get("items"))
        .and_then(Value::as_array)
    {
        for row in rows {
            let Some(id) = row.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !expected.contains(id) {
                continue;
            }
            let Some(verdict) = row.get("verdict").and_then(Value::as_str) else {
                continue;
            };
            let verdict = match verdict {
                "valid" => BuildAuditVerdict::Valid,
                "over_strict" => BuildAuditVerdict::OverStrict,
                "contradicts" => BuildAuditVerdict::Contradicts,
                _ => continue,
            };
            let Some(reason) = row.get("reason").and_then(Value::as_str) else {
                continue;
            };
            if ambiguous.contains(id) {
                continue;
            }
            let item = BuildAuditItem {
                id: id.to_owned(),
                verdict,
                reason: reason.to_owned(),
            };
            if verdicts.insert(id.to_owned(), item).is_some() {
                verdicts.remove(id);
                ambiguous.insert(id.to_owned());
            }
        }
    }
    failed_ids
        .iter()
        .map(|id| {
            let item = verdicts.get(id);
            let verdict = item.map_or(BuildAuditVerdict::Valid, |item| item.verdict);
            AuditDisposition {
                id: id.clone(),
                verdict,
                reason: item.map_or_else(String::new, |item| item.reason.clone()),
                demote: verdict.demotes(),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "auditor_tests.rs"]
mod tests;

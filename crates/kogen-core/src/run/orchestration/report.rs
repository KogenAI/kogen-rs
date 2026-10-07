use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BuildReport {
    pub slug: String,
    pub status: String,
    pub build_id: String,
    pub journal: Vec<Value>,
    pub verdict: Option<String>,
    pub land_policy: String,
    pub advisory_items: Vec<String>,
    pub approval: String,
    pub approved_by: Option<String>,
    pub base: String,
    pub candidate: Option<String>,
    pub landed_sha: Option<String>,
    pub priority: i64,
    pub blocks_on: Vec<String>,
    pub cache_hit_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<Value>>,
    pub credential: CredentialReport,
    pub rungs: Vec<RungReport>,
    pub best_candidate: Option<BestCandidateReport>,
    pub audit: Vec<AuditReport>,
    pub acceptance: Vec<AcceptanceReport>,
    pub checks: Vec<CheckReport>,
    pub model_stages: Vec<ModelStageReport>,
    pub findings: Vec<FindingReport>,
    pub failures: Vec<FailureReport>,
    pub sandbox: SandboxReport,
    pub budget: BudgetReport,
}

impl BuildReport {
    #[must_use]
    pub fn new(slug: impl Into<String>, build_id: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            build_id: build_id.into(),
            land_policy: "green-or-advisory".to_owned(),
            ..Self::default()
        }
    }

    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn set_agents(&mut self, records: Vec<Value>) {
        self.agents = (!records.is_empty()).then_some(records);
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CredentialReport {
    pub source: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RungReport {
    pub rung: String,
    pub model: String,
    pub effort: String,
    pub reason: String,
    pub verdict: String,
    pub diff_lines: usize,
    pub candidate_ref: String,
    pub wall_ms: u64,
    pub tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BestCandidateReport {
    pub rung: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub diff_path: String,
    pub verdict: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AuditReport {
    pub rung: String,
    pub id: String,
    pub verdict: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AcceptanceReport {
    pub id: String,
    pub status: String,
    pub demoted: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CheckReport {
    pub name: String,
    pub status: String,
    pub excused: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ModelStageReport {
    pub stage: String,
    pub attempt: String,
    pub rung: String,
    pub role: String,
    pub model: String,
    pub effort: String,
    pub conversation_id: String,
    pub input: Option<u64>,
    pub cached_input: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
    pub wall_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FindingReport {
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FailureReport {
    pub stage: String,
    pub class: String,
    pub reason: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SandboxReport {
    pub mode: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BudgetReport {
    pub budget_ms: u64,
    pub used_ms: u64,
    pub paused_ms: u64,
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;

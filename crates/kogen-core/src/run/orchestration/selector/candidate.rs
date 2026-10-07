use super::{GatePolicy, GateScore, VerificationVerdict};
use std::cmp::Ordering;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub rung: String,
    pub rung_index: u8,
    /// Completion order breaks ties between repeated attempts on one rung.
    pub order: usize,
    pub model: String,
    pub effort: String,
    pub reason: String,
    pub verdict: VerificationVerdict,
    pub passing_undemoted_items: usize,
    pub blocking_findings: usize,
    pub diff_lines: usize,
    pub candidate_ref: String,
    pub diff_path: String,
    pub wall_ms: u64,
    pub tokens: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateMetadata {
    pub rung: String,
    pub rung_index: u8,
    pub order: usize,
    pub model: String,
    pub effort: String,
    pub reason: String,
    pub diff_lines: usize,
    pub candidate_ref: String,
    pub diff_path: String,
    pub wall_ms: u64,
    pub tokens: Option<u64>,
}

impl Candidate {
    pub fn score(metadata: CandidateMetadata, gate: &GateScore) -> Self {
        Self {
            rung: metadata.rung,
            rung_index: metadata.rung_index,
            order: metadata.order,
            model: metadata.model,
            effort: metadata.effort,
            reason: metadata.reason,
            verdict: gate.verdict,
            passing_undemoted_items: gate.passing_undemoted_items,
            blocking_findings: gate.blocking_findings,
            diff_lines: metadata.diff_lines,
            candidate_ref: metadata.candidate_ref,
            diff_path: metadata.diff_path,
            wall_ms: metadata.wall_ms,
            tokens: metadata.tokens,
        }
    }

    #[must_use]
    pub fn landable(&self, policy: GatePolicy, has_passing_change: bool) -> bool {
        has_passing_change
            && match self.verdict {
                VerificationVerdict::Green => true,
                VerificationVerdict::GreenWithAdvisoryTests => {
                    policy == GatePolicy::GreenOrAdvisory
                }
                VerificationVerdict::Unverified | VerificationVerdict::None => false,
            }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CandidateSelector {
    candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionError;

impl CandidateSelector {
    pub fn offer(&mut self, candidate: Candidate) {
        self.candidates.push(candidate);
    }

    #[must_use]
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    pub fn select(&self) -> Result<&Candidate, SelectionError> {
        self.candidates
            .iter()
            .min_by(|left, right| compare(left, right))
            .ok_or(SelectionError)
    }

    pub fn into_selection(self) -> Result<(Candidate, SelectionReport), SelectionError> {
        let winner = self
            .candidates
            .iter()
            .min_by(|left, right| compare(left, right))
            .ok_or(SelectionError)?
            .clone();
        let report = SelectionReport {
            rung: winner.rung.clone(),
            ref_name: winner.candidate_ref.clone(),
            diff_path: winner.diff_path.clone(),
            verdict: winner.verdict.as_str().to_owned(),
        };
        Ok((winner, report))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionReport {
    pub rung: String,
    pub ref_name: String,
    pub diff_path: String,
    pub verdict: String,
}

fn compare(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .passing_undemoted_items
        .cmp(&left.passing_undemoted_items)
        .then_with(|| left.blocking_findings.cmp(&right.blocking_findings))
        .then_with(|| left.diff_lines.cmp(&right.diff_lines))
        .then_with(|| left.rung_index.cmp(&right.rung_index))
        .then_with(|| left.order.cmp(&right.order))
}

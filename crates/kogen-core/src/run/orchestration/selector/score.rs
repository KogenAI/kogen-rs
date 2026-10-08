use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatePolicy {
    Green,
    GreenOrAdvisory,
}

impl GatePolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::GreenOrAdvisory => "green-or-advisory",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemKind {
    Change,
    Keep,
}

impl ItemKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::Keep => "keep",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemVerdict {
    Pass,
    Fail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationVerdict {
    Green,
    GreenWithAdvisoryTests,
    Unverified,
    None,
}

impl VerificationVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::GreenWithAdvisoryTests => "green-with-advisory-tests",
            Self::Unverified => "unverified",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemResult {
    pub id: String,
    pub kind: ItemKind,
    pub verdict: ItemVerdict,
    pub demoted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckScoreInput {
    pub name: String,
    pub green: bool,
    pub excused: bool,
    pub finding_identities: Vec<String>,
    pub red_without_identity: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateScore {
    pub verdict: VerificationVerdict,
    pub landable: bool,
    pub passing_undemoted_items: usize,
    pub blocking_findings: usize,
}

/// Score a completed gate while keeping the guaranteed receipt separate from
/// the optional advisory policy.
#[must_use]
pub fn score_verification(
    _policy: GatePolicy,
    checks: &[CheckScoreInput],
    items: &[ItemResult],
) -> GateScore {
    if checks.is_empty() && items.is_empty() {
        return GateScore {
            verdict: VerificationVerdict::None,
            landable: false,
            passing_undemoted_items: 0,
            blocking_findings: 0,
        };
    }
    let checks_green = checks.iter().all(|check| check.green || check.excused);
    let items_green =
        !items.is_empty() && items.iter().all(|item| item.verdict == ItemVerdict::Pass);
    let passing_undemoted_items = items
        .iter()
        .filter(|item| item.verdict == ItemVerdict::Pass)
        .count();
    let has_change_pass = items
        .iter()
        .any(|item| item.kind == ItemKind::Change && item.verdict == ItemVerdict::Pass);
    let verdict = if checks_green && items_green {
        VerificationVerdict::Green
    } else {
        VerificationVerdict::Unverified
    };
    let landable = has_change_pass && verdict == VerificationVerdict::Green;
    let identities = checks
        .iter()
        .filter(|check| !check.green && !check.excused)
        .flat_map(|check| check.finding_identities.iter().cloned());
    let no_identity_reds = checks
        .iter()
        .filter(|check| !check.green && !check.excused && check.red_without_identity)
        .count();
    let failed_items = items
        .iter()
        .filter(|item| item.verdict == ItemVerdict::Fail)
        .count();
    GateScore {
        verdict,
        landable,
        passing_undemoted_items,
        blocking_findings: blocking_count(identities, no_identity_reds, failed_items),
    }
}

/// Number of distinct blocking identities, plus each unidentifiable red check
/// and each failing undemoted acceptance item.
#[must_use]
pub fn blocking_count(
    check_finding_identities: impl IntoIterator<Item = String>,
    red_checks_without_identity: usize,
    failing_undemoted_items: usize,
) -> usize {
    check_finding_identities
        .into_iter()
        .collect::<BTreeSet<_>>()
        .len()
        + red_checks_without_identity
        + failing_undemoted_items
}

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{
    CheckScoreInput, GatePolicy, GateScore, ItemKind, ItemResult, ItemVerdict, score_verification,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CheckSnapshot {
    status: &'static str,
    has_ids: bool,
    subset: bool,
    same_exit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ReplayItem {
    kind: String,
    passed: bool,
    demoted: bool,
}

/// The gate slice exercises the L-level demotion and candidate ranking policy.
/// This state is also exposed to production callers through the scoring APIs
/// above; only event decoding lives here for the Quint effect seam.
#[derive(Clone, Debug)]
pub struct GateReplay {
    base_checks: BTreeMap<String, String>,
    current_checks: BTreeMap<String, String>,
    current_snapshots: BTreeMap<String, CheckSnapshot>,
    excused: BTreeMap<String, bool>,
    items: BTreeMap<String, ReplayItem>,
    ledger: String,
    verdict: String,
    landable: bool,
    offers: BTreeMap<String, Offer>,
    winner: String,
    policy: String,
    last: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Offer {
    passed: usize,
    blocking: usize,
    diff: usize,
}

impl Default for GateReplay {
    fn default() -> Self {
        Self::new()
    }
}

impl GateReplay {
    #[must_use]
    pub fn new() -> Self {
        Self {
            policy: "green".to_owned(),
            last: "ok".to_owned(),
            ..Self::default_without_recursion()
        }
    }

    fn default_without_recursion() -> Self {
        Self {
            base_checks: BTreeMap::new(),
            current_checks: BTreeMap::new(),
            current_snapshots: BTreeMap::new(),
            excused: BTreeMap::new(),
            items: BTreeMap::new(),
            ledger: String::new(),
            verdict: String::new(),
            landable: false,
            offers: BTreeMap::new(),
            winner: String::new(),
            policy: String::new(),
            last: String::new(),
        }
    }

    pub fn apply(&mut self, event: &Value) -> Result<(), String> {
        self.last = "ok".to_owned();
        let tag = event
            .get("tag")
            .and_then(Value::as_str)
            .ok_or("event requires string field `tag`")?;
        let value = event.get("value").cloned().unwrap_or(Value::Null);
        match tag {
            "Init" => *self = Self::new(),
            "Baseline" => self.baseline(&value)?,
            "Now" => self.now(&value)?,
            "Rows" => self.rows(&value)?,
            "Demote" => self.demote(&value)?,
            "Score" => self.score(&value)?,
            "Offer" => self.offer(&value)?,
            "Pick" => self.pick(),
            _ => return Err(format!("unknown gate event `{tag}`")),
        }
        Ok(())
    }

    #[must_use]
    pub fn observe(&self) -> Value {
        json!({
            "last": self.last,
            "ledger": self.ledger,
            "verdict": self.verdict,
            "landable": self.landable,
            "winner": self.winner,
            "policy": self.policy,
            "excused": self.excused,
            "items": self.items,
        })
    }

    pub fn score_gate(
        &self,
        policy: GatePolicy,
        check_statuses: impl IntoIterator<Item = (String, bool)>,
    ) -> GateScore {
        let checks = check_statuses
            .into_iter()
            .map(|(name, green)| CheckScoreInput {
                excused: !green && self.excused.get(&name).copied().unwrap_or(false),
                name,
                green,
                finding_identities: Vec::new(),
                red_without_identity: false,
            })
            .collect::<Vec<_>>();
        let items = self
            .items
            .iter()
            .map(|(id, item)| ItemResult {
                id: id.clone(),
                kind: if item.kind == "change" {
                    ItemKind::Change
                } else {
                    ItemKind::Keep
                },
                verdict: if item.passed {
                    ItemVerdict::Pass
                } else {
                    ItemVerdict::Fail
                },
                demoted: item.demoted,
            })
            .collect::<Vec<_>>();
        score_verification(policy, &checks, &items)
    }
}
impl GateReplay {
    fn put_item(&mut self, id: &str, kind: ItemKind, passed: bool, demoted: bool) {
        self.items.insert(
            id.to_owned(),
            ReplayItem {
                kind: kind.as_str().to_owned(),
                passed,
                demoted,
            },
        );
    }

    fn invalidate(&mut self) {
        self.verdict.clear();
        self.landable = false;
    }

    fn error(&mut self, code: &str) -> Result<(), String> {
        self.last = code.to_owned();
        Ok(())
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("event requires string field `{key}`"))
}

fn boolean(value: &Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("event requires boolean field `{key}`"))
}

fn number(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("event requires nonnegative integer field `{key}`"))
}

fn known_check(name: &str) -> bool {
    matches!(name, "c1" | "c2")
}
fn known_item(id: &str) -> bool {
    matches!(id, "A1" | "A2")
}
fn known_status(status: &str) -> bool {
    matches!(
        status,
        "green" | "red" | "unavailable" | "timeout" | "mutating"
    )
}
fn status_name(status: &str) -> &'static str {
    match status {
        "green" => "green",
        "red" => "red",
        "unavailable" => "unavailable",
        "timeout" => "timeout",
        _ => "mutating",
    }
}
fn rung_order(rung: &str) -> u32 {
    rung.parse().unwrap_or(u32::MAX)
}

mod replay_checks;
mod replay_items;
mod replay_selection;

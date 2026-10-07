use super::*;

impl GateReplay {
    pub(super) fn rows(&mut self, value: &Value) -> Result<(), String> {
        let id = string(value, "id")?;
        let kind = string(value, "kind")?;
        let report = string(value, "report")?;
        let runner_down = boolean(value, "runnerDown")?;
        let exit_zero = boolean(value, "exit0")?;
        let mutated = boolean(value, "mutated")?;
        let failed = number(value, "failed")?;
        let rows = number(value, "rows")?;
        let kind = match kind {
            "change" => ItemKind::Change,
            "keep" => ItemKind::Keep,
            _ => return self.error("bad_item"),
        };
        if !known_item(id) {
            return self.error("bad_item");
        }
        let report_is_valid =
            matches!(report, "ok" | "empty" | "malformed") || (report == "missing" && runner_down);
        if !report_is_valid {
            return self.error("bad_report");
        }
        self.invalidate();
        if mutated {
            self.ledger = "tree_mutated".to_owned();
            self.put_item(id, kind, false, false);
        } else if report == "missing" && runner_down {
            self.ledger = "tool_missing".to_owned();
            self.put_item(id, kind, false, false);
        } else if report == "empty" && !exit_zero {
            self.ledger = "acceptance_compile_failed".to_owned();
            self.put_item(id, kind, false, false);
        } else if report == "empty" {
            self.ledger = "no_tagged_tests".to_owned();
            self.put_item(id, kind, false, false);
        } else if report == "malformed" {
            self.ledger = "ledger_invalid".to_owned();
            self.put_item(id, kind, false, false);
        } else if report != "ok" {
            return self.error("bad_report");
        } else {
            let passed = rows >= 1 && failed == 0;
            self.ledger = if passed { "pass" } else { "fail" }.to_owned();
            self.put_item(id, kind, passed, false);
        }
        Ok(())
    }

    pub(super) fn demote(&mut self, value: &Value) -> Result<(), String> {
        let id = string(value, "id")?;
        let verdict = string(value, "verdict")?;
        let Some(_item) = self.items.get(id) else {
            return self.error("unknown_item");
        };
        if !matches!(verdict, "valid" | "over_strict" | "contradicts" | "garbled") {
            return self.error("bad_verdict");
        }
        Ok(())
    }

    pub(super) fn score(&mut self, value: &Value) -> Result<(), String> {
        let policy = string(value, "policy")?;
        if !matches!(policy, "green" | "green-or-advisory") {
            return self.error("bad_policy");
        }
        self.policy = policy.to_owned();
        let checks = self
            .current_checks
            .iter()
            .map(|(name, status)| CheckScoreInput {
                name: name.clone(),
                green: status == "green",
                excused: self.excused.get(name).copied().unwrap_or(false),
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
        let selected_policy = if policy == "green" {
            GatePolicy::Green
        } else {
            GatePolicy::GreenOrAdvisory
        };
        let score = score_verification(selected_policy, &checks, &items);
        self.verdict = score.verdict.as_str().to_owned();
        self.landable = score.landable;
        Ok(())
    }
}

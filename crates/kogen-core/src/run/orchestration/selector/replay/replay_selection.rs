use super::*;

impl GateReplay {
    pub(super) fn offer(&mut self, value: &Value) -> Result<(), String> {
        let rung = string(value, "rung")?;
        let passed = number(value, "passed")? as usize;
        let blocking = number(value, "blocking")? as usize;
        let diff = number(value, "diff")? as usize;
        if rung.parse::<u32>().is_err() || rung.parse::<u32>().is_ok_and(|number| number == 0) {
            return self.error("bad_offer");
        }
        self.offers.insert(
            rung.to_owned(),
            Offer {
                passed,
                blocking,
                diff,
            },
        );
        Ok(())
    }

    pub(super) fn pick(&mut self) {
        let winner = self
            .offers
            .iter()
            .min_by(|(left_key, left), (right_key, right)| {
                right
                    .passed
                    .cmp(&left.passed)
                    .then_with(|| left.blocking.cmp(&right.blocking))
                    .then_with(|| left.diff.cmp(&right.diff))
                    .then_with(|| rung_order(left_key).cmp(&rung_order(right_key)))
            })
            .map(|(rung, _)| rung.clone());
        if let Some(winner) = winner {
            self.winner = winner;
        } else {
            self.last = "no_candidate".to_owned();
        }
    }
}

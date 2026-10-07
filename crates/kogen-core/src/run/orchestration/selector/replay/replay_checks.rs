use super::*;

impl GateReplay {
    pub(super) fn baseline(&mut self, value: &Value) -> Result<(), String> {
        let name = string(value, "name")?;
        let status = string(value, "status")?;
        if !known_check(name) {
            return self.error("unknown_check");
        }
        if !known_status(status) {
            return self.error("bad_status");
        }
        self.base_checks.insert(name.to_owned(), status.to_owned());
        self.invalidate();
        self.remember(name);
        Ok(())
    }

    pub(super) fn now(&mut self, value: &Value) -> Result<(), String> {
        let name = string(value, "name")?;
        let status = string(value, "status")?;
        let has_ids = boolean(value, "hasIds")?;
        let subset = boolean(value, "subset")?;
        let same_exit = boolean(value, "sameExit")?;
        if !known_check(name) {
            return self.error("unknown_check");
        }
        if !known_status(status) {
            return self.error("bad_status");
        }
        self.current_checks
            .insert(name.to_owned(), status.to_owned());
        self.current_snapshots.insert(
            name.to_owned(),
            CheckSnapshot {
                status: status_name(status),
                has_ids,
                subset,
                same_exit,
            },
        );
        self.invalidate();
        let excused = self.is_excused(name);
        self.excused.insert(name.to_owned(), excused);
        Ok(())
    }

    fn is_excused(&self, name: &str) -> bool {
        let Some(base) = self.base_checks.get(name) else {
            return false;
        };
        let Some(now) = self.current_snapshots.get(name) else {
            return false;
        };
        base != "green"
            && base == now.status
            && if now.has_ids {
                now.subset
            } else {
                now.same_exit
            }
    }

    fn remember(&mut self, name: &str) {
        if self.current_snapshots.contains_key(name) {
            let excused = self.is_excused(name);
            self.excused.insert(name.to_owned(), excused);
        }
    }
}

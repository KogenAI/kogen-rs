use super::BudgetReport;

pub const DEFAULT_BUILD_BUDGET_MS: u64 = 3_600_000;
pub const MAX_STAGE_WALL_MS: u64 = 1_800_000;
pub const LANDING_ALLOWANCE_MS: u64 = 600_000;

/// Build wall clock. Provider waits are excluded from both the build and
/// landing allowances; model stages share the remaining build wall.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildBudget {
    budget_ms: u64,
    started_ms: u64,
    paused_since_ms: Option<u64>,
    paused_ms: u64,
    landing_started_ms: Option<u64>,
    landing_pause_base_ms: u64,
}

impl BuildBudget {
    #[must_use]
    pub fn new(started_ms: u64, configured_budget_ms: Option<u64>) -> Self {
        Self {
            budget_ms: configured_budget_ms.unwrap_or(DEFAULT_BUILD_BUDGET_MS),
            started_ms,
            paused_since_ms: None,
            paused_ms: 0,
            landing_started_ms: None,
            landing_pause_base_ms: 0,
        }
    }

    pub fn pause(&mut self, now_ms: u64) -> bool {
        if self.paused_since_ms.is_some() {
            return false;
        }
        self.paused_since_ms = Some(now_ms.max(self.started_ms));
        true
    }

    pub fn resume(&mut self, now_ms: u64) -> bool {
        let Some(started) = self.paused_since_ms.take() else {
            return false;
        };
        self.paused_ms = self
            .paused_ms
            .saturating_add(now_ms.saturating_sub(started));
        true
    }

    pub fn begin_landing(&mut self, now_ms: u64) -> bool {
        if self.landing_started_ms.is_some() {
            return false;
        }
        self.landing_started_ms = Some(now_ms);
        self.landing_pause_base_ms = self.total_paused_ms(now_ms);
        true
    }

    #[must_use]
    pub fn used_ms(&self, now_ms: u64) -> u64 {
        self.active_elapsed(self.started_ms, now_ms)
    }

    #[must_use]
    pub fn paused_ms(&self, now_ms: u64) -> u64 {
        self.total_paused_ms(now_ms)
    }

    #[must_use]
    pub fn remaining_ms(&self, now_ms: u64) -> u64 {
        self.budget_ms.saturating_sub(self.used_ms(now_ms))
    }

    #[must_use]
    pub fn stage_wall_ms(&self, now_ms: u64) -> u64 {
        self.remaining_ms(now_ms).min(MAX_STAGE_WALL_MS)
    }

    #[must_use]
    pub fn is_exhausted(&self, now_ms: u64) -> bool {
        self.remaining_ms(now_ms) == 0
    }

    #[must_use]
    pub fn landing_remaining_ms(&self, now_ms: u64) -> Option<u64> {
        let started = self.landing_started_ms?;
        let paused_since_landing = self
            .total_paused_ms(now_ms)
            .saturating_sub(self.landing_pause_base_ms);
        let elapsed = now_ms
            .saturating_sub(started)
            .saturating_sub(paused_since_landing);
        Some(LANDING_ALLOWANCE_MS.saturating_sub(elapsed))
    }

    #[must_use]
    pub fn report(&self, now_ms: u64) -> BudgetReport {
        BudgetReport {
            budget_ms: self.budget_ms,
            used_ms: self.used_ms(now_ms),
            paused_ms: self.paused_ms(now_ms),
        }
    }

    fn active_elapsed(&self, start_ms: u64, now_ms: u64) -> u64 {
        let end_ms = self.paused_since_ms.unwrap_or(now_ms).min(now_ms);
        end_ms
            .saturating_sub(start_ms)
            .saturating_sub(self.paused_ms)
    }

    fn total_paused_ms(&self, now_ms: u64) -> u64 {
        self.paused_ms.saturating_add(
            self.paused_since_ms
                .map_or(0, |started| now_ms.saturating_sub(started)),
        )
    }
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;

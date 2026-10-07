use serde::{Deserialize, Serialize};

/// Result of a valid, standalone model `finish({})` call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinishAction {
    Continue(String),
    Verify,
}

/// Enforces the two-empty-finish rule for one builder conversation.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FinishPolicy {
    empty_finishes: u8,
}

impl FinishPolicy {
    pub const EMPTY_FINISH_FEEDBACK: &'static str =
        "Kogen found no changed files. Make the requested change before claiming done.";

    #[must_use]
    pub const fn new() -> Self {
        Self { empty_finishes: 0 }
    }

    #[must_use]
    pub const fn empty_finishes(&self) -> u8 {
        self.empty_finishes
    }

    /// `implementation_changed` excludes approved Intent and acceptance files.
    pub fn finish(&mut self, implementation_changed: bool) -> FinishAction {
        if implementation_changed {
            FinishAction::Verify
        } else if self.empty_finishes == 0 {
            self.empty_finishes = 1;
            FinishAction::Continue(Self::EMPTY_FINISH_FEEDBACK.to_owned())
        } else {
            FinishAction::Verify
        }
    }
}

#[cfg(test)]
#[path = "finish_tests.rs"]
mod tests;

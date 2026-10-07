use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuilderModel {
    pub model: String,
    pub effort: String,
}

impl BuilderModel {
    pub fn new(model: impl Into<String>, effort: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            effort: effort.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputKind {
    Plan,
    RequestAndAcceptance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolSet {
    Shell,
    Direct,
    None,
}

impl ToolSet {
    #[must_use]
    pub const fn names(self) -> &'static [&'static str] {
        match self {
            Self::Shell => &["shell", "finish", "tool_output"],
            Self::Direct => &[
                "read",
                "search",
                "edit",
                "write",
                "shell",
                "finish",
                "tool_output",
            ],
            Self::None => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecipeKind {
    Ladder,
    PlanShell,
    Staged,
    Direct,
    DirectEscalate,
    DirectShell,
    EscalateShell,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeRung {
    pub index: u8,
    pub name: &'static str,
    pub builder: BuilderModel,
    pub input: InputKind,
    pub tools: ToolSet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildRecipe {
    pub name: String,
    pub kind: RecipeKind,
    pub edge: bool,
    pub rungs: Vec<RecipeRung>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildRecipeError {
    pub rejected: String,
}

impl fmt::Display for BuildRecipeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "build.recipe must be one of ladder, ladder-diverse, ladder-luna, ladder-sol-low, ladder-sol-medium, ladder-sol-high, plan-shell, staged, direct, direct-escalate, direct-shell, escalate-shell; rejected value {:?}",
            self.rejected
        )
    }
}

impl std::error::Error for BuildRecipeError {}

impl BuildRecipe {
    pub fn parse(name: &str) -> Result<Self, BuildRecipeError> {
        let (base, edge) = name
            .strip_suffix("+edge")
            .map_or((name, false), |value| (value, true));
        let (kind, rungs) = match base {
            "ladder" => (RecipeKind::Ladder, ladder_rungs(false)),
            "ladder-diverse" => (RecipeKind::Ladder, ladder_rungs(true)),
            "ladder-luna" => (
                RecipeKind::Ladder,
                single_model_rungs(BuilderModel::new("gpt-6-luna", "max")),
            ),
            "ladder-sol-low" => (
                RecipeKind::Ladder,
                single_model_rungs(BuilderModel::new("gpt-6.1-sol", "low")),
            ),
            "ladder-sol-medium" => (
                RecipeKind::Ladder,
                single_model_rungs(BuilderModel::new("gpt-6.1-sol", "medium")),
            ),
            "ladder-sol-high" => (
                RecipeKind::Ladder,
                single_model_rungs(BuilderModel::new("gpt-6.1-sol", "high")),
            ),
            "plan-shell" => (
                RecipeKind::PlanShell,
                vec![rung(
                    1,
                    "builder",
                    ladder_models()[0].clone(),
                    InputKind::Plan,
                    ToolSet::Shell,
                )],
            ),
            "staged" => (RecipeKind::Staged, ladder_rungs(false)),
            "direct" => (RecipeKind::Direct, direct_rungs(false, false)),
            "direct-escalate" => (RecipeKind::DirectEscalate, direct_rungs(true, false)),
            "direct-shell" => (RecipeKind::DirectShell, direct_rungs(false, true)),
            "escalate-shell" => (RecipeKind::EscalateShell, direct_rungs(true, true)),
            _ => {
                return Err(BuildRecipeError {
                    rejected: name.to_owned(),
                });
            }
        };
        if edge && !matches!(kind, RecipeKind::Ladder) {
            return Err(BuildRecipeError {
                rejected: name.to_owned(),
            });
        }
        Ok(Self {
            name: name.to_owned(),
            kind,
            edge,
            rungs,
        })
    }

    #[must_use]
    pub fn ladder_default() -> Self {
        Self::parse("ladder").expect("default ladder is a valid recipe")
    }

    #[must_use]
    pub fn rung(&self, index: u8) -> Option<&RecipeRung> {
        self.rungs.iter().find(|rung| rung.index == index)
    }

    /// Apply configured builder roles after resolving the recipe defaults.
    /// R3 and `raw-request` share the configured rung3 model unless a recipe
    /// explicitly supplied a uniform model, in which case role overrides
    /// still remain authoritative.
    pub fn with_role_overrides(
        mut self,
        builder: Option<BuilderModel>,
        rung2: Option<BuilderModel>,
        rung3: Option<BuilderModel>,
    ) -> Self {
        for rung in &mut self.rungs {
            let override_model = match rung.index {
                1 => builder.as_ref(),
                2 => rung2.as_ref(),
                3 | 4 => rung3.as_ref(),
                _ => None,
            };
            if let Some(model) = override_model {
                rung.builder = model.clone();
            }
        }
        self
    }

    /// Hard plans start the first two ladder rungs concurrently by default.
    #[must_use]
    pub fn entry_schedule(
        &self,
        difficulty: Difficulty,
        on_hard_skip_first: bool,
        max_rungs: u8,
    ) -> RungSchedule {
        if self.kind == RecipeKind::Ladder
            && difficulty == Difficulty::Hard
            && max_rungs >= 2
            && !on_hard_skip_first
        {
            RungSchedule::Parallel([
                self.rung(1).expect("first ladder rung").clone(),
                self.rung(2).expect("second ladder rung").clone(),
            ])
        } else {
            RungSchedule::Serial(
                self.rung(
                    if difficulty == Difficulty::Hard && on_hard_skip_first && max_rungs >= 2 {
                        2
                    } else {
                        1
                    },
                )
                .cloned()
                .into_iter()
                .collect(),
            )
        }
    }

    /// Returns the next fresh attempt after a red/unverified result.
    /// `repeat_from` is a zero-based rung index, as in project configuration.
    #[must_use]
    pub fn next_attempt(
        &self,
        completed_index: u8,
        attempt_number: u32,
        budget_left: bool,
        repeat_from: Option<u8>,
    ) -> Option<RungAttempt> {
        if !budget_left || self.rungs.is_empty() {
            return None;
        }
        let next = completed_index.saturating_add(1);
        if let Some(rung) = self.rung(next) {
            return Some(RungAttempt::first(rung));
        }
        let first_repeat_index = repeat_from?;
        if first_repeat_index >= self.rungs.len() as u8 {
            return None;
        }
        let first_repeat_rung = first_repeat_index + 1;
        let offset = completed_index.saturating_sub(self.rungs.len() as u8);
        let cycle_index =
            first_repeat_rung + (offset % (self.rungs.len() as u8 - first_repeat_rung + 1));
        let rung = self.rung(cycle_index)?;
        Some(RungAttempt {
            rung: rung.clone(),
            number: attempt_number.max(2),
            name: format!("{}-{}", rung.name, attempt_number.max(2)),
        })
    }

    #[must_use]
    pub fn stage_count(&self) -> usize {
        match self.kind {
            RecipeKind::PlanShell => 2,
            RecipeKind::Direct | RecipeKind::DirectShell => 1,
            RecipeKind::DirectEscalate | RecipeKind::EscalateShell => self.rungs.len(),
            RecipeKind::Staged => 3 + self.rungs.len(),
            RecipeKind::Ladder => 1 + self.rungs.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Difficulty {
    Easy,
    Hard,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RungSchedule {
    Serial(Vec<RecipeRung>),
    Parallel([RecipeRung; 2]),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RungAttempt {
    pub rung: RecipeRung,
    pub number: u32,
    pub name: String,
}

impl RungAttempt {
    fn first(rung: &RecipeRung) -> Self {
        Self {
            rung: rung.clone(),
            number: 1,
            name: rung.name.to_owned(),
        }
    }
}

mod models;
use models::{direct_rungs, ladder_models, ladder_rungs, rung, single_model_rungs};

#[cfg(test)]
#[path = "recipe_tests.rs"]
mod tests;

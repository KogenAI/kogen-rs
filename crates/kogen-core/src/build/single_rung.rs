//! The default Build recipe, behind the seam used by a later ladder recipe.

use super::approval::ApprovedBuild;
use crate::error::CoreError;
use crate::project::ProjectResolution;

mod execute;
mod landing;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BuildStatus {
    Landed,
    Failed,
    Parked,
    Stopped { class: String, reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BuildOutcome {
    pub status: BuildStatus,
    pub run_id: String,
    pub commit: String,
    pub verdict: String,
    pub advisory_items: Vec<String>,
    pub reason: String,
    pub stderr: String,
    pub has_run: bool,
}

/// A recipe consumes one approved Build and owns its rung policy. The queue
/// depends on this seam, so a multi-rung recipe can replace this implementation
/// without changing claim, dispatch, or terminal-state handling.
pub(super) trait BuildRecipe {
    fn execute(
        &self,
        project: &ProjectResolution,
        approved: &ApprovedBuild,
    ) -> Result<BuildOutcome, CoreError>;
}

struct SingleRungRecipe;

impl BuildRecipe for SingleRungRecipe {
    fn execute(
        &self,
        project: &ProjectResolution,
        approved: &ApprovedBuild,
    ) -> Result<BuildOutcome, CoreError> {
        execute::run(project, approved)
    }
}

pub(super) fn run(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
) -> Result<BuildOutcome, CoreError> {
    SingleRungRecipe.execute(project, approved)
}

pub(super) fn run_witness_build(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &super::config::BuildOptions,
    base_sha: &str,
    run_dir: &std::path::Path,
    store: &crate::run::RunStore,
    snapshot: &mut crate::run::RunSnapshot,
) -> Result<bool, CoreError> {
    execute::run_witness_build(
        project, approved, options, base_sha, run_dir, store, snapshot,
    )
}

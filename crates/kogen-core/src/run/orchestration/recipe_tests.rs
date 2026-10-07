use super::{BuildRecipe, Difficulty, InputKind, RungSchedule, ToolSet};

#[test]
fn default_ladder_has_the_v12_four_rung_recipe() {
    let recipe = BuildRecipe::ladder_default();
    assert_eq!(recipe.rungs.len(), 4);
    assert_eq!(recipe.rung(1).unwrap().name, "builder");
    assert_eq!(recipe.rung(1).unwrap().builder.model, "gpt-6-luna");
    assert_eq!(recipe.rung(2).unwrap().builder.effort, "medium");
    assert_eq!(recipe.rung(3).unwrap().builder.effort, "high");
    assert_eq!(recipe.rung(4).unwrap().name, "raw-request");
    assert_eq!(
        recipe.rung(4).unwrap().input,
        InputKind::RequestAndAcceptance
    );
    assert_eq!(
        recipe.entry_schedule(Difficulty::Hard, false, 4),
        RungSchedule::Parallel([
            recipe.rung(1).unwrap().clone(),
            recipe.rung(2).unwrap().clone(),
        ])
    );
    assert_eq!(recipe.rung(1).unwrap().tools, ToolSet::Shell);
    assert_eq!(
        recipe.entry_schedule(Difficulty::Hard, false, 1),
        RungSchedule::Serial(vec![recipe.rung(1).unwrap().clone()]),
    );
}

#[test]
fn diverse_and_single_model_ladders_preserve_their_named_inputs() {
    let diverse = BuildRecipe::parse("ladder-diverse").unwrap();
    assert_eq!(diverse.rung(2).unwrap().name, "sol-medium-raw");
    assert_eq!(
        diverse.rung(2).unwrap().input,
        InputKind::RequestAndAcceptance
    );

    let uniform = BuildRecipe::parse("ladder-sol-low").unwrap();
    assert_eq!(
        uniform.rungs.iter().map(|r| r.name).collect::<Vec<_>>(),
        ["builder", "fresh-2", "fresh-3", "raw-request"]
    );
    assert!(
        uniform
            .rungs
            .iter()
            .all(|r| r.builder.model == "gpt-6.1-sol" && r.builder.effort == "low")
    );
}

#[test]
fn attempts_repeat_from_sol_high_then_raw_request_after_default_rungs() {
    let recipe = BuildRecipe::ladder_default();
    let first = recipe.next_attempt(4, 2, true, Some(2)).unwrap();
    assert_eq!(first.name, "sol-high-2");
    assert_eq!(first.rung.index, 3);
    let second = recipe.next_attempt(5, 3, true, Some(2)).unwrap();
    assert_eq!(second.name, "raw-request-3");
    assert!(recipe.next_attempt(4, 2, true, None).is_none());
    assert!(recipe.next_attempt(4, 2, false, Some(2)).is_none());
}

#[test]
fn edge_suffix_is_available_only_for_ladder_recipes() {
    let recipe = BuildRecipe::parse("ladder+edge").unwrap();
    assert!(recipe.edge);
    assert!(BuildRecipe::parse("direct+edge").is_err());
    let error = BuildRecipe::parse("unknown").unwrap_err().to_string();
    assert!(error.contains("build.recipe must be one of"));
    assert!(error.contains("unknown"));
}

#[test]
fn role_overrides_apply_to_builder_rung2_and_rung3_models() {
    let recipe = BuildRecipe::ladder_default().with_role_overrides(
        Some(super::BuilderModel::new("custom-builder", "low")),
        Some(super::BuilderModel::new("custom-r2", "medium")),
        Some(super::BuilderModel::new("custom-r3", "high")),
    );
    assert_eq!(recipe.rung(1).unwrap().builder.model, "custom-builder");
    assert_eq!(recipe.rung(2).unwrap().builder.model, "custom-r2");
    assert_eq!(recipe.rung(3).unwrap().builder.model, "custom-r3");
    assert_eq!(recipe.rung(4).unwrap().builder.model, "custom-r3");
}

#[test]
fn recipe_selection_keeps_direct_tools_and_one_stage_shell_behavior() {
    let direct = BuildRecipe::parse("direct").unwrap();
    assert_eq!(
        direct.rung(1).unwrap().tools.names(),
        [
            "read",
            "search",
            "edit",
            "write",
            "shell",
            "finish",
            "tool_output"
        ]
    );
    let shell = BuildRecipe::parse("plan-shell").unwrap();
    assert_eq!(shell.rungs.len(), 1);
    assert_eq!(shell.rung(1).unwrap().tools, ToolSet::Shell);
}

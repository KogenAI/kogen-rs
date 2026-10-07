use super::{BuilderModel, InputKind, RecipeRung, ToolSet};

pub(super) fn ladder_rungs(diverse: bool) -> Vec<RecipeRung> {
    let names = [
        "builder",
        if diverse {
            "sol-medium-raw"
        } else {
            "sol-medium"
        },
        "sol-high",
        "raw-request",
    ];
    ladder_models()
        .into_iter()
        .enumerate()
        .map(|(index, default_model)| {
            let input = if index == 3 || (diverse && index == 1) {
                InputKind::RequestAndAcceptance
            } else {
                InputKind::Plan
            };
            rung(
                index as u8 + 1,
                names[index],
                default_model,
                input,
                ToolSet::Shell,
            )
        })
        .collect()
}

pub(super) fn single_model_rungs(model: BuilderModel) -> Vec<RecipeRung> {
    ["builder", "fresh-2", "fresh-3", "raw-request"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            rung(
                index as u8 + 1,
                name,
                model.clone(),
                if index == 3 {
                    InputKind::RequestAndAcceptance
                } else {
                    InputKind::Plan
                },
                ToolSet::Shell,
            )
        })
        .collect()
}

pub(super) fn direct_rungs(escalate: bool, shell_only: bool) -> Vec<RecipeRung> {
    let models = ladder_models();
    let tools = if shell_only {
        ToolSet::Shell
    } else {
        ToolSet::Direct
    };
    let mut rungs = vec![rung(
        1,
        "builder",
        models[0].clone(),
        InputKind::RequestAndAcceptance,
        tools,
    )];
    if escalate {
        rungs.push(rung(
            2,
            "fresh-2",
            models[1].clone(),
            InputKind::RequestAndAcceptance,
            tools,
        ));
        rungs.push(rung(
            3,
            "fresh-3",
            models[2].clone(),
            InputKind::RequestAndAcceptance,
            tools,
        ));
    }
    rungs
}

pub(super) fn ladder_models() -> [BuilderModel; 4] {
    [
        BuilderModel::new("gpt-6-luna", "max"),
        BuilderModel::new("gpt-6.1-sol", "medium"),
        BuilderModel::new("gpt-6.1-sol", "high"),
        BuilderModel::new("gpt-6.1-sol", "high"),
    ]
}

pub(super) fn rung(
    index: u8,
    name: &'static str,
    builder: BuilderModel,
    input: InputKind,
    tools: ToolSet,
) -> RecipeRung {
    RecipeRung {
        index,
        name,
        builder,
        input,
        tools,
    }
}

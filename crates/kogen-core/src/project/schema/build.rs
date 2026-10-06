use super::{get, require_string, unknown_keys};
use serde_yaml::{Mapping, Value};

pub(super) fn validate(value: &Value, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push("build must be a map".to_owned());
        return;
    };
    let allowed = [
        "recipe",
        "roles",
        "wall_minutes",
        "edge_tests",
        "model_fallback",
        "context_bytes",
        "plan_max_words",
        "tool_result_tokens",
        "model_generation_tokens",
        "luna_provider_mode",
        "ladder",
        "land",
        "budget_ms",
        "fallback",
    ];
    unknown_keys(map, &allowed, "build", issues);
    if let Some(recipe) = get(map, "recipe") {
        require_string(recipe, "build.recipe", issues);
    }
    if let Some(roles) = get(map, "roles") {
        validate_roles(roles, issues);
    }
    if let Some(ladder) = get(map, "ladder") {
        validate_ladder(ladder, issues);
    }
    if let Some(land) = get(map, "land")
        && !matches!(land.as_str(), Some("green-or-advisory" | "green"))
    {
        issues.push("build.land must be green-or-advisory or green".to_owned());
    }
    if let Some(value) = get(map, "fallback") {
        validate_fallback(value, issues);
    }
    validate_positive_integers(map, issues);
    if let Some(value) = get(map, "plan_max_words")
        && !value.as_u64().is_some_and(|n| (300..=2000).contains(&n))
    {
        issues.push("plan_max_words must be an integer from 300 to 2000".to_owned());
    }
    for field in ["edge_tests", "model_fallback"] {
        if let Some(value) = get(map, field)
            && !matches!(value, Value::Bool(_))
        {
            issues.push(format!("build.{field} must be true or false"));
        }
    }
    if let Some(value) = get(map, "luna_provider_mode")
        && !matches!(value.as_str(), Some("responses" | "lite"))
    {
        issues.push("build.luna_provider_mode must be responses or lite".to_owned());
    }
}

fn validate_positive_integers(map: &Mapping, issues: &mut Vec<String>) {
    for field in [
        "wall_minutes",
        "budget_ms",
        "context_bytes",
        "tool_result_tokens",
        "model_generation_tokens",
    ] {
        if let Some(value) = get(map, field)
            && value.as_u64().is_none()
        {
            issues.push(format!("build.{field} must be a positive integer"));
        }
    }
}

fn validate_roles(value: &Value, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push("build.roles must be a map".to_owned());
        return;
    };
    for (key, value) in map {
        let role = key.as_str().unwrap_or("?");
        if !matches!(
            role,
            "builder"
                | "planner"
                | "shaper"
                | "auditor"
                | "reviewer"
                | "context"
                | "fallback_shaper"
                | "rung2"
                | "rung3"
        ) {
            issues.push(format!("build.roles has unknown role \"{role}\""));
            continue;
        }
        validate_role(value, &format!("build.roles.{role}"), issues);
    }
}

fn validate_role(value: &Value, path: &str, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push(format!("{path} must be a map"));
        return;
    };
    unknown_keys(map, &["model", "effort"], path, issues);
    for field in ["model", "effort"] {
        match get(map, field) {
            Some(value) if value.as_str().is_some() => {}
            Some(_) => issues.push(format!("{path}.{field} must be a string")),
            None => issues.push(format!("{path} is missing required key `{field}`")),
        }
    }
}

fn validate_ladder(value: &Value, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push("build.ladder must be a map".to_owned());
        return;
    };
    unknown_keys(
        map,
        &["max_rungs", "experimental_r4"],
        "build.ladder",
        issues,
    );
    if get(map, "max_rungs").is_some_and(|value| value.as_u64().is_none()) {
        issues.push("build.ladder.max_rungs must be a positive integer".to_owned());
    }
    if get(map, "experimental_r4").is_some_and(|value| !matches!(value, Value::Bool(_))) {
        issues.push("build.ladder.experimental_r4 must be true or false".to_owned());
    }
}

fn validate_fallback(value: &Value, issues: &mut Vec<String>) {
    let Some(map) = value.as_mapping() else {
        issues.push("build.fallback must be a map".to_owned());
        return;
    };
    for (key, role) in map {
        let name = key.as_str().unwrap_or("?");
        validate_role(role, &format!("build.fallback.{name}"), issues);
    }
}

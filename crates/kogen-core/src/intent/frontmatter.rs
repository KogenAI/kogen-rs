use super::{IntentParseError, parse_error};
use crate::project::yaml;
use serde_yaml::{Mapping, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Contract {
    pub name: String,
    pub path: String,
    pub contains: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frontmatter {
    pub title: String,
    pub size: String,
    pub domains: Vec<String>,
    pub changes_gate: bool,
    pub limits: Vec<String>,
    pub blocks_on: Vec<String>,
    pub priority: i64,
    pub assumptions: Vec<Contract>,
    pub shared_contracts: Vec<Contract>,
    pub source: Option<String>,
}

pub(super) fn parse_frontmatter(text: &str) -> Result<Frontmatter, IntentParseError> {
    let map = yaml::parse(text.as_bytes())
        .map_err(|error| parse_error(error.line.unwrap_or(1) + 1, error.message))?;
    let Some(map) = map.as_mapping() else {
        return Err(parse_error(2, "frontmatter must be a YAML map"));
    };
    let allowed = [
        "title",
        "size",
        "domains",
        "changes_gate",
        "limits",
        "blocks_on",
        "priority",
        "assumptions",
        "shared_contracts",
        "source",
    ];
    let mut errors = Vec::new();
    for key in map.keys() {
        let Some(key) = key.as_str() else { continue };
        if !allowed.contains(&key) {
            errors.push(parse_error(
                frontmatter_line(text, key),
                format!("unknown frontmatter key \"{key}\""),
            ));
        }
    }
    for required in ["title", "size", "domains"] {
        if get(map, required).is_none() {
            errors.push(parse_error(
                2,
                format!("frontmatter is missing required key `{required}`"),
            ));
        }
    }
    let title = recover(string_field(map, text, "title"), &mut errors);
    let size = recover(string_field(map, text, "size"), &mut errors);
    let domains = recover(string_list_field(map, text, "domains"), &mut errors);
    let changes_gate = match get(map, "changes_gate") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            errors.push(parse_error(
                frontmatter_line(text, "changes_gate"),
                "frontmatter `changes_gate` must be true or false",
            ));
            false
        }
    };
    let limits = recover(string_list_field(map, text, "limits"), &mut errors);
    let blocks_on = recover(string_list_field(map, text, "blocks_on"), &mut errors);
    let priority = match get(map, "priority") {
        None => 0,
        Some(value) => match value.as_i64() {
            Some(priority) => priority,
            None => {
                errors.push(parse_error(
                    frontmatter_line(text, "priority"),
                    "frontmatter `priority` must be an integer",
                ));
                0
            }
        },
    };
    let assumptions = recover(contracts_field(map, text, "assumptions"), &mut errors);
    let shared_contracts = recover(contracts_field(map, text, "shared_contracts"), &mut errors);
    let source = match get(map, "source") {
        None => None,
        Some(value) => match value.as_str() {
            Some(source) => Some(source.to_owned()),
            None => {
                errors.push(parse_error(
                    frontmatter_line(text, "source"),
                    "frontmatter `source` must be a string",
                ));
                None
            }
        },
    };
    if let Some(error) = errors.into_iter().min_by_key(|error| error.0.line) {
        return Err(error);
    }
    Ok(Frontmatter {
        title,
        size,
        domains,
        changes_gate,
        limits,
        blocks_on,
        priority,
        assumptions,
        shared_contracts,
        source,
    })
}

fn recover<T: Default>(
    result: Result<T, IntentParseError>,
    errors: &mut Vec<IntentParseError>,
) -> T {
    result.unwrap_or_else(|error| {
        errors.push(error);
        T::default()
    })
}

fn string_field(map: &Mapping, text: &str, field: &str) -> Result<String, IntentParseError> {
    get(map, field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            parse_error(
                frontmatter_line(text, field),
                format!("frontmatter `{field}` must be a string"),
            )
        })
}

fn string_list_field(
    map: &Mapping,
    text: &str,
    field: &str,
) -> Result<Vec<String>, IntentParseError> {
    let Some(value) = get(map, field) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_sequence() else {
        return Err(parse_error(
            frontmatter_line(text, field),
            format!("frontmatter `{field}` must be a list"),
        ));
    };
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                parse_error(
                    frontmatter_line(text, field),
                    format!("frontmatter `{field}` must contain only strings"),
                )
            })
        })
        .collect()
}

fn contracts_field(
    map: &Mapping,
    text: &str,
    field: &str,
) -> Result<Vec<Contract>, IntentParseError> {
    let Some(value) = get(map, field) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_sequence() else {
        return Err(parse_error(
            frontmatter_line(text, field),
            format!("frontmatter `{field}` must be a list"),
        ));
    };
    values
        .iter()
        .map(|value| {
            let Some(contract) = value.as_mapping() else {
                return Err(parse_error(
                    frontmatter_line(text, field),
                    format!("frontmatter `{field}` must contain only maps"),
                ));
            };
            let name = contract_string(contract, text, field, "name")?;
            let path = contract_string(contract, text, field, "path")?;
            let contains = contract_string(contract, text, field, "contains")?;
            Ok(Contract {
                name,
                path,
                contains,
            })
        })
        .collect()
}

fn contract_string(
    map: &Mapping,
    text: &str,
    field: &str,
    name: &str,
) -> Result<String, IntentParseError> {
    get(map, name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            parse_error(
                frontmatter_line(text, field),
                format!("frontmatter `{field}` entries require string `{name}`"),
            )
        })
}

fn frontmatter_line(text: &str, key: &str) -> usize {
    text.lines()
        .position(|line| {
            let trimmed = line.trim_start();
            if trimmed.len() != line.len() {
                return false;
            }
            [key.to_owned(), format!("\"{key}\""), format!("'{key}'")]
                .iter()
                .any(|form| {
                    trimmed
                        .strip_prefix(form)
                        .is_some_and(|tail| tail.trim_start().starts_with(':'))
                })
        })
        .map_or(2, |index| index + 2)
}

fn get<'a>(map: &'a Mapping, key: &str) -> Option<&'a Value> {
    map.get(Value::String(key.to_owned()))
}

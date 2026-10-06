//! Canonical tool schemas, allowlists, and production tool dispatch.

#[path = "tools/budget.rs"]
mod budget;
#[path = "tools/operations.rs"]
mod operations;
#[path = "tools/path.rs"]
mod path;

use serde_json::{Value, json};
use std::fs;
use std::path::Path;

use crate::run::{ChildEnvironment, ProcessPort};

use super::ModelToolCall;

type RequiredArgument = (&'static str, fn(&Value) -> bool);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolRole {
    BuilderShell,
    BuilderDirect,
    Shaper,
    Planner,
    Auditor,
}

impl ToolRole {
    #[must_use]
    pub fn allowed(self) -> &'static [&'static str] {
        match self {
            Self::BuilderShell => &["shell", "finish", "tool_output"],
            Self::BuilderDirect => &[
                "edit",
                "finish",
                "read",
                "search",
                "shell",
                "tool_output",
                "write",
            ],
            Self::Shaper => &["read", "search", "write"],
            Self::Planner | Self::Auditor => &[],
        }
    }
}

pub struct ToolContext<'a> {
    pub role: ToolRole,
    pub workspace: &'a Path,
    pub run_dir: &'a Path,
    pub shaper_write_paths: [&'a str; 2],
    pub result_tokens: u64,
    pub process: Option<&'a dyn ProcessPort>,
    pub environment: ChildEnvironment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolError {
    NotAllowed,
    InvalidArguments,
    FinishGuard,
    PathEscape,
    MissingFile,
    BinaryOrInvalidUtf8,
    InvalidLimit,
    ShaperScope(String),
    UnknownOutputHandle,
    Io(String),
    Process(String),
}

impl ToolError {
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::NotAllowed => "ERROR (tool_not_allowed): This stage does not allow the requested tool.".to_owned(),
            Self::InvalidArguments => "ERROR (invalid_arguments): Tool arguments do not match the schema.".to_owned(),
            Self::FinishGuard => "finish requires an empty object and must be the only tool call. Continue implementing, then call finish alone with {}.".to_owned(),
            Self::PathEscape => "Path escapes the worktree.".to_owned(),
            Self::MissingFile => "ERROR: File does not exist.".to_owned(),
            Self::BinaryOrInvalidUtf8 => "ERROR: File is binary or is not UTF-8 text.".to_owned(),
            Self::InvalidLimit => "ERROR: limit must be between 1 and 400.".to_owned(),
            Self::ShaperScope(paths) => format!("ERROR: Write target is outside the shaper's two-file scope. Allowed paths: {paths}."),
            Self::UnknownOutputHandle => "ERROR: Unknown or unavailable tool-output handle.".to_owned(),
            Self::Io(detail) | Self::Process(detail) => detail.clone(),
        }
    }
}

/// The canonical schema list is shared across roles; the request's allowed
/// tools are restricted separately by `tool_choice`.
#[must_use]
pub fn canonical_tool_schemas() -> Vec<Value> {
    [
        schema(
            "edit",
            "Replace one exact text span in a workspace file.",
            &["path", "old", "new"],
            json!({"path":string(),"old":string(),"new":string()}),
            false,
        ),
        schema("finish", "Finish the implementation and request the gate.", &[], json!({}), true),
        schema(
            "read",
            "Read a UTF-8 workspace file by line range.",
            &["path"],
            json!({"path":string(),"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1,"maximum":400}}),
            false,
        ),
        schema(
            "search",
            "Search workspace files with ripgrep.",
            &["pattern"],
            json!({"pattern":string(),"path":string()}),
            false,
        ),
        schema("shell", "Run a shell command in the isolated workspace.", &["cmd"], json!({"cmd":string()}), false),
        schema(
            "tool_output",
            "Retrieve a stored tool result by handle and byte range.",
            &["handle"],
            json!({"handle":string(),"output_offset":{"type":"integer","minimum":0},"output_limit":{"type":"integer","minimum":0}}),
            false,
        ),
        schema("write", "Write exact UTF-8 text to a workspace file.", &["path", "content"], json!({"path":string(),"content":string()}), false),
    ]
    .into_iter()
    .collect()
}

fn schema(
    name: &str,
    description: &str,
    required: &[&str],
    properties: Value,
    strict: bool,
) -> Value {
    json!({
        "type":"function",
        "name":name,
        "description":description,
        "parameters":{
            "type":"object",
            "properties":properties,
            "required":required,
            "additionalProperties":false
        },
        "strict":strict
    })
}

fn string() -> Value {
    json!({"type":"string"})
}

pub fn dispatch(
    context: &ToolContext<'_>,
    call: &ModelToolCall,
    call_count: usize,
) -> Result<String, ToolError> {
    if !context.role.allowed().contains(&call.name.as_str()) {
        return Err(ToolError::NotAllowed);
    }
    if call.name == "finish" {
        return if call
            .arguments
            .as_object()
            .is_some_and(|args| args.is_empty())
            && call_count == 1
        {
            Ok("Completion requested. Kogen will run the gate.".to_owned())
        } else {
            Err(ToolError::FinishGuard)
        };
    }
    validate_arguments(&call.name, &call.arguments)?;
    let workspace = fs::canonicalize(context.workspace).map_err(io_error)?;
    let full_result = match call.name.as_str() {
        "shell" => {
            operations::run_shell(context, &workspace, call.arguments["cmd"].as_str().unwrap())?
        }
        "read" => operations::read_file(
            &workspace,
            call.arguments["path"].as_str().unwrap(),
            call.arguments
                .get("offset")
                .and_then(Value::as_u64)
                .unwrap_or(1) as usize,
            call.arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize,
        )?,
        "search" => operations::run_search(context, &workspace, &call.arguments)?,
        "write" => operations::write_file(context, &workspace, &call.arguments)?,
        "edit" => operations::edit_file(context, &workspace, &call.arguments)?,
        "tool_output" => budget::read_output(
            context.run_dir,
            call.arguments["handle"].as_str().unwrap(),
            call.arguments
                .get("output_offset")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            call.arguments.get("output_limit").and_then(Value::as_u64),
        )?,
        _ => return Err(ToolError::NotAllowed),
    };
    budget::bound_result(context.run_dir, &full_result, context.result_tokens)
}

fn validate_arguments(name: &str, args: &Value) -> Result<(), ToolError> {
    let Some(args) = args.as_object() else {
        return Err(ToolError::InvalidArguments);
    };
    let required: &[RequiredArgument] = match name {
        "shell" => &[("cmd", Value::is_string)],
        "read" => &[("path", Value::is_string)],
        "search" => &[("pattern", Value::is_string)],
        "write" => &[("path", Value::is_string), ("content", Value::is_string)],
        "edit" => &[
            ("path", Value::is_string),
            ("old", Value::is_string),
            ("new", Value::is_string),
        ],
        "tool_output" => &[("handle", Value::is_string)],
        _ => return Err(ToolError::InvalidArguments),
    };
    if required
        .iter()
        .any(|(key, is_valid)| args.get(*key).is_none_or(|value| !is_valid(value)))
    {
        return Err(ToolError::InvalidArguments);
    }
    if name == "read"
        && args
            .get("limit")
            .and_then(Value::as_u64)
            .is_some_and(|limit| !(1..=400).contains(&limit))
    {
        return Err(ToolError::InvalidLimit);
    }
    let optional_strings: &[&str] = match name {
        "search" => &["path"],
        _ => &[],
    };
    if optional_strings
        .iter()
        .any(|key| args.get(*key).is_some_and(|value| !value.is_string()))
    {
        return Err(ToolError::InvalidArguments);
    }
    let optional_integers: &[&str] = match name {
        "read" => &["offset", "limit"],
        "tool_output" => &["output_offset", "output_limit"],
        _ => &[],
    };
    if optional_integers.iter().any(|key| {
        args.get(*key)
            .is_some_and(|value| !value.as_u64().is_some())
    }) {
        return Err(ToolError::InvalidArguments);
    }
    Ok(())
}

fn io_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::Io(error.to_string())
}

#[cfg(test)]
#[path = "tools/tests.rs"]
mod tests;

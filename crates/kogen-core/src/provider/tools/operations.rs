//! Workspace file, shell, and search tool implementations.

use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use crate::run::{
    DEFAULT_SHELL_TIMEOUT, ProcessRequest, ScriptError, StdinSource, run_private_script,
};

use super::{ToolContext, ToolError, ToolRole, path};

pub(super) fn read_file(
    root: &Path,
    path: &str,
    offset: usize,
    limit: usize,
) -> Result<String, ToolError> {
    if !(1..=400).contains(&limit) {
        return Err(ToolError::InvalidLimit);
    }
    let path = path::resolve(root, path, true)?;
    let bytes = fs::read(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ToolError::MissingFile
        } else {
            io_error(error)
        }
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ToolError::BinaryOrInvalidUtf8)?;
    let lines: Vec<_> = text.lines().collect();
    if offset == 0 {
        return Err(ToolError::InvalidArguments);
    }
    let mut output = format!("{}:\n", path::display_relative(root, &path));
    let end = (offset - 1).saturating_add(limit).min(lines.len());
    for (index, line) in lines.iter().enumerate().take(end).skip(offset - 1) {
        output.push_str(&format!("{}: {}\n", index + 1, line));
    }
    if end < lines.len() {
        output.push_str(&format!("\n[continue with offset={}]", end + 1));
    }
    Ok(output)
}

pub(super) fn write_file(
    context: &ToolContext<'_>,
    root: &Path,
    args: &Value,
) -> Result<String, ToolError> {
    let requested = args["path"].as_str().unwrap();
    let target = path::resolve(root, requested, false)?;
    ensure_shaper_scope(context, root, requested, &target)?;
    let contents = args["content"].as_str().unwrap();
    fs::write(&target, contents.as_bytes()).map_err(io_error)?;
    Ok(format!("Wrote {}.", path::display_relative(root, &target)))
}

pub(super) fn edit_file(
    context: &ToolContext<'_>,
    root: &Path,
    args: &Value,
) -> Result<String, ToolError> {
    let requested = args["path"].as_str().unwrap();
    let target = path::resolve(root, requested, true)?;
    ensure_shaper_scope(context, root, requested, &target)?;
    let bytes = fs::read(&target).map_err(io_error)?;
    let mut contents = String::from_utf8(bytes).map_err(|_| ToolError::BinaryOrInvalidUtf8)?;
    let old = args["old"].as_str().unwrap();
    if old.is_empty() || !contents.contains(old) {
        return Err(ToolError::InvalidArguments);
    }
    contents = contents.replacen(old, args["new"].as_str().unwrap(), 1);
    fs::write(&target, contents.as_bytes()).map_err(io_error)?;
    Ok(format!("Edited {}.", path::display_relative(root, &target)))
}

fn ensure_shaper_scope(
    context: &ToolContext<'_>,
    root: &Path,
    requested: &str,
    target: &Path,
) -> Result<(), ToolError> {
    if context.role != ToolRole::Shaper {
        return Ok(());
    }
    let rel = path::display_relative(root, target);
    if !context.shaper_write_paths.contains(&rel.as_str()) || requested.starts_with('/') {
        return Err(ToolError::ShaperScope(format!(
            "{}, {}",
            context.shaper_write_paths[0], context.shaper_write_paths[1]
        )));
    }
    Ok(())
}

pub(super) fn run_shell(
    context: &ToolContext<'_>,
    root: &Path,
    command: &str,
) -> Result<String, ToolError> {
    let runner = context
        .process
        .ok_or_else(|| ToolError::Process("shell process port is unavailable".to_owned()))?;
    let result = run_private_script(
        runner,
        command.as_bytes(),
        root,
        context.run_dir,
        context.environment.clone(),
    )
    .map_err(script_error)?;
    let mut output = read_process_log(&result.log_path).unwrap_or_else(|_| {
        let captured = String::from_utf8_lossy(&result.output_tail);
        format!("[process log unavailable; captured tail may be incomplete]\n{captured}")
    });
    if result.timed_out {
        output = format!("timed out after 120 seconds\n{output}");
    } else {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&format!("exit {}\n", result.exit_status.unwrap_or(-1)));
    }
    Ok(output)
}

pub(super) fn run_search(
    context: &ToolContext<'_>,
    root: &Path,
    args: &Value,
) -> Result<String, ToolError> {
    let runner = context
        .process
        .ok_or_else(|| ToolError::Process("search process port is unavailable".to_owned()))?;
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let target = path::resolve(root, path, true)?;
    let pattern = args["pattern"].as_str().unwrap();
    let relative = path::display_relative(root, &target);
    let mut request = ProcessRequest::new("rg", root, context.run_dir);
    request.args = vec![
        OsString::from("--line-number"),
        OsString::from("--"),
        OsString::from(pattern),
        OsString::from(relative),
    ];
    request.env = context.environment.clone();
    request.timeout = DEFAULT_SHELL_TIMEOUT;
    request.stdin = StdinSource::Null;
    request.log_name = "search".to_owned();
    let result = match runner.run(request.clone()) {
        Ok(result) if !result.unavailable => result,
        Ok(_) | Err(_) => {
            request.program = OsString::from("grep");
            request.args = vec![
                OsString::from("-R"),
                OsString::from("-n"),
                OsString::from("--"),
                OsString::from(pattern),
                OsString::from(path::display_relative(root, &target)),
            ];
            match runner.run(request) {
                Ok(result) if !result.unavailable => result,
                _ => {
                    return Err(ToolError::Process(
                        "search process port is unavailable".to_owned(),
                    ));
                }
            }
        }
    };
    let output = read_process_log(&result.log_path).unwrap_or_default();
    if output.is_empty() {
        Ok("No matches.".to_owned())
    } else {
        Ok(output)
    }
}

fn read_process_log(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    if let Ok(text) = std::str::from_utf8(&bytes) {
        Ok(text.to_owned())
    } else {
        use base64::Engine as _;
        Ok(format!(
            "[non-UTF-8 output, base64 encoded]\n{}\n",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }
}

fn script_error(error: ScriptError) -> ToolError {
    ToolError::Process(error.to_string())
}

fn io_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::Io(error.to_string())
}

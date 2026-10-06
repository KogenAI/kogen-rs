//! Normalization and pure shaping validation rules.

use super::ShapeWarning;
use crate::intent::{Intent, IntentParseError, LintIssue, LintSeverity};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ValidationFailure {
    pub reason: &'static str,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub(super) struct ParsedShape {
    pub intent: Intent,
    pub bytes: Vec<u8>,
    pub style: Vec<LintIssue>,
}

pub(super) fn normalize(generated: &[u8], request: &[u8]) -> Result<Vec<u8>, ValidationFailure> {
    let text = std::str::from_utf8(generated).map_err(|_| ValidationFailure {
        reason: "intent_parse_failed",
        detail: "Intent is not valid UTF-8".to_owned(),
    })?;
    let request_heading = heading_start(text, "Request");
    let mut prefix = text[..request_heading.unwrap_or(text.len())].to_owned();
    normalize_approach(&mut prefix);
    let mut output = prefix.into_bytes();
    if output.ends_with(b"\n") {
        output.push(b'\n');
    } else {
        output.extend_from_slice(b"\n\n");
    }
    output.extend_from_slice(b"## Request\n");
    output.extend_from_slice(request);
    Ok(output)
}

pub(super) fn parse_and_lint(slug: &str, bytes: &[u8]) -> Result<ParsedShape, ValidationFailure> {
    parse_and_lint_mode(slug, bytes, false)
}

fn parse_and_lint_mode(
    slug: &str,
    bytes: &[u8],
    allow_no_change_item: bool,
) -> Result<ParsedShape, ValidationFailure> {
    let prefix_end = heading_end(bytes, "Request").unwrap_or(bytes.len());
    let intent = Intent::parse(slug, &bytes[..prefix_end]).map_err(parse_error)?;
    let issues = intent.lint_for_shaping();
    if issues.iter().any(|finding| {
        finding.severity == LintSeverity::Error
            && !(allow_no_change_item && finding.rule == "no_change_item")
    }) {
        let details = issues
            .iter()
            .filter(|finding| {
                finding.severity == LintSeverity::Error
                    && !(allow_no_change_item && finding.rule == "no_change_item")
            })
            .map(|finding| format!("{}: {}", finding.rule, render_issue(finding)))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ValidationFailure {
            reason: "intent_lint_failed",
            detail: details,
        });
    }
    let style = issues
        .into_iter()
        .filter(|finding| finding.severity == LintSeverity::Style)
        .collect();
    Ok(ParsedShape {
        intent,
        bytes: bytes.to_vec(),
        style,
    })
}

pub(super) fn undeclared_gate_path(
    intent: &Intent,
    test: &[u8],
    gate_paths: &[String],
) -> Option<String> {
    let test = String::from_utf8_lossy(test);
    gate_paths
        .iter()
        .find(|path| intent.notes.contains(path.as_str()) || test.contains(path.as_str()))
        .cloned()
}

pub(super) fn reclassify(
    slug: &str,
    bytes: &[u8],
    results: &BTreeMap<String, bool>,
) -> Result<(Vec<u8>, Vec<ShapeWarning>, bool), ValidationFailure> {
    let mut parsed = parse_and_lint(slug, bytes)?;
    let mut failed_keep = Vec::new();
    let mut passed_change = Vec::new();
    let mut prefix = intent_prefix(&parsed.bytes);
    let text = std::str::from_utf8(&prefix).map_err(|_| ValidationFailure {
        reason: "intent_parse_failed",
        detail: "Intent is not valid UTF-8".to_owned(),
    })?;
    let verify_rows: BTreeMap<usize, (&str, bool)> = parsed
        .intent
        .verify
        .iter()
        .map(|item| (item.line, (item.id.as_str(), item.is_keep())))
        .collect();
    let mut rewritten = String::with_capacity(text.len());
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let Some((id, was_keep)) = verify_rows.get(&(index + 1)) else {
            rewritten.push_str(line);
            continue;
        };
        let Some(passed) = results.get(*id).copied() else {
            rewritten.push_str(line);
            continue;
        };
        if passed == *was_keep {
            rewritten.push_str(line);
            continue;
        }
        let line_body = line.strip_suffix('\n').unwrap_or(line);
        let carriage_return = line_body.ends_with('\r');
        let content = line_body.strip_suffix('\r').unwrap_or(line_body);
        let new_content = change_keep_modifier(content, passed);
        rewritten.push_str(&new_content);
        if carriage_return {
            rewritten.push('\r');
        }
        if line.ends_with('\n') {
            rewritten.push('\n');
        }
        if passed {
            passed_change.push((*id).to_owned());
        } else {
            failed_keep.push((*id).to_owned());
        }
    }
    prefix = rewritten.into_bytes();
    let updated = append_request(&prefix, request_part(bytes));
    parsed = parse_and_lint_mode(slug, &updated, true)?;
    let mut warnings = Vec::new();
    if !failed_keep.is_empty() {
        let message = reclassified_message(&failed_keep, "test");
        warnings.push(ShapeWarning {
            code: "shape_reclassified".to_owned(),
            item_ids: failed_keep,
            message,
        });
    }
    if !passed_change.is_empty() {
        let message = reclassified_message(&passed_change, "test keep");
        warnings.push(ShapeWarning {
            code: "shape_reclassified".to_owned(),
            item_ids: passed_change,
            message,
        });
    }
    let any_red_change = parsed
        .intent
        .verify
        .iter()
        .any(|item| item.is_change() && results.get(&item.id).is_some_and(|passed| !*passed));
    Ok((updated, warnings, any_red_change))
}

fn reclassified_message(item_ids: &[String], kind: &str) -> String {
    let verb = if item_ids.len() == 1 { "was" } else { "were" };
    format!("{} {verb} reclassified as {kind}", item_ids.join(", "))
}

pub(super) fn render_issue(issue: &LintIssue) -> String {
    match issue.line {
        Some(line) => format!("line {line}: {}", issue.message),
        None => issue.message.clone(),
    }
}

fn parse_error(error: IntentParseError) -> ValidationFailure {
    ValidationFailure {
        reason: "intent_parse_failed",
        detail: error.to_string(),
    }
}

fn normalize_approach(prefix: &mut String) {
    let Some((start, end)) = section_range(prefix, "Notes") else {
        return;
    };
    let body = &prefix[start..end];
    let Some(normalized) = super::super::style::normalized_notes(body) else {
        return;
    };
    let leading = body.len() - body.trim_start().len();
    let mut result = String::with_capacity(normalized.len() + leading);
    result.push_str(&body[..leading]);
    result.push_str(&normalized);
    prefix.replace_range(start..end, &result);
}

fn section_range(text: &str, section: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut body_start = None;
    for line in text.split_inclusive('\n') {
        let line_end = offset + line.len();
        if is_heading(line, section) {
            body_start = Some(line_end);
        } else if body_start.is_some() && known_section(line) {
            return Some((body_start?, offset));
        }
        offset = line_end;
    }
    body_start.map(|start| (start, text.len()))
}

fn heading_start(text: &str, section: &str) -> Option<usize> {
    heading_start_bytes(text.as_bytes(), section)
}

fn heading_end(bytes: &[u8], section: &str) -> Option<usize> {
    let mut offset = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let end = offset + line.len();
        if bytes_heading(line, section) {
            return Some(end);
        }
        offset = end;
    }
    None
}

fn is_heading(line: &str, section: &str) -> bool {
    line.trim().trim_end_matches('\r') == format!("## {section}")
}

fn known_section(line: &str) -> bool {
    ["Acceptance", "Verify", "Notes", "Request"]
        .iter()
        .any(|section| is_heading(line, section))
}

fn intent_prefix(bytes: &[u8]) -> Vec<u8> {
    bytes[..heading_start_bytes(bytes, "Request").unwrap_or(bytes.len())].to_vec()
}

fn heading_start_bytes(bytes: &[u8], section: &str) -> Option<usize> {
    let mut offset = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if bytes_heading(line, section) {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn request_part(bytes: &[u8]) -> &[u8] {
    let Some(end) = heading_end(bytes, "Request") else {
        return b"";
    };
    &bytes[end..]
}

fn bytes_heading(line: &[u8], section: &str) -> bool {
    let mut line = line.strip_suffix(b"\n").unwrap_or(line);
    line = line.strip_suffix(b"\r").unwrap_or(line);
    let line = trim_ascii(line);
    line == format!("## {section}").as_bytes()
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn append_request(prefix: &[u8], request: &[u8]) -> Vec<u8> {
    let mut output = prefix.to_vec();
    if output.ends_with(b"\n\n") {
        // The normalized prefix already has its Request separator.
    } else if output.ends_with(b"\n") {
        output.push(b'\n');
    } else {
        output.extend_from_slice(b"\n\n");
    }
    output.extend_from_slice(b"## Request\n");
    output.extend_from_slice(request);
    output
}

fn change_keep_modifier(line: &str, make_keep: bool) -> String {
    let Some((marker, rest)) = line.split_once(':') else {
        return line.to_owned();
    };
    let after_colon = rest;
    let leading = after_colon.len() - after_colon.trim_start().len();
    let content = &after_colon[leading..];
    let trailing = content.len() - content.trim_end().len();
    let trailing_text = &content[content.len() - trailing..];
    let core = content.trim_end();
    let (kind, modifiers) = core
        .split_once(char::is_whitespace)
        .map_or((core, ""), |(kind, modifiers)| (kind, modifiers));
    if kind != "test" {
        return line.to_owned();
    }
    let modifiers = modifiers.trim_start();
    let is_keep = modifiers
        .split_whitespace()
        .next()
        .is_some_and(|word| word == "keep");
    let rest_modifiers = if is_keep {
        modifiers
            .split_once(char::is_whitespace)
            .map_or("", |(_, rest)| rest.trim_start())
    } else {
        modifiers
    };
    let next_modifiers = if make_keep {
        if rest_modifiers.is_empty() {
            "keep".to_owned()
        } else {
            format!("keep {rest_modifiers}")
        }
    } else {
        rest_modifiers.to_owned()
    };
    let mut rebuilt = format!("{marker}:{}test", " ".repeat(leading));
    if !next_modifiers.is_empty() {
        rebuilt.push(' ');
        rebuilt.push_str(&next_modifiers);
    }
    rebuilt.push_str(trailing_text);
    rebuilt
}

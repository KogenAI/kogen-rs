use super::YamlIssue;
use super::{lexical, structure};
use std::str;

const LIMIT: usize = 1_048_576;

#[derive(Clone)]
pub(super) struct Candidate {
    pub(super) line: usize,
    pub(super) order: u8,
    pub(super) message: String,
}

pub(super) fn check(bytes: &[u8]) -> Result<&str, YamlIssue> {
    if bytes.len() > LIMIT {
        return Err(issue(
            None,
            "document exceeds the maximum size of 1048576 bytes",
        ));
    }
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(issue(Some(1), "leading UTF-8 BOM is not allowed"));
    }
    let text = str::from_utf8(bytes).map_err(|error| {
        let line = bytes[..error.valid_up_to()]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count()
            + 1;
        issue(Some(line), "document is not valid UTF-8")
    })?;
    let lines: Vec<&str> = text.split('\n').collect();
    let mut candidates = Vec::new();
    lexical::scan_lines(&lines, &mut candidates);
    structure::check_indentation(&lines, &mut candidates);
    structure::check_missing_values(&lines, &mut candidates);
    structure::check_duplicates(&lines, &mut candidates);
    structure::check_flow_duplicates(&lines, &mut candidates);
    if !text
        .lines()
        .any(|line| !lexical::strip_comment(line).trim().is_empty())
    {
        add(&mut candidates, 0, 23, "empty document");
    }
    if let Some(first) = candidates
        .into_iter()
        .min_by_key(|error| (error.line, error.order))
    {
        return Err(issue(
            (first.line != 0).then_some(first.line),
            first.message,
        ));
    }
    Ok(text)
}

pub(super) fn add(errors: &mut Vec<Candidate>, line: usize, order: u8, message: impl Into<String>) {
    errors.push(Candidate {
        line,
        order,
        message: message.into(),
    });
}

fn issue(line: Option<usize>, message: impl Into<String>) -> YamlIssue {
    YamlIssue {
        line,
        message: message.into(),
    }
}

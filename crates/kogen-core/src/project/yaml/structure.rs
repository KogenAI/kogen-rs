use super::lexical::{mapping_key_value, strip_comment};
use super::preflight::{Candidate, add};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn check_indentation(lines: &[&str], errors: &mut Vec<Candidate>) {
    let mut previous_root_value = false;
    for (index, line) in lines.iter().enumerate() {
        let Some((indent, body)) = line_parts(line) else {
            continue;
        };
        if indent > 0 && previous_root_value && !body.starts_with('-') {
            add(errors, index + 1, 19, "unexpected indentation");
        }
        if indent == 0 {
            previous_root_value =
                mapping_key_value(body).is_some_and(|(_, value)| !value.trim().is_empty());
        }
    }
}

pub(super) fn check_missing_values(lines: &[&str], errors: &mut Vec<Candidate>) {
    for (index, line) in lines.iter().enumerate() {
        let Some((indent, body)) = line_parts(line) else {
            continue;
        };
        if let Some((_, value)) = mapping_key_value(body) {
            if value.trim().is_empty() {
                let next = lines[index + 1..]
                    .iter()
                    .enumerate()
                    .find(|(_, next)| line_parts(next).is_some());
                let indentless_sequence = next.is_some_and(|(_, next)| {
                    let (next_indent, next_body) = line_parts(next).unwrap_or((0, ""));
                    next_indent == indent && next_body.starts_with('-')
                });
                if let Some((next_index, _)) = next.filter(|_| indentless_sequence) {
                    add(errors, index + next_index + 2, 19, "unexpected indentation");
                    continue;
                }
                let child_exists = next.is_some_and(|(_, next)| leading_whitespace(next) > indent);
                if !child_exists {
                    add(errors, index + 1, 21, "mapping key has no value");
                }
            }
        } else if body == "-" {
            add(errors, index + 1, 22, "list item has no value");
        }
    }
}

fn leading_whitespace(line: &str) -> usize {
    line.chars().take_while(|ch| ch.is_whitespace()).count()
}

pub(super) fn check_duplicates(lines: &[&str], errors: &mut Vec<Candidate>) {
    let mut keys: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        let Some((indent, body)) = line_parts(line) else {
            continue;
        };
        let (scope, mapping) = if let Some(item) = body.strip_prefix("- ") {
            keys.retain(|level, _| *level < indent + 2);
            (indent + 2, item)
        } else {
            keys.retain(|level, _| *level <= indent);
            (indent, body)
        };
        if let Some((key, _)) = mapping_key_value(mapping) {
            let key = unquote_key(key.trim());
            if !keys.entry(scope).or_default().insert(key.clone()) {
                add(errors, index + 1, 20, format!("duplicate key \"{key}\""));
            }
        }
    }
}

pub(super) fn check_flow_duplicates(lines: &[&str], errors: &mut Vec<Candidate>) {
    let mut stack: Vec<FlowFrame> = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for (line_index, source) in lines.iter().enumerate() {
        let line = source.strip_suffix('\r').unwrap_or(source);
        let visible = strip_comment(line);
        let chars: Vec<(usize, char)> = visible.char_indices().collect();
        for (index, &(offset, ch)) in chars.iter().enumerate() {
            if let Some(active) = quote {
                if active == '"' && escaped {
                    escaped = false;
                } else if active == '"' && ch == '\\' {
                    escaped = true;
                } else if ch == active {
                    quote = None;
                }
                if stack
                    .last()
                    .is_some_and(|frame| frame.open == '{' && frame.reading_key)
                {
                    let frame = stack.last_mut().expect("flow map exists");
                    if frame.key.trim().is_empty() {
                        frame.key_line = line_index + 1;
                    }
                    frame.key.push(ch);
                }
                continue;
            }
            if ch == '"' || ch == '\'' {
                quote = Some(ch);
                if stack
                    .last()
                    .is_some_and(|frame| frame.open == '{' && frame.reading_key)
                {
                    let frame = stack.last_mut().expect("flow map exists");
                    frame.key_line = line_index + 1;
                    frame.key.push(ch);
                }
                continue;
            }
            match ch {
                '[' | '{' => stack.push(FlowFrame::new(ch, line_index + 1)),
                ']' | '}' => {
                    if stack.last().is_some_and(|frame| {
                        (frame.open == '[' && ch == ']') || (frame.open == '{' && ch == '}')
                    }) {
                        stack.pop();
                    }
                }
                ',' if stack.last().is_some_and(|frame| frame.open == '{') => {
                    let frame = stack.last_mut().expect("flow map exists");
                    frame.key.clear();
                    frame.reading_key = true;
                    frame.key_line = line_index + 1;
                }
                ':' if stack
                    .last()
                    .is_some_and(|frame| frame.open == '{' && frame.reading_key)
                    && chars.get(index + 1).is_none_or(|(_, next)| {
                        next.is_whitespace() || matches!(next, ',' | '}' | ']')
                    }) =>
                {
                    let frame = stack.last_mut().expect("flow map exists");
                    let key = unquote_key(frame.key.trim());
                    if key == "<<" {
                        add(
                            errors,
                            frame.key_line,
                            8,
                            "YAML merge key `<<` is not allowed",
                        );
                    }
                    if !frame.seen.insert(key.clone()) {
                        add(
                            errors,
                            frame.key_line,
                            20,
                            format!("duplicate key \"{key}\""),
                        );
                    }
                    frame.key.clear();
                    frame.reading_key = false;
                }
                _ if stack
                    .last()
                    .is_some_and(|frame| frame.open == '{' && frame.reading_key) =>
                {
                    let frame = stack.last_mut().expect("flow map exists");
                    if frame.key.trim().is_empty() {
                        frame.key_line = line_index + 1;
                    }
                    frame.key.push(ch);
                }
                _ => {}
            }
            if ch == '#' && (offset == 0 || visible[..offset].ends_with(char::is_whitespace)) {
                break;
            }
        }
    }
}

struct FlowFrame {
    open: char,
    key: String,
    key_line: usize,
    reading_key: bool,
    seen: BTreeSet<String>,
}

impl FlowFrame {
    fn new(open: char, line: usize) -> Self {
        Self {
            open,
            key: String::new(),
            key_line: line,
            reading_key: open == '{',
            seen: BTreeSet::new(),
        }
    }
}

fn line_parts(line: &str) -> Option<(usize, &str)> {
    let line = strip_comment(line.strip_suffix('\r').unwrap_or(line));
    if line.trim().is_empty() {
        return None;
    }
    let indent = line.len() - line.trim_start_matches(' ').len();
    Some((indent, line[indent..].trim_end()))
}

fn unquote_key(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value)
        .to_owned()
}

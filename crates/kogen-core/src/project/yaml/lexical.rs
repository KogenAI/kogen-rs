use super::preflight::{Candidate, add};

pub(super) fn scan_lines(lines: &[&str], errors: &mut Vec<Candidate>) {
    let mut flow: Vec<(char, usize)> = Vec::new();
    for (index, source) in lines.iter().enumerate() {
        let line_no = index + 1;
        let line = source.strip_suffix('\r').unwrap_or(source);
        if line.contains('\t') {
            add(errors, line_no, 3, "tab character: indent with spaces");
        }
        let visible = strip_comment(line);
        let trimmed = visible.trim();
        if trimmed.starts_with('%') || trimmed == "---" || trimmed == "..." {
            add(
                errors,
                line_no,
                4,
                "directives and document markers are not allowed",
            );
        }
        scan_quotes_and_flow(line, line_no, &mut flow, errors);
        scan_reserved(line, line_no, errors);
        if trimmed.starts_with('|')
            || trimmed.starts_with('>')
            || trimmed
                .strip_prefix("- ")
                .is_some_and(|value| value.trim_start().starts_with(['|', '>']))
        {
            add(
                errors,
                line_no,
                5,
                "anchors, aliases, tags, and block scalars are not allowed",
            );
        }
        if let Some((key, value)) = mapping_key_value(line) {
            if key.trim() == "<<" {
                add(errors, line_no, 8, "YAML merge key `<<` is not allowed");
            }
            let value = strip_comment(value).trim();
            if value.starts_with('|') || value.starts_with('>') {
                add(
                    errors,
                    line_no,
                    5,
                    "anchors, aliases, tags, and block scalars are not allowed",
                );
            }
            if starts_list_item(value) {
                add(errors, line_no, 18, "list item in a value position");
            }
            if contains_plain_colon_space(value) {
                let scalar = value.trim_matches('"').trim_matches('\'');
                add(
                    errors,
                    line_no,
                    17,
                    format!("unquoted `: ` inside a value: \"{scalar}\""),
                );
            }
        }
    }
    if let Some((_, line)) = flow.first() {
        add(errors, *line, 15, "unterminated flow collection");
    }
}

fn scan_quotes_and_flow(
    line: &str,
    line_no: usize,
    flow: &mut Vec<(char, usize)>,
    errors: &mut Vec<Candidate>,
) {
    let mut quote = None;
    let mut escaped = false;
    let mut previous = '\0';
    let mut item_start = 0;
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (offset, ch) = chars[i];
        if let Some(active) = quote {
            if active == '"' && escaped {
                if ch == 'u' {
                    add(errors, line_no, 9, "Unicode escape \\u is not supported");
                } else if !matches!(ch, 'n' | 't' | '"' | '\\' | '/') {
                    add(errors, line_no, 10, format!("unsupported escape \\{ch}"));
                }
                escaped = false;
            } else if active == '"' && ch == '\\' {
                escaped = true;
            } else if active == '\'' && ch == '\'' {
                if chars.get(i + 1).is_some_and(|(_, next)| *next == '\'') {
                    i += 1;
                } else {
                    quote = None;
                    quote_tail(
                        line,
                        chars.get(i + 1).map(|(at, _)| *at).unwrap_or(line.len()),
                        line_no,
                        errors,
                    );
                }
            } else if active == '"' && ch == '"' {
                quote = None;
                quote_tail(
                    line,
                    chars.get(i + 1).map(|(at, _)| *at).unwrap_or(line.len()),
                    line_no,
                    errors,
                );
            }
            previous = ch;
            i += 1;
            continue;
        }
        if ch == '#' && (offset == 0 || previous.is_whitespace()) {
            break;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if matches!(ch, '[' | '{') {
            if ch == '['
                && !flow.is_empty()
                && !matches!(previous, '\0' | '[' | '{' | ',' | ':')
                && !previous.is_whitespace()
            {
                let token_end = line[offset..]
                    .find(']')
                    .map(|end| offset + end + 1)
                    .unwrap_or(line.len());
                let token = line[item_start..token_end].trim();
                add(
                    errors,
                    line_no,
                    13,
                    format!("quote \"{token}\": brackets inside a flow collection"),
                );
            }
            flow.push((ch, line_no));
            if flow.len() > 64 {
                add(
                    errors,
                    line_no,
                    7,
                    "maximum nesting depth of 64 collections exceeded",
                );
            }
            item_start = offset + ch.len_utf8();
        } else if matches!(ch, ']' | '}') {
            if flow
                .last()
                .is_some_and(|(open, _)| (*open == '[' && ch == ']') || (*open == '{' && ch == '}'))
            {
                flow.pop();
                if flow.is_empty() {
                    let tail = line[offset + ch.len_utf8()..].trim();
                    if !tail.is_empty() && !tail.starts_with('#') {
                        add(errors, line_no, 16, "trailing text after flow collection");
                    }
                }
            } else if !flow.is_empty() {
                add(
                    errors,
                    line_no,
                    14,
                    format!("malformed flow collection near {}", line[offset..].trim()),
                );
            }
        } else if ch == ',' && !flow.is_empty() {
            let rest = line[offset + 1..].trim_start();
            let before = line[..offset].trim_end();
            if before.ends_with(',') || before.ends_with('[') || before.ends_with('{') {
                let start = line.find(['[', '{']).map(|at| at + 1).unwrap_or(offset);
                add(
                    errors,
                    line_no,
                    14,
                    format!("malformed flow collection near {}", line[start..].trim()),
                );
            }
            item_start = offset + 1 + (line[offset + 1..].len() - rest.len());
        }
        previous = ch;
        i += 1;
    }
    if quote.is_some() {
        add(errors, line_no, 11, "unterminated quoted string");
    }
}

fn quote_tail(line: &str, offset: usize, line_no: usize, errors: &mut Vec<Candidate>) {
    let tail = line[offset..].trim_start();
    if !tail.is_empty() && !tail.starts_with(['#', ':', ',', ']', '}']) {
        add(errors, line_no, 12, "text after closing quote");
    }
}

fn scan_reserved(line: &str, line_no: usize, errors: &mut Vec<Candidate>) {
    let visible = strip_comment(line);
    let bytes = visible.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if !matches!(byte, b'&' | b'*' | b'!') {
            continue;
        }
        let prefix = visible[..index].trim_end();
        let token_start = prefix.is_empty() || prefix.ends_with([':', '[', '{', ',', '-']);
        if token_start
            && bytes
                .get(index + 1)
                .is_some_and(|next| !next.is_ascii_whitespace())
        {
            let message = if byte == b'!' {
                "anchors, aliases, and tags are not allowed"
            } else {
                "anchors, aliases, tags, and block scalars are not allowed"
            };
            add(errors, line_no, 6, message);
            break;
        }
    }
}

pub(super) fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    let bytes = line.as_bytes();
    for (index, ch) in line.char_indices() {
        if let Some(active) = quote {
            if active == '"' && escaped {
                escaped = false;
            } else if active == '"' && ch == '\\' {
                escaped = true;
            } else if ch == active {
                if active == '\'' && bytes.get(index + 1) == Some(&b'\'') {
                    continue;
                }
                quote = None;
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch == '#' && (index == 0 || line[..index].ends_with(char::is_whitespace)) {
            return &line[..index];
        }
    }
    line
}

pub(super) fn mapping_key_value(line: &str) -> Option<(&str, &str)> {
    let visible = strip_comment(line).trim();
    let visible = visible.strip_prefix("- ").unwrap_or(visible);
    let colon = plain_colon(visible)?;
    Some((&visible[..colon], visible[colon + 1..].trim_start()))
}

fn plain_colon(value: &str) -> Option<usize> {
    let mut quote = None;
    let mut depth = 0usize;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if let Some(active) = quote {
            if active == '"' && escaped {
                escaped = false;
            } else if active == '"' && ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if matches!(ch, '[' | '{') {
            depth += 1;
        } else if matches!(ch, ']' | '}') {
            depth = depth.saturating_sub(1);
        } else if ch == ':' && depth == 0 {
            return Some(index);
        }
    }
    None
}

fn contains_plain_colon_space(value: &str) -> bool {
    let mut quote = None;
    let mut depth = 0usize;
    let mut escaped = false;
    let chars: Vec<(usize, char)> = value.char_indices().collect();
    for (index, &(_, ch)) in chars.iter().enumerate() {
        if let Some(active) = quote {
            if active == '"' && escaped {
                escaped = false;
            } else if active == '"' && ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if matches!(ch, '[' | '{') {
            depth += 1;
        } else if matches!(ch, ']' | '}') {
            depth = depth.saturating_sub(1);
        } else if ch == ':'
            && depth == 0
            && chars
                .get(index + 1)
                .is_some_and(|(_, next)| next.is_whitespace())
        {
            return true;
        }
    }
    false
}

fn starts_list_item(value: &str) -> bool {
    value
        .strip_prefix('-')
        .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace))
}

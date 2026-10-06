use crate::gate::CheckFinding;
use std::path::{Path, PathBuf};

pub(super) fn parse(output: &[u8], workdir: &Path) -> Vec<CheckFinding> {
    let text = String::from_utf8_lossy(output);
    let lines = text.lines().collect::<Vec<_>>();
    let mut findings = parse_test_failures(&lines, workdir);
    findings.extend(
        lines
            .iter()
            .filter_map(|line| compiler_finding(line, workdir)),
    );
    findings.extend(parse_credo(&lines, workdir));
    findings.extend(parse_format(&lines, workdir));
    findings
}

fn parse_test_failures(lines: &[&str], workdir: &Path) -> Vec<CheckFinding> {
    let mut findings = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in lines {
        if let Some(name) = failure_name(line) {
            if let Some(previous) = current.take()
                && let Some(finding) = test_finding(previous, workdir)
            {
                findings.push(finding);
            }
            current = Some((name, vec![line]));
        } else if let Some((_, block)) = &mut current {
            if line.trim_start().starts_with("Finished in ") {
                if let Some(previous) = current.take()
                    && let Some(finding) = test_finding(previous, workdir)
                {
                    findings.push(finding);
                }
            } else {
                block.push(line);
            }
        }
    }
    if let Some(previous) = current
        && let Some(finding) = test_finding(previous, workdir)
    {
        findings.push(finding);
    }
    findings
}

fn failure_name(line: &str) -> Option<String> {
    let cleaned = clean_cli_line(line);
    let (_, remainder) = cleaned.split_once(") test ")?;
    let end = remainder.rfind(" (")?;
    let name = remainder[..end]
        .strip_prefix("test ")
        .unwrap_or(&remainder[..end]);
    (!name.is_empty()).then(|| name.to_owned())
}

fn test_finding((symbol, block): (String, Vec<&str>), workdir: &Path) -> Option<CheckFinding> {
    let location = block
        .iter()
        .find_map(|line| {
            let clean = clean_cli_line(line);
            parse_location(&clean).map(|(path, line, column)| (path.to_owned(), line, column))
        })
        .filter(|(path, _, _)| path.ends_with(".exs") || path.ends_with(".ex"));
    let (path, line, column) = location
        .map(|(path, line, column)| (normalize_path(&path, workdir), Some(line), column))
        .unwrap_or_default();
    let assertion = block.iter().any(|line| {
        let line = clean_cli_line(line);
        line.contains("Assertion")
            || line.contains("match (=) failed")
            || line.contains("Expected truthy")
    });
    let environmental = block.iter().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("no such file or directory")
            || line.contains("permission denied")
            || line.contains("operation not permitted")
            || line.contains("command not found")
    });
    let rule = if environmental {
        "exunit/environment"
    } else if assertion {
        "exunit/assertion"
    } else {
        "exunit/failure"
    };
    let message = block
        .iter()
        .map(|line| clean_cli_line(line))
        .find(|line| {
            line.contains("Assertion")
                || line.contains("match (=) failed")
                || line.starts_with("** (")
                || line.contains("Expected truthy")
        })
        .unwrap_or_else(|| "test failed".to_owned());
    Some(CheckFinding {
        path,
        rule: rule.to_owned(),
        symbol,
        message,
        line,
        column,
    })
}

fn compiler_finding(line: &str, workdir: &Path) -> Option<CheckFinding> {
    let line = clean_cli_line(line);
    let marker = [
        "(CompileError) ",
        "(SyntaxError) ",
        "(TokenMissingError) ",
        "(CompileWarning) ",
    ]
    .iter()
    .find_map(|marker| line.find(marker).map(|index| (index, marker.len())))?;
    let diagnostic = line[marker.0 + marker.1..].trim();
    let location_text = diagnostic.split_whitespace().next()?;
    let (path, line_number, column) = parse_location(location_text)?;
    let detail = diagnostic[location_text.len()..].trim_start_matches(": ");
    let lower = diagnostic.to_ascii_lowercase();
    let kind = if lower.contains("undefined or private") || lower.contains("undefined function") {
        "undefined"
    } else if lower.contains("deprecated") {
        "deprecated"
    } else if lower.contains("unused") {
        "unused"
    } else {
        "compile_error"
    };
    Some(CheckFinding {
        path: normalize_path(path, workdir),
        rule: format!("compile/{kind}"),
        symbol: String::new(),
        message: if detail.is_empty() {
            diagnostic.to_owned()
        } else {
            detail.to_owned()
        },
        line: Some(line_number),
        column,
    })
}

fn parse_credo(lines: &[&str], workdir: &Path) -> Vec<CheckFinding> {
    let mut findings = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in lines {
        let clean = clean_cli_line(line);
        if let Some((severity, message)) = credo_header(&clean) {
            if let Some(block) = current.take()
                && let Some(finding) = credo_finding(block, workdir)
            {
                findings.push(finding);
            }
            current = Some((severity, vec![message]));
        } else if current.is_some() && !clean.is_empty() {
            let has_location = parse_location(&clean)
                .is_some_and(|(path, _, _)| path.ends_with(".exs") || path.ends_with(".ex"));
            if let Some((_, messages)) = current.as_mut() {
                messages.push(clean);
            }
            if has_location
                && let Some(block) = current.take()
                && let Some(finding) = credo_finding(block, workdir)
            {
                findings.push(finding);
            }
        }
    }
    if let Some(block) = current
        && let Some(finding) = credo_finding(block, workdir)
    {
        findings.push(finding);
    }
    findings
}

fn credo_header(line: &str) -> Option<(String, String)> {
    let header = line.strip_prefix('[')?;
    let (severity, message) = header.split_once(']')?;
    if !matches!(severity, "F" | "W" | "C" | "R" | "D") {
        return None;
    }
    let message = message.trim().trim_start_matches(['↗', '↘', '→']).trim();
    Some((severity.to_owned(), message.to_owned()))
}

fn credo_finding((_, messages): (String, Vec<String>), workdir: &Path) -> Option<CheckFinding> {
    let joined = messages.join(" ");
    let location = messages
        .iter()
        .find_map(|message| parse_location(message))
        .filter(|(path, _, _)| path.ends_with(".exs") || path.ends_with(".ex"));
    let (path, line, column) = location
        .map(|(path, line, column)| (normalize_path(path, workdir), Some(line), column))
        .unwrap_or_default();
    let rule = credo_rule(&joined);
    Some(CheckFinding {
        path,
        rule: format!("credo/{rule}"),
        symbol: String::new(),
        message: joined.split_whitespace().collect::<Vec<_>>().join(" "),
        line,
        column,
    })
}

fn credo_rule(message: &str) -> &str {
    for prefix in ["Credo.Check.", "Warning."] {
        if let Some(start) = message.find(prefix) {
            let rule = &message[start + prefix.len()..];
            return rule
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '.')
                .next()
                .filter(|rule| !rule.is_empty())
                .unwrap_or("unknown");
        }
    }
    if message.contains("spans ") {
        "ModuleSize"
    } else if message.contains("File has ") {
        "FileSize"
    } else {
        "unknown"
    }
}

fn parse_format(lines: &[&str], workdir: &Path) -> Vec<CheckFinding> {
    let mut findings = Vec::new();
    let mut listing = false;
    for line in lines {
        let clean = clean_cli_line(line);
        if clean.contains("The following files are not formatted:") {
            listing = true;
            continue;
        }
        if listing {
            let path = clean
                .strip_prefix("* ")
                .or_else(|| clean.strip_prefix("- "));
            if let Some(path) = path {
                findings.push(CheckFinding {
                    path: normalize_path(path.trim(), workdir),
                    rule: "format/unformatted".to_owned(),
                    symbol: String::new(),
                    message: "file is not formatted".to_owned(),
                    line: None,
                    column: None,
                });
            } else if !clean.is_empty() {
                listing = false;
            }
        }
        if clean.contains("mix format failed") {
            for path in clean
                .split_whitespace()
                .filter(|part| part.ends_with(".ex") || part.ends_with(".exs"))
            {
                findings.push(CheckFinding {
                    path: normalize_path(
                        path.trim_matches(|character| character == '*' || character == '\x60'),
                        workdir,
                    ),
                    rule: "format/unformatted".to_owned(),
                    symbol: String::new(),
                    message: "file is not formatted".to_owned(),
                    line: None,
                    column: None,
                });
            }
        }
    }
    findings
}

fn parse_location(value: &str) -> Option<(&str, u32, Option<u32>)> {
    let value = value.trim().trim_end_matches(':');
    let (prefix, final_field) = value.rsplit_once(':')?;
    let final_field = final_field.parse::<u32>().ok()?;
    if let Some((path, line)) = prefix.rsplit_once(':')
        && let Ok(line) = line.parse::<u32>()
    {
        return Some((path, line, Some(final_field)));
    }
    Some((prefix, final_field, None))
}

fn clean_cli_line(line: &str) -> String {
    line.trim()
        .trim_start_matches(['┃', '│', '└', '─'])
        .trim()
        .to_owned()
}

fn normalize_path(path: &str, workdir: &Path) -> String {
    if let Some(relative) = path.strip_prefix("$WORKDIR/") {
        return relative.to_owned();
    }
    let path = Path::new(path);
    if let Ok(relative) = path.strip_prefix(workdir) {
        return relative.to_string_lossy().replace('\\', "/");
    }
    if path.is_absolute() {
        let components = path.components().collect::<Vec<_>>();
        if let Some(index) = components.iter().position(|component| {
            matches!(component.as_os_str().to_str(), Some("lib" | "test" | "src"))
        }) {
            return components[index..]
                .iter()
                .collect::<PathBuf>()
                .to_string_lossy()
                .replace('\\', "/");
        }
    }
    path.to_string_lossy().replace('\\', "/")
}

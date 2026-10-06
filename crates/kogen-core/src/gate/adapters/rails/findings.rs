use crate::gate::CheckFinding;
use std::path::{Path, PathBuf};

pub(super) fn parse(output: &[u8], tool: &str, workdir: &Path) -> Vec<CheckFinding> {
    let text = String::from_utf8_lossy(output);
    let mut findings = parse_minitest(&text, workdir);
    findings.extend(
        text.lines()
            .filter_map(|line| ruby_lint_finding(line, tool, workdir)),
    );
    findings
}

fn parse_minitest(output: &str, workdir: &Path) -> Vec<CheckFinding> {
    let mut findings = Vec::new();
    let mut in_failure = false;
    let mut message = String::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains(") Failure:") || trimmed.contains(") Error:") {
            in_failure = true;
            message = trimmed.to_owned();
            continue;
        }
        if !in_failure {
            continue;
        }
        if let Some((symbol, path, line_number)) = minitest_location(trimmed) {
            findings.push(CheckFinding {
                path: normalize_path(path, workdir),
                rule: "minitest/failure".to_owned(),
                symbol,
                message: message.clone(),
                line: Some(line_number),
                column: None,
            });
            in_failure = false;
        } else if trimmed.starts_with("Finished in ") {
            in_failure = false;
        }
    }
    findings
}

fn minitest_location(line: &str) -> Option<(String, &str, u32)> {
    let marker = line.find(" [")?;
    let symbol = line[..marker].to_owned();
    if !symbol.contains("#test_") {
        return None;
    }
    let location = line[marker + 2..].strip_suffix("]:")?;
    let (path, line_number) = location.rsplit_once(':')?;
    Some((symbol, path, line_number.parse().ok()?))
}

fn ruby_lint_finding(line: &str, tool: &str, workdir: &Path) -> Option<CheckFinding> {
    let mut pieces = line.splitn(4, ':');
    let path = pieces.next()?.trim();
    let line_number = pieces.next()?.trim().parse::<u32>().ok()?;
    let column = pieces.next()?.trim().parse::<u32>().ok()?;
    let mut detail = pieces.next()?.trim();
    if let Some(rest) = ["C: ", "W: ", "E: ", "F: "]
        .into_iter()
        .find_map(|severity| detail.strip_prefix(severity))
    {
        detail = rest;
    }
    detail = detail
        .strip_prefix("[Correctable] ")
        .or_else(|| detail.strip_prefix("[Corrected] "))
        .unwrap_or(detail);
    let (rule, message) = detail.split_once(": ")?;
    if rule.is_empty() {
        return None;
    }
    Some(CheckFinding {
        path: normalize_path(path, workdir),
        rule: format!("{tool}/{rule}"),
        symbol: String::new(),
        message: message.to_owned(),
        line: Some(line_number),
        column: Some(column),
    })
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
            matches!(
                component.as_os_str().to_str(),
                Some("test" | "app" | "config" | "lib")
            )
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

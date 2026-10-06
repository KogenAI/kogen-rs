//! Rails project detection and command construction.

mod findings;

use crate::run::ChildEnvironment;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const ACCEPTANCE_EXTENSION: &str = "_test.rb";
pub const CANDIDATE_DIRECTORY: &str = "test/acceptance";
pub const GATE_FILES: &[&str] = &[
    "Gemfile",
    "Gemfile.lock",
    "bin/rails",
    ".standard.yml",
    ".rubocop.yml",
];
pub const SETUP_SEEDS: &[&str] = &["vendor/cache"];

/// Rails is auto-selected only when both identifying files exist.
pub fn detected(checkout: &Path) -> bool {
    checkout.join("Gemfile").is_file() && checkout.join("config/application.rb").is_file()
}

/// An explicit adapter wins over stack detection.
pub fn selected(checkout: &Path, configured_adapter: Option<&str>) -> bool {
    configured_adapter.map_or_else(|| detected(checkout), |adapter| adapter == "rails")
}

pub fn source_path(slug: &str) -> PathBuf {
    PathBuf::from(format!(".kogen/acceptance/{slug}{ACCEPTANCE_EXTENSION}"))
}

pub fn candidate_path(slug: &str) -> PathBuf {
    PathBuf::from(format!(
        "{CANDIDATE_DIRECTORY}/{slug}{ACCEPTANCE_EXTENSION}"
    ))
}

/// The shared command adapter replaces the exact {path} argument.
pub fn runner_command() -> Vec<OsString> {
    words(["bundle", "exec", "rails", "test", "{path}"])
}

pub fn acceptance_check() -> Vec<OsString> {
    words(["ruby", "-c", "{path}"])
}

/// Returns the project formatter selected from Gemfile declarations.
pub fn formatter(gemfile: &str) -> Option<Vec<OsString>> {
    if declares_gem(gemfile, "standard") {
        Some(words(["bundle", "exec", "standardrb", "-a"]))
    } else if declares_gem(gemfile, "rubocop") {
        Some(words(["bundle", "exec", "rubocop", "-a"]))
    } else {
        None
    }
}

pub fn setup_seeds() -> &'static [&'static str] {
    SETUP_SEEDS
}

pub fn setup_command() -> Vec<OsString> {
    words(["bundle", "install", "--local"])
}

/// Adds the Rails-only environment values to the filtered child environment.
pub fn child_environment(vendor_cache: &Path) -> ChildEnvironment {
    let mut environment = ChildEnvironment::new();
    environment.insert(
        OsString::from("BUNDLE_PATH"),
        vendor_cache.as_os_str().to_owned(),
    );
    environment.insert(OsString::from("RAILS_ENV"), OsString::from("test"));
    environment
}

pub fn gate_files() -> &'static [&'static str] {
    GATE_FILES
}

pub fn parse_findings(output: &[u8], workdir: &Path) -> Vec<crate::gate::CheckFinding> {
    findings::parse(output, "rubocop", workdir)
}

pub fn parse_findings_for_tool(
    output: &[u8],
    tool: &str,
    workdir: &Path,
) -> Vec<crate::gate::CheckFinding> {
    findings::parse(output, tool, workdir)
}

fn words<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

fn declares_gem(gemfile: &str, expected: &str) -> bool {
    gemfile
        .lines()
        .any(|line| line_declares_gem(line, expected))
}

fn line_declares_gem(line: &str, expected: &str) -> bool {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'#' => return false,
            b'\'' | b'"' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else if bytes[index] == quote {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                if &line[start..index] == "gem"
                    && declaration_argument(&line[index..]) == Some(expected)
                {
                    return true;
                }
            }
            _ => index += 1,
        }
    }
    false
}

fn declaration_argument(mut tail: &str) -> Option<&str> {
    tail = tail.trim_start();
    if let Some(rest) = tail.strip_prefix('(') {
        tail = rest.trim_start();
    }
    let quote = tail.as_bytes().first().copied()?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let literal = &tail[1..];
    let end = literal.as_bytes().iter().position(|byte| *byte == quote)?;
    Some(&literal[..end])
}

#[cfg(test)]
#[path = "rails_tests.rs"]
mod tests;

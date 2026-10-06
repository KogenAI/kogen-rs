//! Strict YAML subset used by project files and Intent frontmatter.

mod lexical;
mod preflight;
mod structure;

#[cfg(test)]
mod tests;

use serde_yaml::Value;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YamlIssue {
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for YamlIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(line) = self.line {
            write!(f, "line {line}: {}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<Value, YamlIssue> {
    match preflight::check(bytes) {
        Ok(text) => parse_validated(text),
        Err(issue) => Err(issue),
    }
}

fn parse_validated(text: &str) -> Result<Value, YamlIssue> {
    serde_yaml::from_str(text).map_err(|error| YamlIssue {
        line: error.location().map(|location| location.line()),
        message: "invalid YAML".to_owned(),
    })
}

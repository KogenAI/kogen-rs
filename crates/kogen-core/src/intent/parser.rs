#[path = "frontmatter.rs"]
mod frontmatter;

use crate::project::valid_slug;
use std::collections::BTreeSet;
use std::fmt;

use frontmatter::parse_frontmatter;
pub use frontmatter::{Contract, Frontmatter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseIssue {
    pub line: usize,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntentParseError(pub ParseIssue);

impl fmt::Display for IntentParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.0.line, self.0.message)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptanceItem {
    pub id: String,
    pub text: String,
    pub line: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyItem {
    pub id: String,
    pub words: Vec<String>,
    pub line: usize,
}

impl VerifyItem {
    pub fn kind(&self) -> Option<&str> {
        self.words.first().map(String::as_str)
    }
    pub fn is_keep(&self) -> bool {
        self.words.get(1).is_some_and(|word| word == "keep")
    }
    pub fn is_change(&self) -> bool {
        self.kind() == Some("test") && !self.is_keep()
    }
    pub fn is_integration(&self) -> bool {
        self.words.iter().skip(1).any(|word| word == "integration")
    }
    pub fn domain(&self) -> Option<&str> {
        self.words
            .iter()
            .rev()
            .find_map(|word| word.strip_prefix("domain="))
    }
    pub fn after_ids(&self) -> impl Iterator<Item = &str> {
        self.words
            .iter()
            .filter_map(|word| word.strip_prefix("after="))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Intent {
    pub slug: String,
    pub frontmatter: Frontmatter,
    pub brief: String,
    pub brief_lines: Vec<(usize, String)>,
    pub acceptance: Vec<AcceptanceItem>,
    pub verify: Vec<VerifyItem>,
    pub notes: String,
    pub notes_lines: Vec<(usize, String)>,
    pub request: Option<String>,
    raw_bytes: Vec<u8>,
}

#[derive(Clone)]
struct SourceLine<'a> {
    text: &'a str,
    line: usize,
    start: usize,
    end: usize,
}

impl SourceLine<'_> {
    fn logical(&self) -> &str {
        self.text.strip_suffix('\r').unwrap_or(self.text)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Section {
    Brief,
    Acceptance,
    Verify,
    Notes,
}

impl Intent {
    pub fn parse(slug: &str, bytes: &[u8]) -> Result<Self, IntentParseError> {
        if !valid_slug(slug) {
            return Err(parse_error(1, "invalid slug"));
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| parse_error(1, "Intent is not valid UTF-8"))?;
        let lines = source_lines(text);
        let Some(first) = lines.first() else {
            return Err(parse_error(1, "frontmatter must start with `---`"));
        };
        if first.logical() != "---" {
            return Err(parse_error(1, "frontmatter must start with `---`"));
        }
        let closing = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, line)| line.logical() == "---");
        let Some((closing_index, closing_line)) = closing else {
            return Err(parse_error(
                text.lines().count() + 1,
                "frontmatter is missing its closing `---`",
            ));
        };
        let frontmatter = parse_frontmatter(&text[first.end..closing_line.start])?;
        Self::parse_body(slug, bytes, text, &lines, closing_index, frontmatter)
    }

    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw_bytes
    }
    pub fn verify_for(&self, id: &str) -> Option<&VerifyItem> {
        self.verify.iter().find(|item| item.id == id)
    }

    fn parse_body(
        slug: &str,
        bytes: &[u8],
        text: &str,
        lines: &[SourceLine<'_>],
        closing_index: usize,
        frontmatter: Frontmatter,
    ) -> Result<Self, IntentParseError> {
        let mut section = Section::Brief;
        let mut seen = BTreeSet::new();
        let mut brief = Vec::new();
        let mut acceptance_lines = Vec::new();
        let mut verify_lines = Vec::new();
        let mut notes = Vec::new();
        let mut request = None;
        for line in lines.iter().skip(closing_index + 1) {
            let logical = line.logical();
            if let Some((heading, next)) = known_heading(logical.trim()) {
                if !seen.insert(heading) {
                    return Err(parse_error(
                        line.line + 1,
                        format!("duplicate {heading} section"),
                    ));
                }
                if next == Section::Brief {
                    request = Some(text[line.end.min(bytes.len())..].to_owned());
                    break;
                }
                section = next;
                continue;
            }
            if let Some(name) = unknown_heading(logical.trim())
                && section != Section::Brief
            {
                return Err(parse_error(
                    line.line + 1,
                    format!("unknown Intent section \"{name}\""),
                ));
            }
            match section {
                Section::Brief => brief.push((line.line, logical.to_owned())),
                Section::Acceptance => acceptance_lines.push((line.line, logical.to_owned())),
                Section::Verify => verify_lines.push((line.line, logical.to_owned())),
                Section::Notes => notes.push((line.line, logical.to_owned())),
            }
        }
        let acceptance = parse_acceptance(&acceptance_lines)?;
        let verify = parse_verify(&verify_lines)?;
        validate_verify_targets(&acceptance, &verify)?;
        Ok(Self {
            slug: slug.to_owned(),
            frontmatter,
            brief: trim_lines(&brief),
            brief_lines: brief,
            acceptance,
            verify,
            notes: trim_lines(&notes),
            notes_lines: notes,
            request,
            raw_bytes: bytes.to_vec(),
        })
    }
}

fn parse_acceptance(rows: &[(usize, String)]) -> Result<Vec<AcceptanceItem>, IntentParseError> {
    rows.iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(line, value)| {
            let (id, text) = parse_id_entry(value).ok_or_else(|| {
                parse_error(
                    *line,
                    "Acceptance entries use `- A<n>: one sentence` on one line",
                )
            })?;
            Ok(AcceptanceItem {
                id,
                text,
                line: *line,
            })
        })
        .collect()
}

fn parse_verify(rows: &[(usize, String)]) -> Result<Vec<VerifyItem>, IntentParseError> {
    let mut seen = BTreeSet::new();
    rows.iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(line, value)| {
            let (id, text) = parse_id_entry(value).ok_or_else(|| {
                parse_error(
                    *line,
                    "Verify entries use `- A<n>: test` or `- A<n>: test keep`",
                )
            })?;
            if !seen.insert(id.clone()) {
                return Err(parse_error(
                    *line,
                    format!("duplicate Verify entry for {id}"),
                ));
            }
            Ok(VerifyItem {
                id,
                words: text.split_whitespace().map(str::to_owned).collect(),
                line: *line,
            })
        })
        .collect()
}

fn validate_verify_targets(
    acceptance: &[AcceptanceItem],
    verify: &[VerifyItem],
) -> Result<(), IntentParseError> {
    for entry in verify {
        if !acceptance.iter().any(|item| item.id == entry.id) {
            return Err(parse_error(
                entry.line,
                format!("Verify entry {} has no Acceptance item", entry.id),
            ));
        }
    }
    Ok(())
}

fn parse_id_entry(line: &str) -> Option<(String, String)> {
    let rest = line
        .trim()
        .strip_prefix('-')?
        .trim_start()
        .strip_prefix('A')?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let suffix = &rest[digits..];
    Some((
        format!("A{}", &rest[..digits]),
        suffix.strip_prefix(':')?.trim_start().to_owned(),
    ))
}

fn known_heading(value: &str) -> Option<(&'static str, Section)> {
    match value {
        "## Acceptance" => Some(("Acceptance", Section::Acceptance)),
        "## Verify" => Some(("Verify", Section::Verify)),
        "## Notes" => Some(("Notes", Section::Notes)),
        "## Request" => Some(("Request", Section::Brief)),
        _ => None,
    }
}

fn unknown_heading(value: &str) -> Option<&str> {
    value.strip_prefix("## ").filter(|name| !name.is_empty())
}

fn source_lines(text: &str) -> Vec<SourceLine<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (index, part) in text.split_inclusive('\n').enumerate() {
        let end = start + part.len();
        out.push(SourceLine {
            text: part.strip_suffix('\n').unwrap_or(part),
            line: index + 1,
            start,
            end,
        });
        start = end;
    }
    out
}

fn trim_lines(lines: &[(usize, String)]) -> String {
    lines
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub(super) fn parse_error(line: usize, message: impl Into<String>) -> IntentParseError {
    IntentParseError(ParseIssue {
        line,
        message: message.into(),
    })
}

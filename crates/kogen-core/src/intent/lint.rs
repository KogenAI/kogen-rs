use super::parser::Intent;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LintSeverity {
    Error,
    Style,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintIssue {
    pub rule: &'static str,
    pub severity: LintSeverity,
    pub line: Option<usize>,
    pub message: String,
}

impl Intent {
    /// Return the guaranteed structural lint findings. Style policy is owned by package 12.
    pub fn lint(&self) -> Vec<LintIssue> {
        let mut out = Vec::new();
        if self.brief.trim().is_empty() {
            out.push(issue("missing_brief", None, "write the Brief as prose"));
        }
        if let Some((line, _)) = self.brief_lines.iter().find(|(_, value)| is_list(value)) {
            out.push(issue(
                "list_in_brief",
                Some(*line),
                "the Brief cannot contain lists",
            ));
        }
        if let Some((line, _)) = self.brief_lines.iter().find(|(_, value)| is_heading(value)) {
            out.push(issue(
                "heading_in_brief",
                Some(*line),
                "the Brief cannot contain headings",
            ));
        }
        if let Some((line, _)) = self
            .brief_lines
            .iter()
            .find(|(_, value)| is_code_fence(value))
        {
            out.push(issue(
                "code_block_in_brief",
                Some(*line),
                "the Brief cannot contain code blocks",
            ));
        }
        if !matches!(self.frontmatter.size.as_str(), "small" | "medium" | "large") {
            out.push(issue(
                "unknown_size",
                None,
                "size must be small, medium, or large",
            ));
        }
        if self.acceptance.is_empty() {
            out.push(issue(
                "acceptance_count",
                None,
                "Acceptance needs at least one item",
            ));
        }
        if self.frontmatter.title.trim().is_empty() {
            out.push(issue("missing_title", None, "title is required"));
        }
        if !(1..=4).contains(&self.frontmatter.domains.len()) {
            out.push(issue(
                "domain_count",
                None,
                "declare between one and four domains",
            ));
        }
        append_id_findings(self, &mut out);
        append_verify_findings(self, &mut out);
        if !self.verify.iter().any(super::parser::VerifyItem::is_change) {
            out.push(issue(
                "no_change_item",
                None,
                "at least one Acceptance item must be a change item (test)",
            ));
        }
        if let Some(line) = first_open_question(self) {
            out.push(issue(
                "open_question",
                Some(line),
                "remove TBD, TODO, FIXME, or unresolved question markers",
            ));
        }
        out
    }
}

fn append_id_findings(intent: &Intent, out: &mut Vec<LintIssue>) {
    let mut seen = std::collections::BTreeSet::new();
    if let Some(item) = intent
        .acceptance
        .iter()
        .find(|item| !seen.insert(item.id.as_str()))
    {
        out.push(issue(
            "duplicate_id",
            Some(item.line),
            "Acceptance ids must be unique",
        ));
    }
    if intent
        .acceptance
        .iter()
        .enumerate()
        .any(|(index, item)| item.id != format!("A{}", index + 1))
    {
        out.push(issue(
            "sequential_ids",
            None,
            "Acceptance ids must be A1 through An in order",
        ));
    }
}

fn append_verify_findings(intent: &Intent, out: &mut Vec<LintIssue>) {
    if intent
        .acceptance
        .iter()
        .any(|item| intent.verify_for(&item.id).is_none())
    {
        out.push(issue(
            "missing_verify",
            None,
            "every Acceptance item needs a Verify kind",
        ));
    }
    for item in &intent.verify {
        let Some(kind) = item.kind() else {
            out.push(issue(
                "invalid_verify",
                Some(item.line),
                "unknown Verify word \"\"",
            ));
            continue;
        };
        if matches!(kind, "example" | "check") {
            out.push(issue(
                "unsupported_verify_kind",
                Some(item.line),
                format!("{kind} is not supported in core v1"),
            ));
            continue;
        }
        if kind != "test" {
            out.push(issue(
                "invalid_verify",
                Some(item.line),
                format!("unknown Verify word \"{kind}\""),
            ));
            continue;
        }
        let modifiers = if item.words.get(1).is_some_and(|word| word == "keep") {
            item.words.iter().skip(2)
        } else {
            item.words.iter().skip(1)
        };
        for modifier in modifiers {
            let allowed = modifier == "integration"
                || modifier
                    .strip_prefix("domain=")
                    .is_some_and(|value| !value.is_empty())
                || modifier
                    .strip_prefix("after=")
                    .is_some_and(|value| !value.is_empty());
            if !allowed {
                out.push(issue(
                    "invalid_verify",
                    Some(item.line),
                    format!("unknown Verify word \"{modifier}\""),
                ));
                break;
            }
        }
    }
}

fn first_open_question(intent: &Intent) -> Option<usize> {
    intent
        .brief_lines
        .iter()
        .find(|(_, text)| contains_open_marker(text))
        .map(|(line, _)| *line)
        .or_else(|| {
            intent
                .acceptance
                .iter()
                .find(|item| contains_open_marker(&item.text))
                .map(|item| item.line)
        })
        .or_else(|| {
            intent
                .notes_lines
                .iter()
                .find(|(_, text)| contains_open_marker(text))
                .map(|(line, _)| *line)
        })
}

fn contains_open_marker(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    ["TBD", "TODO", "FIXME"]
        .iter()
        .any(|marker| has_word(&upper, marker))
        || has_needs_clarification(&upper)
        || upper.contains("??")
}

fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(start, _)| {
        let end = start + word.len();
        let left = text[..start].chars().next_back();
        let right = text[end..].chars().next();
        left.is_none_or(|ch| !is_word_char(ch)) && right.is_none_or(|ch| !is_word_char(ch))
    })
}

fn has_needs_clarification(text: &str) -> bool {
    text.match_indices('[').any(|(start, _)| {
        text[start + 1..]
            .trim_start()
            .starts_with("NEEDS CLARIFICATION")
    })
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn is_list(text: &str) -> bool {
    let text = text.trim_start();
    text.strip_prefix(['-', '*', '+'])
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
        || text.bytes().take_while(u8::is_ascii_digit).count() > 0
            && text
                .trim_start_matches(|ch: char| ch.is_ascii_digit())
                .starts_with(['.', ')'])
            && text
                .trim_start_matches(|ch: char| ch.is_ascii_digit())
                .get(1..)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

fn is_heading(text: &str) -> bool {
    let text = text.trim_start();
    let hashes = text.bytes().take_while(|&byte| byte == b'#').count();
    (1..=6).contains(&hashes)
        && text
            .as_bytes()
            .get(hashes)
            .is_some_and(u8::is_ascii_whitespace)
}

fn is_code_fence(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with("```") || text.starts_with("~~~")
}

fn issue(rule: &'static str, line: Option<usize>, message: impl Into<String>) -> LintIssue {
    LintIssue {
        rule,
        severity: LintSeverity::Error,
        line,
        message: message.into(),
    }
}

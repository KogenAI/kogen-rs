//! Data-driven Intent style findings shared by shaping and approval.

use super::lint::{LintIssue, LintSeverity};
use super::parser::Intent;
use serde_json::Value;
use std::sync::LazyLock;

static DATA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("lint-data.json")).expect("embedded lint data is valid")
});

pub(super) fn findings(intent: &Intent, shaping: bool) -> Vec<LintIssue> {
    let mut out = Vec::new();
    lint_tiers(intent, &mut out);
    lint_title(intent, &mut out);
    lint_items(intent, &mut out);
    lint_words_and_phrases(intent, &mut out);
    lint_sentences(intent, &mut out);
    lint_notes(intent, shaping, &mut out);
    out
}

pub(super) fn normalized_notes(notes: &str) -> Option<String> {
    let trimmed = notes.trim_start();
    if trimmed
        .get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Approach:"))
    {
        let suffix = &trimmed[9..];
        let suffix = if suffix.starts_with(char::is_whitespace) {
            suffix.to_owned()
        } else {
            format!(" {suffix}")
        };
        let normalized = format!("Approach:{suffix}");
        return (normalized != notes).then_some(normalized);
    }
    let min = DATA["limits"]["approach_min_words"].as_u64().unwrap_or(8) as usize;
    let tokens = words(trimmed);
    let starts_with_action = tokens
        .first()
        .is_some_and(|first| has_match(first, &DATA["action_verbs"]));
    if tokens.len() >= min && starts_with_action {
        Some(format!("Approach: {trimmed}"))
    } else {
        None
    }
}

fn lint_tiers(intent: &Intent, out: &mut Vec<LintIssue>) {
    let tier = intent.frontmatter.size.as_str();
    let Some(limits) = DATA.get("tiers").and_then(|tiers| tiers.get(tier)) else {
        return;
    };
    let brief_paragraphs = paragraph_count(&intent.brief);
    let paragraph_limit = limits["brief_paragraphs"].as_u64().unwrap_or(u64::MAX) as usize;
    if brief_paragraphs > paragraph_limit {
        out.push(issue(
            "brief_paragraphs",
            None,
            format!("{tier} Intents allow at most {paragraph_limit} Brief paragraphs"),
        ));
    }
    let brief_limit = limits["brief_words"].as_u64().unwrap_or(u64::MAX) as usize;
    if words(&intent.brief).len() > brief_limit {
        out.push(issue(
            "brief_too_long",
            None,
            format!("{tier} Intents allow at most {brief_limit} Brief words"),
        ));
    }
    let item_limit = limits["items"].as_u64().unwrap_or(u64::MAX) as usize;
    if intent.acceptance.len() > item_limit {
        out.push(issue(
            "too_many_items",
            None,
            format!("{tier} Intents allow at most {item_limit} Acceptance items"),
        ));
    }
    let notes_limit = limits["notes_words"].as_u64().unwrap_or(u64::MAX) as usize;
    if words(&intent.notes).len() > notes_limit {
        out.push(issue(
            "notes_too_long",
            None,
            format!("{tier} Intents allow at most {notes_limit} Notes words"),
        ));
    }
}

fn lint_title(intent: &Intent, out: &mut Vec<LintIssue>) {
    let limit = DATA["limits"]["title_chars"].as_u64().unwrap_or(72) as usize;
    if intent.frontmatter.title.chars().count() > limit {
        out.push(issue(
            "title_too_long",
            None,
            format!("title must be at most {limit} characters"),
        ));
    }
}

fn lint_items(intent: &Intent, out: &mut Vec<LintIssue>) {
    let item_limit = DATA["limits"]["item_words"].as_u64().unwrap_or(25) as usize;
    let sentence_limit = DATA["limits"]["sentence_words"].as_u64().unwrap_or(30) as usize;
    for item in &intent.acceptance {
        if words(&item.text).len() > item_limit {
            out.push(issue(
                "item_too_long",
                Some(item.line),
                format!("{} exceeds {item_limit} words", item.id),
            ));
        }
        let plain = without_inline_code(&item.text);
        if has_match(&plain, &DATA["hedge_words"]) || has_match(&plain, &DATA["hedge_phrases"]) {
            out.push(issue(
                "hedge",
                Some(item.line),
                format!("{} contains a hedge; state an observable result", item.id),
            ));
        }
        lint_banned(&item.text, &item.id, Some(item.line), out);
        if sentence_count(&item.text)
            .iter()
            .any(|sentence| words(sentence).len() > sentence_limit)
        {
            out.push(issue(
                "sentence_too_long",
                Some(item.line),
                format!("{} has a sentence over {sentence_limit} words", item.id),
            ));
        }
    }
}

fn lint_words_and_phrases(intent: &Intent, out: &mut Vec<LintIssue>) {
    let plain = without_inline_code(&intent.brief);
    lint_banned(&intent.brief, "Brief", None, out);
    if has_match(&plain, &DATA["hedge_words"]) || has_match(&plain, &DATA["hedge_phrases"]) {
        out.push(issue(
            "hedge",
            None,
            "Brief contains a hedge; state an observable result",
        ));
    }
}

fn lint_banned(text: &str, section: &str, line: Option<usize>, out: &mut Vec<LintIssue>) {
    let plain = without_inline_code(text);
    for phrase in DATA["banned_words"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(DATA["banned_phrases"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
    {
        if contains_term(&plain, phrase) {
            out.push(issue(
                "banned_phrase",
                line,
                format!("{section} contains banned phrase \"{phrase}\""),
            ));
        }
    }
}

fn lint_sentences(intent: &Intent, out: &mut Vec<LintIssue>) {
    let limit = DATA["limits"]["sentence_words"].as_u64().unwrap_or(30) as usize;
    if sentence_count(&intent.brief)
        .iter()
        .any(|sentence| words(sentence).len() > limit)
    {
        out.push(issue(
            "sentence_too_long",
            None,
            format!("Brief has a sentence over {limit} words"),
        ));
    }
}

fn lint_notes(intent: &Intent, shaping: bool, out: &mut Vec<LintIssue>) {
    let code_limit = DATA["limits"]["notes_code_block_lines"]
        .as_u64()
        .unwrap_or(15) as usize;
    let mut fence: Option<&str> = None;
    let mut code_lines = 0;
    for line in intent.notes.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
                if code_lines > code_limit {
                    out.push(issue(
                        "long_code_block",
                        None,
                        format!("Notes code blocks must contain at most {code_limit} lines"),
                    ));
                }
            } else {
                code_lines += 1;
            }
        } else if trimmed.starts_with("```") {
            fence = Some("```");
            code_lines = 0;
        } else if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            code_lines = 0;
        }
    }
    if fence.is_some() && code_lines > code_limit {
        out.push(issue(
            "long_code_block",
            None,
            format!("Notes code blocks must contain at most {code_limit} lines"),
        ));
    }
    if shaping && !valid_approach(&intent.notes) {
        let min = DATA["limits"]["approach_min_words"].as_u64().unwrap_or(8) as usize;
        out.push(issue(
            "missing_approach",
            None,
            format!("Notes must start with Approach: naming the code path (at least {min} words and an action verb)"),
        ));
    }
}

fn valid_approach(notes: &str) -> bool {
    let trimmed = notes.trim_start();
    let approach = trimmed
        .get(..9)
        .filter(|prefix| prefix.eq_ignore_ascii_case("Approach:"))
        .map(|_| trimmed[9..].trim_start())
        .unwrap_or(trimmed);
    let min = DATA["limits"]["approach_min_words"].as_u64().unwrap_or(8) as usize;
    let tokens = words(approach);
    !tokens.is_empty()
        && tokens.len() >= min
        && has_match(&tokens[0..1].join(" "), &DATA["action_verbs"])
}

fn has_match(text: &str, values: &Value) -> bool {
    values
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|value| contains_term(text, value))
}

fn contains_term(text: &str, term: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let term = term.to_ascii_lowercase();
    text.match_indices(&term).any(|(start, _)| {
        let end = start + term.len();
        let left = text[..start].chars().next_back();
        let right = text[end..].chars().next();
        left.is_none_or(|character| !word_char(character))
            && right.is_none_or(|character| !word_char(character))
    })
}

fn without_inline_code(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut inside = false;
    for character in text.chars() {
        if character == '`' {
            inside = !inside;
            output.push(' ');
        } else if inside {
            output.push(' ');
        } else {
            output.push(character);
        }
    }
    output
}

fn paragraph_count(text: &str) -> usize {
    let mut count = 0;
    let mut in_paragraph = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            if in_paragraph {
                count += 1;
                in_paragraph = false;
            }
        } else {
            in_paragraph = true;
        }
    }
    count + usize::from(in_paragraph)
}

fn sentence_count(text: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let chars = text.char_indices().collect::<Vec<_>>();
    for (index, (offset, character)) in chars.iter().copied().enumerate() {
        if matches!(character, '.' | '!' | '?')
            && chars
                .get(index + 1)
                .is_some_and(|(_, next)| next.is_whitespace())
        {
            sentences.push(text[start..offset + character.len_utf8()].trim());
            start = offset + character.len_utf8();
        }
    }
    if !text[start..].trim().is_empty() {
        sentences.push(text[start..].trim());
    }
    sentences
}

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

fn word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn issue(rule: &'static str, line: Option<usize>, message: impl Into<String>) -> LintIssue {
    LintIssue {
        rule,
        severity: LintSeverity::Style,
        line,
        message: message.into(),
    }
}

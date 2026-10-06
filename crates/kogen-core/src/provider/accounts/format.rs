//! Parser and deterministic writer for the machine account YAML subset.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const HEADER: &str = "# Kogen accounts on this machine, written by kogen provider use.\n";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderAccounts {
    pub default: Option<String>,
    pub projects: BTreeMap<PathBuf, String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    pub default: Option<String>,
    pub projects: BTreeMap<PathBuf, String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccountFile {
    pub chatgpt: ProviderAccounts,
    pub grok: ProviderAccounts,
    pub selection: Selection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountParseError;

pub fn parse(text: &str) -> Result<AccountFile, AccountParseError> {
    let mut result = AccountFile::default();
    let mut section = Section::None;
    let mut section_populated = false;
    let mut seen = BTreeSet::new();
    let mut current_project: Option<PathBuf> = None;

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.contains('\t') {
            return Err(AccountParseError);
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        let content = line.trim_start_matches(' ');
        if indent == 0 {
            if current_project.is_some()
                || (matches!(
                    section,
                    Section::Chatgpt | Section::Grok | Section::Selection
                ) && !section_populated)
            {
                return Err(AccountParseError);
            }
            current_project = None;
            let (key, value) = content.split_once(':').ok_or(AccountParseError)?;
            section = match key {
                "chatgpt" => Section::Chatgpt,
                "grok" => Section::Grok,
                "selection" => Section::Selection,
                _ => return Err(AccountParseError),
            };
            if !seen.insert(section) {
                return Err(AccountParseError);
            }
            section_populated = false;
            match value.trim() {
                "{}" if section != Section::Selection => {
                    if section == Section::Chatgpt {
                        result.chatgpt = ProviderAccounts::default();
                    } else {
                        result.grok = ProviderAccounts::default();
                    }
                    section_populated = true;
                }
                "" => {}
                _ => return Err(AccountParseError),
            }
            continue;
        }

        match section {
            Section::Chatgpt | Section::Grok => {
                let target = if section == Section::Chatgpt {
                    &mut result.chatgpt
                } else {
                    &mut result.grok
                };
                match indent {
                    2 if content == "projects:" => {
                        if current_project.is_some() {
                            return Err(AccountParseError);
                        }
                        section_populated = true;
                        current_project = None;
                    }
                    2 if content.starts_with("default:") => {
                        let value = scalar(content, "default:")?;
                        if !super::valid_label(&value) || target.default.replace(value).is_some() {
                            return Err(AccountParseError);
                        }
                        section_populated = true;
                        current_project = None;
                    }
                    4 if content.starts_with("- path:") => {
                        if current_project.is_some() {
                            return Err(AccountParseError);
                        }
                        let value = scalar(content, "- path:")?;
                        let path = quoted_path(&value)?;
                        if target.projects.contains_key(&path) {
                            return Err(AccountParseError);
                        }
                        current_project = Some(path);
                    }
                    6 if content.starts_with("account:") => {
                        let value = scalar(content, "account:")?;
                        if !super::valid_label(&value) {
                            return Err(AccountParseError);
                        }
                        let path = current_project.take().ok_or(AccountParseError)?;
                        target.projects.insert(path, value);
                        section_populated = true;
                    }
                    _ => return Err(AccountParseError),
                }
            }
            Section::Selection => match indent {
                2 if content == "projects:" => {
                    if current_project.is_some() {
                        return Err(AccountParseError);
                    }
                    section_populated = true;
                    current_project = None;
                }
                2 if content.starts_with("default:") => {
                    let value = scalar(content, "default:")?;
                    if !matches!(value.as_str(), "chatgpt" | "grok")
                        || result.selection.default.replace(value).is_some()
                    {
                        return Err(AccountParseError);
                    }
                    section_populated = true;
                    current_project = None;
                }
                4 if content.starts_with("- path:") => {
                    if current_project.is_some() {
                        return Err(AccountParseError);
                    }
                    let value = scalar(content, "- path:")?;
                    let path = quoted_path(&value)?;
                    if result.selection.projects.contains_key(&path) {
                        return Err(AccountParseError);
                    }
                    current_project = Some(path);
                }
                6 if content.starts_with("provider:") => {
                    let value = scalar(content, "provider:")?;
                    if !matches!(value.as_str(), "chatgpt" | "grok") {
                        return Err(AccountParseError);
                    }
                    let path = current_project.take().ok_or(AccountParseError)?;
                    result.selection.projects.insert(path, value);
                    section_populated = true;
                }
                _ => return Err(AccountParseError),
            },
            Section::None => return Err(AccountParseError),
        }
    }

    if current_project.is_some()
        || (matches!(
            section,
            Section::Chatgpt | Section::Grok | Section::Selection
        ) && !section_populated)
    {
        return Err(AccountParseError);
    }
    Ok(result)
}

pub fn render(file: &AccountFile) -> String {
    let mut out = String::from(HEADER);
    render_provider(&mut out, "chatgpt", &file.chatgpt);
    render_provider(&mut out, "grok", &file.grok);
    render_selection(&mut out, &file.selection);
    out
}

fn scalar(line: &str, prefix: &str) -> Result<String, AccountParseError> {
    let value = line.strip_prefix(prefix).ok_or(AccountParseError)?.trim();
    if value.is_empty() {
        return Err(AccountParseError);
    }
    Ok(value.to_owned())
}

fn quoted_path(value: &str) -> Result<PathBuf, AccountParseError> {
    if !value.starts_with('"') {
        return Err(AccountParseError);
    }
    let decoded: String = serde_json::from_str(value).map_err(|_| AccountParseError)?;
    if decoded.is_empty() {
        return Err(AccountParseError);
    }
    Ok(PathBuf::from(decoded))
}

fn render_provider(out: &mut String, name: &str, provider: &ProviderAccounts) {
    if provider.default.is_none() && !provider.projects.keys().any(|path| path.is_dir()) {
        return;
    }
    out.push_str(name);
    out.push_str(":\n");
    if let Some(label) = &provider.default {
        out.push_str("  default: ");
        out.push_str(label);
        out.push('\n');
    }
    render_projects(out, &provider.projects, "account");
}

fn render_selection(out: &mut String, selection: &Selection) {
    if selection.default.is_none() && !selection.projects.keys().any(|path| path.is_dir()) {
        return;
    }
    out.push_str("selection:\n");
    if let Some(provider) = &selection.default {
        out.push_str("  default: ");
        out.push_str(provider);
        out.push('\n');
    }
    render_projects(out, &selection.projects, "provider");
}

fn render_projects(out: &mut String, projects: &BTreeMap<PathBuf, String>, value_key: &str) {
    let existing: Vec<_> = projects.iter().filter(|(path, _)| path.is_dir()).collect();
    if existing.is_empty() {
        return;
    }
    out.push_str("  projects:\n");
    for (path, value) in existing {
        let quoted =
            serde_json::to_string(&path.to_string_lossy()).unwrap_or_else(|_| "\"\"".into());
        out.push_str("    - path: ");
        out.push_str(&quoted);
        out.push('\n');
        out.push_str("      ");
        out.push_str(value_key);
        out.push_str(": ");
        out.push_str(value);
        out.push('\n');
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Section {
    None,
    Chatgpt,
    Grok,
    Selection,
}

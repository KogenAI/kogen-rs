use super::model::Witness;
use crate::git::GitRepo;
use crate::intent::{Intent, intent_sha256};
use crate::project::ProjectResolution;
use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub(super) struct ManifestResult {
    pub hashes: BTreeMap<String, String>,
    pub behind: Vec<String>,
}

pub(super) fn protected_manifest(
    project: &ProjectResolution,
    base_sha: &str,
    intent: &Intent,
    intent_bytes: &[u8],
    acceptance_path: &str,
    acceptance_bytes: &[u8],
) -> Result<ManifestResult, String> {
    let origin = GitRepo::new(&project.origin);
    let base_paths = origin
        .list_paths(base_sha)
        .map_err(|error| error.to_string())?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let own = format!(".kogen/intents/{}/intent.md", intent.slug);
    let mut selected = BTreeSet::from([own.clone(), acceptance_path.to_owned()]);

    let protected = strings(config_value(project, "protected_paths"));
    for pattern in protected {
        let pattern = if pattern.ends_with('/') {
            format!("{pattern}**")
        } else {
            pattern
        };
        let expanded = expand_braces(&pattern);
        let matched = base_paths
            .iter()
            .filter(|path| expanded.iter().any(|glob| glob_matches(glob, path)))
            .cloned()
            .collect::<Vec<_>>();
        if matched.is_empty() && !has_magic(&pattern) {
            selected.insert(pattern);
        } else {
            selected.extend(matched);
        }
    }

    if !intent.frontmatter.changes_gate {
        add_if_present(
            &mut selected,
            &base_paths,
            &project.checkout,
            ".kogen/project.yaml",
        );
        for path in strings(config_value(project, "gate_paths")) {
            add_if_present(&mut selected, &base_paths, &project.checkout, &path);
        }
        for key in ["checks", "acceptance_checks", "fix"] {
            for command in config_commands(project, key) {
                add_command_paths(&mut selected, &base_paths, &project.checkout, &command);
            }
        }
        if let Some(run) = config_value(project, "acceptance")
            .and_then(Value::as_mapping)
            .and_then(|map| map.get(Value::String("run".to_owned())))
            .and_then(Value::as_sequence)
        {
            for arg in run.iter().filter_map(Value::as_str) {
                add_command_paths(
                    &mut selected,
                    &base_paths,
                    &project.checkout,
                    &[arg.to_owned()],
                );
            }
        }
    }

    let mut hashes = BTreeMap::new();
    let mut behind = Vec::new();
    for path in selected {
        if path == own {
            hashes.insert(path, intent_sha256(intent_bytes));
            continue;
        }
        if path == acceptance_path {
            hashes.insert(path, intent_sha256(acceptance_bytes));
            continue;
        }
        let base_bytes = origin
            .blob_at(base_sha, &path)
            .map_err(|error| error.to_string())?;
        if !checkout_matches(&project.checkout.join(&path), base_bytes.as_deref()) {
            behind.push(path.clone());
        }
        let hash = base_bytes
            .as_deref()
            .map(intent_sha256)
            .unwrap_or_else(|| intent_sha256(b"kogen:absent"));
        hashes.insert(path, hash);
    }
    behind.sort();
    Ok(ManifestResult { hashes, behind })
}

pub(super) fn witness(
    project: &ProjectResolution,
    slug: &str,
    base_sha: &str,
    warnings_have_concern: bool,
) -> Result<(String, Option<Witness>), String> {
    let Some(config) = project.config.as_ref().map(|config| &config.raw) else {
        return Ok(("not checked".to_owned(), None));
    };
    let mode = config
        .as_mapping()
        .and_then(|map| map.get(Value::String("shaping".to_owned())))
        .and_then(Value::as_mapping)
        .and_then(|map| map.get(Value::String("proof".to_owned())))
        .and_then(Value::as_str);
    if mode != Some("witness") {
        return Ok(("not checked".to_owned(), None));
    }
    let repo = GitRepo::new(&project.origin);
    let ref_name = format!("refs/kogen/witness/{slug}");
    let Some(commit) = repo
        .ref_target(&ref_name)
        .map_err(|error| error.to_string())?
    else {
        return Ok(("UNPROVEN".to_owned(), None));
    };
    let diff = repo
        .output(&["diff", "--binary", base_sha, &commit])
        .map_err(|error| error.to_string())?;
    let diff_sha256 = intent_sha256(&diff);
    let verdict = if warnings_have_concern {
        "PROVEN_WITH_CONCERNS"
    } else {
        "PROVEN"
    };
    Ok((
        verdict.replace('_', " "),
        Some(Witness {
            verdict: verdict.to_owned(),
            commit,
            diff_sha256,
            base_sha: base_sha.to_owned(),
        }),
    ))
}

pub(super) fn baseline_warning(rows: &[super::model::BaselineRow]) -> bool {
    rows.iter().any(|row| row.status == "red")
}

pub(super) fn baseline_warning_lines(rows: &[super::model::BaselineRow]) -> Vec<String> {
    let mut lines = Vec::new();
    for row in rows.iter().filter(|row| row.status == "red") {
        for finding in row.findings.iter().take(5) {
            let detail = if finding.symbol.is_empty() {
                format!(
                    "{}:{}: {}",
                    finding.path,
                    finding.line.unwrap_or_default(),
                    finding.message
                )
            } else {
                format!(
                    "{}:{}: {}: {}",
                    finding.path,
                    finding.line.unwrap_or_default(),
                    finding.symbol,
                    finding.message
                )
            };
            lines.push(format!("  - {}: [{}] {detail}", row.name, finding.rule));
        }
    }
    lines
}

fn add_command_paths(
    selected: &mut BTreeSet<String>,
    base: &BTreeSet<String>,
    checkout: &Path,
    argv: &[String],
) {
    let Some(program) = argv.first() else { return };
    if program.rsplit('/').next() == Some("make") {
        for makefile in ["Makefile", "GNUmakefile", "makefile"] {
            add_if_present(selected, base, checkout, makefile);
        }
        return;
    }
    let interpreters = [
        "sh", "bash", "zsh", "dash", "python", "python3", "ruby", "node", "perl", "elixir",
        "escript",
    ];
    let chosen = if interpreters.contains(&program.as_str()) {
        argv.iter().skip(1).find(|arg| !arg.starts_with('-'))
    } else {
        Some(program)
    };
    if let Some(path) = chosen {
        let path = path.strip_prefix("./").unwrap_or(path);
        if base.contains(path) || exact_exists(checkout, path) {
            selected.insert(path.to_owned());
        }
    }
}

fn add_if_present(
    selected: &mut BTreeSet<String>,
    base: &BTreeSet<String>,
    checkout: &Path,
    path: &str,
) {
    if base.contains(path) || exact_exists(checkout, path) {
        selected.insert(path.to_owned());
    }
}

fn exact_exists(checkout: &Path, path: &str) -> bool {
    let mut current = checkout.to_path_buf();
    for segment in path.split('/') {
        let Ok(entries) = fs::read_dir(&current) else {
            return false;
        };
        let Some(found) = entries
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().as_ref() == segment)
        else {
            return false;
        };
        current = found.path();
    }
    current.exists()
}

fn config_commands(project: &ProjectResolution, key: &str) -> Vec<Vec<String>> {
    config_value(project, key)
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row.as_mapping()?
                .get(Value::String("argv".to_owned()))?
                .as_sequence()
                .map(|argv| {
                    argv.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
        })
        .collect()
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn config_value<'a>(project: &'a ProjectResolution, field: &str) -> Option<&'a Value> {
    project
        .config
        .as_ref()?
        .raw
        .as_mapping()?
        .get(Value::String(field.to_owned()))
}

fn checkout_matches(path: &Path, expected: Option<&[u8]>) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            expected.is_some_and(|expected| fs::read(path).is_ok_and(|actual| actual == expected))
        }
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => expected.is_none(),
        Err(_) => false,
    }
}

fn has_magic(pattern: &str) -> bool {
    pattern
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b'{'))
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(start) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(relative_end) = pattern[start + 1..].find('}') else {
        return vec![pattern.to_owned()];
    };
    let end = start + 1 + relative_end;
    pattern[start + 1..end]
        .split(',')
        .flat_map(|alternative| {
            let expanded = format!(
                "{}{}{}",
                &pattern[..start],
                alternative,
                &pattern[end + 1..]
            );
            expand_braces(&expanded)
        })
        .collect()
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_bytes(pattern.as_bytes(), path.as_bytes())
}

fn glob_bytes(pattern: &[u8], path: &[u8]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    match pattern[0] {
        b'*' if pattern.get(1) == Some(&b'*') => {
            let mut rest = &pattern[2..];
            if rest.first() == Some(&b'/') {
                rest = &rest[1..];
                if glob_bytes(rest, path) {
                    return true;
                }
            }
            (0..=path.len()).any(|index| glob_bytes(rest, &path[index..]))
        }
        b'*' => (0..=path.len()).any(|index| {
            !path[..index].contains(&b'/') && glob_bytes(&pattern[1..], &path[index..])
        }),
        b'?' => path
            .first()
            .is_some_and(|byte| *byte != b'/' && glob_bytes(&pattern[1..], &path[1..])),
        b'[' => match_class(pattern, path),
        byte => path.first() == Some(&byte) && glob_bytes(&pattern[1..], &path[1..]),
    }
}

fn match_class(pattern: &[u8], path: &[u8]) -> bool {
    let Some(end) = pattern.iter().position(|byte| *byte == b']') else {
        return path.first() == Some(&b'[') && glob_bytes(&pattern[1..], &path[1..]);
    };
    let Some(value) = path.first().copied().filter(|value| *value != b'/') else {
        return false;
    };
    let mut included = false;
    let mut index = 1;
    while index < end {
        if index + 2 < end && pattern[index + 1] == b'-' {
            included |= (pattern[index]..=pattern[index + 2]).contains(&value);
            index += 3;
        } else {
            included |= pattern[index] == value;
            index += 1;
        }
    }
    included && glob_bytes(&pattern[end + 1..], &path[1..])
}

#[cfg(test)]
mod tests {
    use super::baseline_warning_lines;
    use crate::approval::model::{BaselineRow, Finding};

    #[test]
    fn baseline_warning_omits_the_empty_symbol_separator() {
        let rows = [BaselineRow {
            name: "lint".to_owned(),
            status: "red".to_owned(),
            exit_status: Some(1),
            findings: vec![Finding {
                path: "lib/greet.txt".to_owned(),
                rule: "lint/todo".to_owned(),
                symbol: String::new(),
                message: "greet.txt: TODO found".to_owned(),
                line: Some(2),
            }],
        }];

        assert_eq!(
            baseline_warning_lines(&rows),
            ["  - lint: [lint/todo] lib/greet.txt:2: greet.txt: TODO found"]
        );
    }
}

//! Machine account mappings and the public provider-list presentation.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::provider_error;

mod format;
pub use format::{AccountFile, AccountParseError, ProviderAccounts, Selection, parse, render};

pub fn accounts_path(home: &Path) -> PathBuf {
    home.join(".kogen/accounts.yaml")
}

pub fn profiles_path(home: &Path) -> PathBuf {
    home.join(".kogen/profiles.json")
}

pub fn read(home: &Path) -> Result<AccountFile, super::CoreError> {
    let path = accounts_path(home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AccountFile::default());
        }
        Err(_) => return Err(invalid_accounts(&path)),
    };
    parse(&text).map_err(|_| invalid_accounts(&path))
}

pub fn select_provider(
    file: &AccountFile,
    project: &Path,
    requested: Option<&str>,
) -> Result<String, super::CoreError> {
    if let Some(provider) = requested {
        return match provider {
            "chatgpt" | "grok" => Ok(provider.to_owned()),
            _ => Err(provider_error(
                "invalid_provider",
                "KOGEN_BENCH_PROVIDER must be chatgpt or grok",
            )),
        };
    }
    let project = canonical_or_input(project);
    Ok(file
        .selection
        .projects
        .get(&project)
        .cloned()
        .or_else(|| file.selection.default.clone())
        .unwrap_or_else(|| "chatgpt".to_owned()))
}

pub fn select_account(
    file: &AccountFile,
    provider: &str,
    project: &Path,
    requested: Option<&str>,
    committed_chatgpt_account: Option<&str>,
) -> Result<String, super::CoreError> {
    if let Some(label) = requested {
        return if valid_label(label) {
            Ok(label.to_owned())
        } else {
            Err(provider_error(
                "invalid_account_label",
                "invalid account label",
            ))
        };
    }
    let mapping = provider_accounts(file, provider);
    let project = canonical_or_input(project);
    let committed = if provider == "chatgpt" {
        committed_chatgpt_account
    } else {
        None
    };
    if let Some(label) = committed
        && !valid_label(label)
    {
        return Err(provider_error(
            "invalid_account_label",
            "invalid account label",
        ));
    }
    Ok(mapping
        .and_then(|accounts| accounts.projects.get(&project).cloned())
        .or_else(|| committed.map(str::to_owned))
        .or_else(|| mapping.and_then(|accounts| accounts.default.clone()))
        .unwrap_or_else(|| "default".to_owned()))
}

pub fn set_use(home: &Path, label: &str, project: Option<&Path>) -> Result<(), super::CoreError> {
    set_provider_use(home, "chatgpt", label, project)
}

pub fn set_provider_use(
    home: &Path,
    provider: &str,
    label: &str,
    project: Option<&Path>,
) -> Result<(), super::CoreError> {
    if !matches!(provider, "chatgpt" | "grok") {
        return Err(provider_error(
            "invalid_provider",
            "provider must be chatgpt or grok",
        ));
    }
    if !valid_label(label) {
        return Err(provider_error(
            "invalid_account_label",
            "invalid account label",
        ));
    }
    let mut file = read(home)?;
    let account_map = if provider == "grok" {
        &mut file.grok
    } else {
        &mut file.chatgpt
    };
    if let Some(project) = project {
        let path = fs::canonicalize(project).map_err(|_| {
            super::environment_error(
                "project_not_found",
                format!("project path {} does not exist", project.display()),
            )
        })?;
        account_map.projects.insert(path.clone(), label.to_owned());
        file.selection.projects.insert(path, provider.to_owned());
    } else {
        account_map.default = Some(label.to_owned());
        file.selection.default = Some(provider.to_owned());
    }
    write(home, &file)
}

pub fn write(home: &Path, file: &AccountFile) -> Result<(), super::CoreError> {
    let path = accounts_path(home);
    write_private_atomic(&path, render(file).as_bytes()).map_err(|_| {
        super::environment_error(
            "accounts_write_failed",
            format!("could not write {}", path.display()),
        )
    })
}

pub fn list(home: &Path) -> Result<String, super::CoreError> {
    let file = read(home)?;
    let path = profiles_path(home);
    let profiles: Value = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
            super::environment_error(
                "invalid_profiles_file",
                format!("{} is not valid", path.display()),
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(_) => {
            return Err(super::environment_error(
                "invalid_profiles_file",
                format!("{} is not valid", path.display()),
            ));
        }
    };
    let Some(providers) = profiles.as_object() else {
        return Err(super::environment_error(
            "invalid_profiles_file",
            format!("{} is not valid", path.display()),
        ));
    };
    let count = ["chatgpt", "grok"]
        .iter()
        .filter_map(|provider| providers.get(*provider).and_then(Value::as_object))
        .map(Map::len)
        .sum::<usize>();
    if count == 0 {
        return Ok("chatgpt: not signed in\ngrok: not signed in\n".to_owned());
    }

    let selected_provider = file.selection.default.as_deref().unwrap_or("chatgpt");
    let mut out = String::new();
    for (provider, account_map) in [("chatgpt", &file.chatgpt), ("grok", &file.grok)] {
        let Some(labels) = providers.get(provider).and_then(Value::as_object) else {
            continue;
        };
        for (label, data) in labels {
            if !valid_label(label) {
                continue;
            }
            out.push_str(provider);
            out.push(':');
            out.push_str(label);
            if selected_provider == provider && account_map.default.as_deref() == Some(label) {
                out.push_str(" (default)");
            }
            let signed_in = data
                .get("signed_in")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            out.push_str(if signed_in {
                " signed in"
            } else {
                " signed out"
            });
            if let Some(email) = data.get("email").and_then(Value::as_str) {
                out.push(' ');
                out.push_str(email);
            }
            if let Some(expires) = data.get("expires_at").and_then(Value::as_i64) {
                out.push_str(" expires=");
                out.push_str(&expires.to_string());
            }
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn valid_label(label: &str) -> bool {
    let mut bytes = label.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'))
        && label.len() <= 64
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn provider_accounts<'a>(file: &'a AccountFile, provider: &str) -> Option<&'a ProviderAccounts> {
    match provider {
        "chatgpt" => Some(&file.chatgpt),
        "grok" => Some(&file.grok),
        _ => None,
    }
}

fn invalid_accounts(path: &Path) -> super::CoreError {
    super::environment_error(
        "invalid_accounts_file",
        format!("{} is not valid; fix or delete it", path.display()),
    )
}

fn canonical_or_input(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().expect("state path has parent");
    fs::create_dir_all(parent)?;
    set_private_dir(parent)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".accounts-{}-{stamp}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)
}

fn set_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AccountFile, parse, render, valid_label};

    #[test]
    fn parses_empty_and_project_account_maps() {
        let empty =
            "# Kogen accounts on this machine, written by kogen provider use.\nchatgpt: {}\n";
        assert_eq!(parse(empty), Ok(AccountFile::default()));
        let project = std::env::current_dir().unwrap();
        let quoted = serde_json::to_string(&project.to_string_lossy()).unwrap();
        let row = format!(
            "# Kogen accounts on this machine, written by kogen provider use.\nchatgpt:\n  default: work\n  projects:\n    - path: {quoted}\n      account: work\n"
        );
        let parsed = parse(&row).unwrap();
        assert_eq!(
            parsed.chatgpt.projects.get(&project),
            Some(&"work".to_owned())
        );
        assert_eq!(render(&parsed), row);
    }

    #[test]
    fn labels_follow_the_fixed_ascii_contract() {
        assert!(valid_label("A0._-z"));
        assert!(!valid_label(""));
        assert!(!valid_label("-bad"));
        assert!(!valid_label("a".repeat(65).as_str()));
    }
}

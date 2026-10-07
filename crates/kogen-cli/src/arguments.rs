use std::path::{Path, PathBuf};

use crate::request::{Command, Options, ProjectOptions, Route};

pub(super) fn scan_options(route: Route, tail: &[String]) -> Options {
    let mut options = Options::default();
    let mut options_ended = false;
    let mut index = 0;
    while index < tail.len() {
        let token = tail[index].as_str();
        if options_ended || token == "-" || !token.starts_with('-') {
            index += 1;
            continue;
        }
        if token == "--" {
            options_ended = true;
            index += 1;
            continue;
        }

        let (name, attached) = token
            .split_once('=')
            .map_or((token, None), |(name, value)| (name, Some(value)));
        match name {
            "--json" | "--watch" | "--force" | "--detach" => {
                if attached.is_some() {
                    options.boolean_value.get_or_insert(name.to_owned());
                } else {
                    match name {
                        "--json" => options.json = true,
                        "--watch" => options.watch = true,
                        "--force" => options.force = true,
                        "--detach" => options.detach = true,
                        _ => unreachable!(),
                    }
                    if !option_allowed(route, name) {
                        options.disallowed_option.get_or_insert(name.to_owned());
                    }
                }
            }
            "--project" | "--origin" | "--base" | "--by" | "--as" => {
                if !option_allowed(route, name) {
                    options.disallowed_option.get_or_insert(name.to_owned());
                }
                let value = if let Some(value) = attached {
                    Some(value.to_owned())
                } else if index + 1 < tail.len() && tail[index + 1] != "--" {
                    index += 1;
                    Some(tail[index].clone())
                } else {
                    None
                };
                if let Some(value) = value {
                    match name {
                        "--project" => options.project = Some(value),
                        "--origin" => options.origin = Some(value),
                        "--base" => options.base = Some(value),
                        "--by" => options.by = Some(value),
                        "--as" => options.as_label = Some(value),
                        _ => unreachable!(),
                    }
                } else {
                    options.missing_value.get_or_insert(name.to_owned());
                }
            }
            _ => {
                options.unknown_option.get_or_insert(name.to_owned());
            }
        }
        index += 1;
    }
    options
}

fn option_allowed(route: Route, option: &str) -> bool {
    match route {
        Route::Status => matches!(
            option,
            "--project" | "--origin" | "--base" | "--watch" | "--json"
        ),
        Route::IntentShape | Route::IntentApprove | Route::IntentRemove => {
            matches!(option, "--project" | "--origin" | "--base")
                || (route == Route::IntentApprove && option == "--by")
                || (route == Route::IntentRemove && option == "--force")
        }
        Route::QueueStart | Route::QueueStop => {
            matches!(option, "--project" | "--origin" | "--base")
                || (route == Route::QueueStart && option == "--detach")
        }
        Route::ProviderUse => matches!(option, "--project" | "--as"),
        Route::ProviderList | Route::ProviderLogin | Route::ProviderLogout | Route::Version => {
            false
        }
    }
}

pub(super) fn collect_positionals(tail: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    let mut options_ended = false;
    let mut index = 0;
    while index < tail.len() {
        let token = tail[index].as_str();
        if options_ended {
            values.push(token.to_owned());
            index += 1;
            continue;
        }
        if token == "--" {
            options_ended = true;
            index += 1;
            continue;
        }
        if token == "-" || !token.starts_with('-') {
            values.push(token.to_owned());
            index += 1;
            continue;
        }
        let name = token.split_once('=').map_or(token, |(name, _)| name);
        if is_value_option(name) && !token.contains('=') && index + 1 < tail.len() {
            index += 2;
        } else {
            index += 1;
        }
    }
    values
}

fn is_value_option(name: &str) -> bool {
    matches!(name, "--project" | "--origin" | "--base" | "--by" | "--as")
}

pub(super) fn missing_positional(route: Route, count: usize) -> Option<&'static str> {
    match route {
        Route::IntentShape if count < 1 => Some("<slug>"),
        Route::IntentShape if count < 2 => Some("<file|->"),
        Route::IntentApprove | Route::IntentRemove if count < 1 => Some("<slug>"),
        Route::ProviderLogin | Route::ProviderLogout | Route::ProviderUse if count < 1 => {
            Some("<provider>")
        }
        _ => None,
    }
}

pub(super) fn unexpected_positional(route: Route, values: &[String]) -> Option<&str> {
    let max = match route {
        Route::Status => 1,
        Route::IntentShape => 2,
        Route::IntentApprove => 2,
        Route::IntentRemove | Route::ProviderLogin | Route::ProviderLogout | Route::ProviderUse => {
            1
        }
        Route::QueueStart | Route::QueueStop | Route::ProviderList | Route::Version => 0,
    };
    values.get(max).map(String::as_str)
}

pub(super) fn build_command(
    route: Route,
    values: Vec<String>,
    options: Options,
    cwd: &Path,
) -> Result<Command, String> {
    match route {
        Route::Status => {
            if options.watch && options.json {
                return Err("kogen status: --watch and --json can't be combined".to_owned());
            }
            Ok(Command::Status {
                slug: values.first().cloned(),
                watch: options.watch,
                json: options.json,
                project: project_options(&options, cwd),
            })
        }
        Route::IntentShape => Ok(Command::IntentShape {
            slug: values[0].clone(),
            request: values[1].clone(),
            project: project_options(&options, cwd),
        }),
        Route::IntentApprove => {
            if let Some(hash) = values.get(1)
                && !valid_hash_prefix(hash)
            {
                return Err(
                    "kogen intent approve: <hash> must be 6 to 64 lowercase hex characters"
                        .to_owned(),
                );
            }
            Ok(Command::IntentApprove {
                slug: values[0].clone(),
                hash: values.get(1).cloned(),
                by: options.by.clone(),
                project: project_options(&options, cwd),
            })
        }
        Route::IntentRemove => Ok(Command::IntentRemove {
            slug: values[0].clone(),
            force: options.force,
            project: project_options(&options, cwd),
        }),
        Route::QueueStart => Ok(Command::QueueStart {
            detach: options.detach,
            project: project_options(&options, cwd),
        }),
        Route::QueueStop => Ok(Command::QueueStop {
            project: project_options(&options, cwd),
        }),
        Route::ProviderList => Ok(Command::ProviderList),
        Route::ProviderLogin | Route::ProviderLogout | Route::ProviderUse => {
            let provider = &values[0];
            if !matches!(provider.as_str(), "chatgpt" | "grok") {
                let verb = match route {
                    Route::ProviderLogin => "login",
                    Route::ProviderLogout => "logout",
                    Route::ProviderUse => "use",
                    _ => unreachable!(),
                };
                return Err(format!(
                    "kogen provider {verb}: unknown provider '{provider}' (supported: chatgpt, grok)"
                ));
            }
            match route {
                Route::ProviderLogin => Ok(Command::ProviderLogin {
                    provider: provider.clone(),
                }),
                Route::ProviderLogout => Ok(Command::ProviderLogout {
                    provider: provider.clone(),
                }),
                Route::ProviderUse => {
                    let Some(label) = options.as_label.clone() else {
                        return Err("kogen provider use: missing --as <label>".to_owned());
                    };
                    Ok(Command::ProviderUse {
                        provider: provider.clone(),
                        label,
                        project: options
                            .project
                            .as_deref()
                            .map(|value| absolute_path(Some(value), cwd)),
                    })
                }
                _ => unreachable!(),
            }
        }
        Route::Version => Ok(Command::Version),
    }
}

fn project_options(options: &Options, cwd: &Path) -> ProjectOptions {
    ProjectOptions {
        project: absolute_path(options.project.as_deref(), cwd),
        origin: options
            .origin
            .as_deref()
            .map(|value| absolute_path(Some(value), cwd)),
        base: options.base.clone(),
    }
}

fn absolute_path(value: Option<&str>, cwd: &Path) -> PathBuf {
    match value {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }
        None => cwd.to_path_buf(),
    }
}

fn valid_hash_prefix(hash: &str) -> bool {
    (6..=64).contains(&hash.len())
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

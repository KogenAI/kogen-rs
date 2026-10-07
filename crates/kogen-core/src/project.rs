//! Project resolution, strict YAML configuration, and state-root identity.

mod schema;
pub mod yaml;

#[cfg(test)]
mod tests;

use schema::ValidatedConfig;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigIssue {
    pub line: Option<usize>,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError {
    pub path: PathBuf,
    pub issues: Vec<ConfigIssue>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        for issue in &self.issues {
            writeln!(f)?;
            if let Some(line) = issue.line {
                write!(f, "line {line}: ")?;
            }
            write!(f, "{}", issue.detail)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ProjectConfig {
    pub name: String,
    pub base: Option<String>,
    pub raw: serde_yaml::Value,
}

impl ProjectConfig {
    pub fn from_bytes(path: impl Into<PathBuf>, bytes: &[u8]) -> Result<Self, ConfigError> {
        let path = path.into();
        let value = yaml::parse(bytes).map_err(|error| ConfigError {
            path: path.clone(),
            issues: vec![ConfigIssue {
                line: error.line,
                detail: error.message,
            }],
        })?;
        schema::validate(value)
            .map(Self::from_validated)
            .map_err(|details| ConfigError {
                path,
                issues: details
                    .into_iter()
                    .map(|detail| ConfigIssue { line: None, detail })
                    .collect(),
            })
    }

    pub fn load(checkout: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = checkout.as_ref().join(".kogen/project.yaml");
        let bytes = fs::read(&path).map_err(|error| ConfigError {
            path: path.clone(),
            issues: vec![ConfigIssue {
                line: None,
                detail: error.to_string(),
            }],
        })?;
        Self::from_bytes(path, &bytes)
    }

    fn from_validated(value: ValidatedConfig) -> Self {
        Self {
            name: value.name,
            base: value.base,
            raw: value.raw,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProjectOptions {
    pub cwd: Option<PathBuf>,
    pub project: Option<PathBuf>,
    pub origin: Option<PathBuf>,
    pub base: Option<String>,
    pub home: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectError {
    ProjectUnavailable(PathBuf),
    NotGitWorkTree(PathBuf),
    InvalidConfig(ConfigError),
    BaseUnavailable(String),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectUnavailable(path) => write!(f, "{}", path.display()),
            Self::NotGitWorkTree(path) => write!(f, "{}", path.display()),
            Self::InvalidConfig(error) => write!(f, "{error}"),
            Self::BaseUnavailable(detail) => f.write_str(detail),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProjectResolution {
    pub checkout: PathBuf,
    pub origin: PathBuf,
    pub base: String,
    pub state_root: PathBuf,
    pub config: Option<ProjectConfig>,
}

impl ProjectResolution {
    pub fn resolve(options: &ProjectOptions) -> Result<Self, ProjectError> {
        let cwd = options
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let project_path = options
            .project
            .as_ref()
            .map_or_else(|| cwd.clone(), |path| absolute_from(&cwd, path));
        let canonical_project = fs::canonicalize(&project_path)
            .map_err(|_| ProjectError::ProjectUnavailable(project_path.clone()))?;
        if !canonical_project.is_dir() {
            return Err(ProjectError::ProjectUnavailable(project_path));
        }
        let top = git_output(&canonical_project, &["rev-parse", "--show-toplevel"])
            .ok_or_else(|| ProjectError::NotGitWorkTree(canonical_project.clone()))?;
        let checkout = fs::canonicalize(top.trim()).unwrap_or_else(|_| canonical_project.clone());
        let config_path = checkout.join(".kogen/project.yaml");
        let config = match fs::read(&config_path) {
            Ok(bytes) => Some(
                ProjectConfig::from_bytes(&config_path, &bytes)
                    .map_err(ProjectError::InvalidConfig)?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(ProjectError::InvalidConfig(ConfigError {
                    path: config_path,
                    issues: vec![ConfigIssue {
                        line: None,
                        detail: error.to_string(),
                    }],
                }));
            }
        };
        let origin = resolve_origin(options, &checkout);
        let base = resolve_base(options, config.as_ref(), &checkout, &origin)
            .ok_or_else(|| ProjectError::BaseUnavailable("base is not resolvable".to_owned()))?;
        let home = options
            .home
            .clone()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_default();
        let state_root = home.join(".kogen/workspaces").join(state_key(&checkout));
        Ok(Self {
            checkout,
            origin,
            base,
            state_root,
            config,
        })
    }

    pub fn ensure_state_root(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.state_root)
    }
}

pub fn valid_slug(slug: &str) -> bool {
    (3..=48).contains(&slug.len())
        && slug.is_ascii()
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}

fn resolve_origin(options: &ProjectOptions, checkout: &Path) -> PathBuf {
    if let Some(origin) = &options.origin {
        let path = absolute_from(checkout, origin);
        return fs::canonicalize(&path).unwrap_or(path);
    }
    let remote = git_output(checkout, &["config", "--get", "remote.origin.url"]);
    remote
        .as_deref()
        .and_then(|remote| local_origin_path(remote.trim(), checkout))
        .and_then(|path| fs::canonicalize(&path).ok())
        .unwrap_or_else(|| checkout.to_path_buf())
}

fn resolve_base(
    options: &ProjectOptions,
    config: Option<&ProjectConfig>,
    checkout: &Path,
    origin: &Path,
) -> Option<String> {
    if let Some(base) = options
        .base
        .as_deref()
        .or_else(|| config.and_then(|config| config.base.as_deref()))
    {
        if git_output(
            origin,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )
        .is_some()
        {
            return Some(base.to_owned());
        }
        return None;
    }
    if origin == checkout {
        if let Some(head) = git_output(
            checkout,
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        ) {
            return Some(head.trim().to_owned());
        }
    } else if let Some(head) = git_output(origin, &["symbolic-ref", "--short", "HEAD"]) {
        return Some(head.trim().to_owned());
    }
    git_output(checkout, &["branch", "--show-current"]).and_then(|branch| {
        let branch = branch.trim();
        (!branch.is_empty()).then(|| branch.to_owned())
    })
}

fn local_origin_path(remote: &str, checkout: &Path) -> Option<PathBuf> {
    if let Some(url) = remote.strip_prefix("file://") {
        let path = if let Some(path) = url.strip_prefix("localhost/") {
            format!("/{path}")
        } else if url.starts_with('/') {
            url.to_owned()
        } else {
            return None;
        };
        return Some(PathBuf::from(percent_decode(&path)?));
    }
    let scp_like = remote.split_once(':').is_some_and(|(host, _)| {
        host.contains('@') && !host.contains('/') && !host.starts_with('.')
    });
    if remote.contains("://") || scp_like {
        return None;
    }
    let expanded = if let Some(path) = remote.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME")?).join(path)
    } else {
        absolute_from(checkout, Path::new(remote))
    };
    let looks_like_repo = expanded.join("HEAD").is_file() || expanded.join(".git").exists();
    looks_like_repo.then_some(expanded)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn absolute_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn state_key(checkout: &Path) -> String {
    let basename = checkout
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("checkout");
    let mut safe = String::new();
    let mut in_replacement = false;
    for ch in basename.chars() {
        if ch.is_ascii_alphanumeric() || ".-_".contains(ch) {
            safe.push(ch);
            in_replacement = false;
        } else if !in_replacement {
            safe.push('-');
            in_replacement = true;
        }
    }
    safe.truncate(40);
    if safe.is_empty() {
        safe = "checkout".to_owned();
    }
    let digest = Sha256::digest(checkout.to_string_lossy().as_bytes());
    let short = digest[..5]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{safe}-{short}")
}

use crate::project::ProjectConfig;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

mod inputs;
use inputs::input;

#[derive(Clone, Debug)]
pub struct SetupKeyInput {
    pub base_tree: String,
    pub setup: Value,
    pub setup_outputs: Vec<String>,
    pub child_env: BTreeMap<String, String>,
    pub os: String,
    pub arch: String,
    pub elixir: String,
    pub otp: String,
    pub inputs: Option<Vec<SetupInput>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SetupInput {
    pub path: String,
    /// Git-compatible mode (100644, 100755, or 120000), encoded as an integer.
    pub mode: u32,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct SetupCacheKey {
    material: Value,
}

#[derive(Debug)]
pub enum SetupKeyError {
    InvalidConfig(String),
    InputPath(PathBuf),
    InputOutsideCheckout(PathBuf),
    InputNotFile(PathBuf),
    NonUnicodeEnvironment(OsString),
    Io { path: PathBuf, detail: String },
}

impl fmt::Display for SetupKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(detail) => formatter.write_str(detail),
            Self::InputPath(path) => {
                write!(formatter, "invalid setup input path {}", path.display())
            }
            Self::InputOutsideCheckout(path) => {
                write!(
                    formatter,
                    "setup input escapes checkout: {}",
                    path.display()
                )
            }
            Self::InputNotFile(path) => {
                write!(formatter, "setup input is not a file: {}", path.display())
            }
            Self::NonUnicodeEnvironment(name) => {
                write!(
                    formatter,
                    "setup environment variable is not UTF-8: {name:?}"
                )
            }
            Self::Io { path, detail } => write!(formatter, "{}: {detail}", path.display()),
        }
    }
}

impl SetupCacheKey {
    pub fn from_project(
        config: Option<&ProjectConfig>,
        checkout: &Path,
        base_tree: &str,
        environment: &BTreeMap<OsString, OsString>,
    ) -> Result<Self, SetupKeyError> {
        let root = fs::canonicalize(checkout).map_err(|error| SetupKeyError::Io {
            path: checkout.to_path_buf(),
            detail: error.to_string(),
        })?;
        let setup_inputs = config_values(config, "setup_inputs")?;
        let inputs = if setup_inputs.is_empty() {
            None
        } else {
            let mut values = setup_inputs
                .iter()
                .map(|path| input(&root, path))
                .collect::<Result<Vec<_>, _>>()?;
            values.sort_by(|left, right| left.path.cmp(&right.path));
            Some(values)
        };
        let mut child_env = BTreeMap::new();
        for (name, value) in environment {
            let name_text = name
                .to_str()
                .ok_or_else(|| SetupKeyError::NonUnicodeEnvironment(name.clone()))?;
            if excluded_env(name_text) {
                continue;
            }
            let value_text = value
                .to_str()
                .ok_or_else(|| SetupKeyError::NonUnicodeEnvironment(name.clone()))?;
            child_env.insert(name_text.to_owned(), value_text.to_owned());
        }
        let material = SetupKeyInput {
            base_tree: if inputs.is_some() {
                String::new()
            } else {
                base_tree.to_owned()
            },
            setup: config_value(config, "setup")?.unwrap_or_else(|| json!([])),
            setup_outputs: config_strings(config, "setup_outputs")?,
            elixir: runtime_value(environment, &["ELIXIR_VERSION", "MISE_ELIXIR_VERSION"])?
                .unwrap_or_default(),
            otp: runtime_value(
                environment,
                &["OTP_VERSION", "ERLANG_VERSION", "MISE_ERLANG_VERSION"],
            )?
            .unwrap_or_default(),
            child_env,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            inputs,
        };
        Self::from_input(material)
    }

    pub fn from_input(input: SetupKeyInput) -> Result<Self, SetupKeyError> {
        let mut material = Map::new();
        material.insert("v".to_owned(), json!(2));
        material.insert("base_tree".to_owned(), json!(input.base_tree));
        material.insert("setup".to_owned(), canonical(input.setup));
        material.insert("setup_outputs".to_owned(), json!(input.setup_outputs));
        material.insert("child_env".to_owned(), json!(input.child_env));
        material.insert("os".to_owned(), json!(input.os));
        material.insert("arch".to_owned(), json!(input.arch));
        material.insert("elixir".to_owned(), json!(input.elixir));
        material.insert("otp".to_owned(), json!(input.otp));
        material.insert(
            "inputs".to_owned(),
            input.inputs.map_or(Value::Null, |values| json!(values)),
        );
        Ok(Self {
            material: canonical(Value::Object(material)),
        })
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.material).expect("setup key material is valid JSON")
    }

    #[must_use]
    pub fn digest(&self) -> String {
        digest(&self.canonical_bytes())
    }

    pub fn digest_with_checks(&self, checks: &Value) -> Result<String, SetupKeyError> {
        let mut material = self.material.clone();
        let Some(object) = material.as_object_mut() else {
            return Err(SetupKeyError::InvalidConfig(
                "setup cache key is not a JSON object".to_owned(),
            ));
        };
        object.insert("checks".to_owned(), canonical(checks.clone()));
        serde_json::to_vec(&canonical(material))
            .map(|bytes| digest(&bytes))
            .map_err(|error| SetupKeyError::InvalidConfig(error.to_string()))
    }
}

fn config_value(
    config: Option<&ProjectConfig>,
    name: &str,
) -> Result<Option<Value>, SetupKeyError> {
    let Some(config) = config else {
        return Ok(None);
    };
    let Some(mapping) = config.raw.as_mapping() else {
        return Err(SetupKeyError::InvalidConfig(
            "project config is not a map".to_owned(),
        ));
    };
    mapping
        .get(serde_yaml::Value::String(name.to_owned()))
        .map(|value| {
            serde_json::to_value(value)
                .map(canonical)
                .map_err(|error| SetupKeyError::InvalidConfig(error.to_string()))
        })
        .transpose()
}

fn config_values(config: Option<&ProjectConfig>, name: &str) -> Result<Vec<String>, SetupKeyError> {
    let Some(value) = config_value(config, name)? else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| SetupKeyError::InvalidConfig(format!("{name} must be a list")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| SetupKeyError::InvalidConfig(format!("{name} must contain strings")))
        })
        .collect()
}

fn config_strings(
    config: Option<&ProjectConfig>,
    name: &str,
) -> Result<Vec<String>, SetupKeyError> {
    config_values(config, name)
}

fn runtime_value(
    environment: &BTreeMap<OsString, OsString>,
    names: &[&str],
) -> Result<Option<String>, SetupKeyError> {
    for name in names {
        if let Some(value) = environment.get(OsStr::new(name)) {
            return value
                .to_str()
                .map(|value| Some(value.to_owned()))
                .ok_or_else(|| SetupKeyError::NonUnicodeEnvironment(OsString::from(name)));
        }
    }
    Ok(None)
}

fn excluded_env(name: &str) -> bool {
    matches!(
        name,
        "TMPDIR" | "MISE_STATE_DIR" | "MISE_CACHE_DIR" | "MISE_TRUSTED_CONFIG_PATHS"
    )
}

fn canonical(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
        Value::Object(values) => {
            let ordered = values
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(ordered.into_iter().collect())
        }
        other => other,
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn key_io(path: &Path, error: std::io::Error) -> SetupKeyError {
    SetupKeyError::Io {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

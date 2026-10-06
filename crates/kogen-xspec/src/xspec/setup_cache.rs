use kogen_core::project::ProjectConfig;
use kogen_core::run::setup_cache::{
    SetupCacheKey, SetupCacheRequest, cached_entry_count, run_setup,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

pub(super) struct Replay {
    root: PathBuf,
    workdir: Option<PathBuf>,
    sequence: u64,
    observation: Value,
}

impl Replay {
    pub(super) fn new() -> Result<Self, String> {
        let id = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kogen-xspec-setup-cache-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(Self {
            root,
            workdir: None,
            sequence: 0,
            observation: initial_observation(),
        })
    }

    pub(super) fn reset(&mut self) -> Result<Value, String> {
        fs::remove_dir_all(&self.root).map_err(|error| error.to_string())?;
        fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        self.workdir = None;
        self.sequence = 0;
        self.observation = initial_observation();
        Ok(self.observe())
    }

    pub(super) fn observe(&self) -> Value {
        self.observation.clone()
    }

    pub(super) fn apply(&mut self, event: &Value) -> Result<Value, String> {
        match event.get("tag").and_then(Value::as_str) {
            Some("Run") => self.run(event.get("value").ok_or("Run requires value")?),
            Some("Mutate") => self.mutate(event.get("value").ok_or("Mutate requires value")?),
            Some(tag) => Err(format!("unknown setup-cache event {tag:?}")),
            None => Err("event requires string field `tag`".to_owned()),
        }
    }

    fn run(&mut self, value: &Value) -> Result<Value, String> {
        let base = string(value, "base")?;
        let variant = string(value, "variant")?;
        let tracked = boolean(value, "tracked")?;
        let input = string(value, "input")?;
        let enabled = boolean(value, "enabled")?;
        let stable = boolean(value, "stable")?;
        let ok = boolean(value, "ok")?;
        let payload = string(value, "payload")?;
        self.sequence += 1;
        let workdir = self.root.join(format!("work-{}", self.sequence));
        fs::create_dir_all(&workdir).map_err(|error| error.to_string())?;
        if tracked {
            fs::write(workdir.join("input.txt"), input).map_err(|error| error.to_string())?;
        }
        let config = project_config(&workdir, enabled, tracked, variant)?;
        let mut environment = BTreeMap::from([
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("ELIXIR_VERSION"), OsString::from("1.0.0")),
        ]);
        if !variant.is_empty() {
            environment.insert(OsString::from("CACHE_VARIANT"), OsString::from(variant));
        }
        let key = SetupCacheKey::from_project(Some(&config), &workdir, base, &environment)
            .map_err(|error| error.to_string())?
            .digest();
        let output_paths = if enabled {
            vec!["result.txt".to_owned()]
        } else {
            Vec::new()
        };
        let mut did_run = false;
        let outcome = run_setup(
            SetupCacheRequest {
                checkout: &workdir,
                cache_root: &self.root.join("setup-cache"),
                key: &key,
                outputs: output_paths,
                enabled,
            },
            || {
                did_run = true;
                if !ok {
                    return Err("setup failed".to_owned());
                }
                fs::write(workdir.join("result.txt"), payload)
                    .map_err(|error| error.to_string())?;
                if tracked && !stable {
                    fs::write(workdir.join("input.txt"), "changed-during-setup")
                        .map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(17)
            },
        );
        let successful = outcome.is_ok();
        let reused = outcome.as_ref().is_ok_and(|result| result.reused);
        let work = if successful {
            fs::read_to_string(workdir.join("result.txt")).unwrap_or_default()
        } else {
            String::new()
        };
        let setup_runs =
            self.observation["setupRuns"].as_u64().unwrap_or_default() + u64::from(did_run);
        self.workdir = Some(workdir);
        self.observation = json!({
            "entryCount": cached_entry_count(&self.root.join("setup-cache")),
            "work": work,
            "present": successful && self.workdir.as_ref().is_some_and(|path| path.join("result.txt").is_file()),
            "reused": reused,
            "setupRuns": setup_runs,
            "last": if !successful { "failed" } else if reused { "hit" } else { "miss" },
        });
        Ok(self.observe())
    }

    fn mutate(&mut self, value: &Value) -> Result<Value, String> {
        let payload = string(value, "payload")?;
        let Some(workdir) = &self.workdir else {
            self.observation["last"] = json!("no_output");
            return Ok(self.observe());
        };
        let output = workdir.join("result.txt");
        if !output.is_file() {
            self.observation["last"] = json!("no_output");
            return Ok(self.observe());
        }
        fs::write(&output, payload).map_err(|error| error.to_string())?;
        self.observation["work"] = json!(payload);
        self.observation["last"] = json!("ok");
        Ok(self.observe())
    }
}

impl Drop for Replay {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn project_config(
    workdir: &Path,
    enabled: bool,
    tracked: bool,
    variant: &str,
) -> Result<ProjectConfig, String> {
    let mut source = String::from("name: xspec\nchecks: []\n");
    if enabled {
        source.push_str(
            "setup:\n  - name: prepare\n    argv: [\"true\"]\n    timeout_ms: 1000\nsetup_outputs: [result.txt]\n",
        );
    } else {
        source.push_str("setup: []\nsetup_outputs: []\n");
    }
    if tracked {
        source.push_str("setup_inputs: [input.txt]\n");
    }
    if !variant.is_empty() {
        source.push_str(&format!("env:\n  CACHE_VARIANT: {variant}\n"));
    }
    ProjectConfig::from_bytes(workdir.join(".kogen/project.yaml"), source.as_bytes())
        .map_err(|error| error.to_string())
}

fn initial_observation() -> Value {
    json!({
        "entryCount": 0,
        "work": "",
        "present": false,
        "reused": false,
        "setupRuns": 0,
        "last": "ok",
    })
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("event requires string field `{key}`"))
}

fn boolean(value: &Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("event requires boolean field `{key}`"))
}

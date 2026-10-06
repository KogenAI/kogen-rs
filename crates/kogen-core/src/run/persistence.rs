//! Durable snapshots and journals for one Build run.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RunSnapshot {
    pub schema: u8,
    pub run_id: String,
    pub slug: String,
    pub approval_sha256: String,
    pub approval_commit: String,
    pub target_branch: String,
    pub status: String,
    pub landing: Option<LandingRecord>,
    pub owner_pid: u32,
    pub owner_started_ms: i64,
    pub started_ms: i64,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LandingRecord {
    pub approval_commit: String,
    pub run_id: String,
    pub expected_parent: String,
    pub final_tree: String,
    pub candidate_commit: String,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RunEvent {
    pub event: String,
    pub ts: i64,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

impl RunEvent {
    #[must_use]
    pub fn new(event: impl Into<String>, ts: i64) -> Self {
        Self {
            event: event.into(),
            ts,
            fields: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunPersistenceError {
    pub operation: &'static str,
    pub path: PathBuf,
    pub detail: String,
}

impl fmt::Display for RunPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}: {}",
            self.operation,
            self.path.display(),
            self.detail
        )
    }
}

impl std::error::Error for RunPersistenceError {}

#[derive(Clone, Debug)]
pub struct RunStore {
    directory: PathBuf,
}

impl RunStore {
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn create(&self, snapshot: &RunSnapshot) -> Result<(), RunPersistenceError> {
        fs::create_dir_all(&self.directory)
            .map_err(|error| persistence_error("create run directory", &self.directory, error))?;
        set_private_dir(&self.directory)
            .map_err(|error| persistence_error("protect run directory", &self.directory, error))?;
        self.write_snapshot(snapshot)
    }

    /// Append the journal entry durably, then publish the matching snapshot atomically.
    pub fn record(
        &self,
        event: &RunEvent,
        snapshot: &RunSnapshot,
    ) -> Result<(), RunPersistenceError> {
        let path = self.directory.join("events.jsonl");
        fs::create_dir_all(&self.directory)
            .map_err(|error| persistence_error("create run directory", &self.directory, error))?;
        set_private_dir(&self.directory)
            .map_err(|error| persistence_error("protect run directory", &self.directory, error))?;
        let mut file = private_append(&path)
            .map_err(|error| persistence_error("append run event", &path, error))?;
        serde_json::to_writer(&mut file, event)
            .map_err(|error| persistence_error("append run event", &path, error))?;
        file.write_all(b"\n")
            .and_then(|()| file.sync_all())
            .map_err(|error| persistence_error("append run event", &path, error))?;
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| persistence_error("sync run directory", &self.directory, error))?;
        self.write_snapshot(snapshot)
    }

    pub fn write_snapshot(&self, snapshot: &RunSnapshot) -> Result<(), RunPersistenceError> {
        let path = self.directory.join("run.json");
        let bytes = serde_json::to_vec(snapshot)
            .map_err(|error| persistence_error("encode run snapshot", &path, error))?;
        atomic_replace(&path, &bytes)
            .map_err(|error| persistence_error("publish run snapshot", &path, error))
    }

    pub fn read_snapshot(&self) -> Result<RunSnapshot, RunPersistenceError> {
        read_json(&self.directory.join("run.json"), "read run snapshot")
    }

    pub fn read_events(&self) -> Result<Vec<RunEvent>, RunPersistenceError> {
        let path = self.directory.join("events.jsonl");
        let file = File::open(&path)
            .map_err(|error| persistence_error("read run journal", &path, error))?;
        BufReader::new(file)
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let line =
                    line.map_err(|error| persistence_error("read run journal", &path, error))?;
                serde_json::from_str(&line).map_err(|error| {
                    persistence_error(
                        "decode run journal",
                        &path,
                        format!("line {}: {error}", index + 1),
                    )
                })
            })
            .collect()
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    operation: &'static str,
) -> Result<T, RunPersistenceError> {
    let bytes = fs::read(path).map_err(|error| persistence_error(operation, path, error))?;
    serde_json::from_slice(&bytes).map_err(|error| persistence_error(operation, path, error))
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let index = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run"),
        std::process::id(),
        index
    ));
    let mut file = private_create(&temporary)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    File::open(parent)?.sync_all()
}

fn private_create(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn private_append(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn set_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn persistence_error(
    operation: &'static str,
    path: &Path,
    error: impl fmt::Display,
) -> RunPersistenceError {
    RunPersistenceError {
        operation,
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

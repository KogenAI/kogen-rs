//! Durable snapshots and journals for one Build run.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    #[serde(skip)]
    pub fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LandingRecord {
    pub approval_commit: String,
    pub run_id: String,
    pub expected_parent: String,
    pub final_tree: String,
    pub candidate_commit: String,
    #[serde(skip)]
    pub fields: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::{RunEvent, RunSnapshot, RunStore};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn run_snapshot_serializes_only_the_specified_fields() {
        let snapshot = RunSnapshot {
            schema: 2,
            run_id: "a".repeat(32),
            slug: "greet".to_owned(),
            approval_sha256: "b".repeat(64),
            approval_commit: "c".repeat(40),
            target_branch: "main".to_owned(),
            status: "running".to_owned(),
            landing: None,
            owner_pid: 7,
            owner_started_ms: 1,
            started_ms: 2,
            fields: BTreeMap::from([("verdict".to_owned(), json!("green"))]),
        };

        let value = serde_json::to_value(&snapshot).expect("serialize run snapshot");
        let keys = value
            .as_object()
            .expect("snapshot object")
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            [
                "schema",
                "run_id",
                "slug",
                "approval_sha256",
                "approval_commit",
                "target_branch",
                "status",
                "landing",
                "owner_pid",
                "owner_started_ms",
                "started_ms",
            ]
            .into_iter()
            .collect()
        );

        let directory = std::env::temp_dir().join(format!(
            "kogen-run-snapshot-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let store = RunStore::new(&directory);
        store.create(&snapshot).expect("persist snapshot");
        let loaded: Value = serde_json::from_slice(
            &std::fs::read(directory.join("run.json")).expect("read run.json"),
        )
        .expect("parse run.json");
        assert_eq!(loaded, value);
        std::fs::remove_dir_all(directory).expect("remove snapshot fixture");
    }

    #[cfg(unix)]
    #[test]
    fn run_store_rejects_symlinked_journal_and_snapshot_outputs() {
        use std::os::unix::fs::symlink;

        let directory = std::env::temp_dir().join(format!(
            "kogen-run-store-symlink-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let outside = directory.with_extension("outside");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&outside, b"untouched\n").unwrap();
        let snapshot = RunSnapshot {
            schema: 2,
            run_id: "a".repeat(32),
            slug: "greet".to_owned(),
            approval_sha256: "b".repeat(64),
            approval_commit: "c".repeat(40),
            target_branch: "main".to_owned(),
            status: "running".to_owned(),
            landing: None,
            owner_pid: 7,
            owner_started_ms: 1,
            started_ms: 2,
            fields: BTreeMap::new(),
        };
        let store = RunStore::new(&directory);
        store.create(&snapshot).unwrap();
        symlink(&outside, directory.join("events.jsonl")).unwrap();
        assert!(store.record(&RunEvent::new("test", 3), &snapshot).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched\n");

        std::fs::remove_file(directory.join("events.jsonl")).unwrap();
        std::fs::remove_file(directory.join("run.json")).unwrap();
        symlink(&outside, directory.join("run.json")).unwrap();
        assert!(store.write_snapshot(&snapshot).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched\n");

        let outside_directory = directory.with_extension("outside-dir");
        std::fs::create_dir_all(&outside_directory).unwrap();
        let linked_run = directory.with_extension("linked-run");
        symlink(&outside_directory, &linked_run).unwrap();
        assert!(RunStore::new(&linked_run).create(&snapshot).is_err());
        assert!(!outside_directory.join("run.json").exists());

        let _ = std::fs::remove_dir_all(&directory);
        let _ = std::fs::remove_file(&linked_run);
        let _ = std::fs::remove_dir_all(outside_directory);
        let _ = std::fs::remove_file(outside);
    }
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
    append_lock: Arc<Mutex<()>>,
}

impl RunStore {
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            append_lock: Arc::new(Mutex::new(())),
        }
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn create(&self, snapshot: &RunSnapshot) -> Result<(), RunPersistenceError> {
        crate::safe_fs::ensure_directory_path(&self.directory)
            .map_err(|error| persistence_error("create run directory", &self.directory, error))?;
        ensure_real_directory(&self.directory)
            .map_err(|error| persistence_error("validate run directory", &self.directory, error))?;
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
        let _guard = self
            .append_lock
            .lock()
            .map_err(|error| persistence_error("lock run journal", &path, error))?;
        ensure_real_directory(&self.directory)
            .map_err(|error| persistence_error("validate run directory", &self.directory, error))?;
        set_private_dir(&self.directory)
            .map_err(|error| persistence_error("protect run directory", &self.directory, error))?;
        if matches!(event.event.as_str(), "finished" | "reconciled") && snapshot.status != "running"
        {
            self.prepare_cleanup(snapshot)?;
        }
        let mut file = crate::safe_fs::append_file(&self.directory, Path::new("events.jsonl"))
            .map_err(|error| persistence_error("append run event", &path, error))?;
        serde_json::to_writer(&mut file, event)
            .map_err(|error| persistence_error("append run event", &path, error))?;
        file.write_all(b"\n")
            .and_then(|()| file.sync_all())
            .map_err(|error| persistence_error("append run event", &path, error))?;
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| persistence_error("sync run directory", &self.directory, error))?;
        drop(_guard);
        self.write_snapshot(snapshot)
    }

    pub fn append_transcript(&self, row: &Value) -> Result<(), RunPersistenceError> {
        let path = self.directory.join("transcript.jsonl");
        let _guard = self
            .append_lock
            .lock()
            .map_err(|error| persistence_error("lock run transcript", &path, error))?;
        ensure_real_directory(&self.directory)
            .map_err(|error| persistence_error("validate run directory", &self.directory, error))?;
        set_private_dir(&self.directory)
            .map_err(|error| persistence_error("protect run directory", &self.directory, error))?;
        let mut file = crate::safe_fs::append_file(&self.directory, Path::new("transcript.jsonl"))
            .map_err(|error| persistence_error("append run transcript", &path, error))?;
        serde_json::to_writer(&mut file, row)
            .map_err(|error| persistence_error("append run transcript", &path, error))?;
        file.write_all(b"\n")
            .and_then(|()| file.sync_all())
            .map_err(|error| persistence_error("append run transcript", &path, error))
    }

    pub fn write_snapshot(&self, snapshot: &RunSnapshot) -> Result<(), RunPersistenceError> {
        let path = self.directory.join("run.json");
        let bytes = serde_json::to_vec(snapshot)
            .map_err(|error| persistence_error("encode run snapshot", &path, error))?;
        crate::safe_fs::atomic_replace(&self.directory, Path::new("run.json"), &bytes)
            .map_err(|error| persistence_error("publish run snapshot", &path, error))
    }

    /// Durably records that the run's claim, incoming ref, and workspaces must
    /// be cleaned. Recovery removes this marker only after every operation is
    /// complete, so a crash at any later point leaves a retryable obligation.
    pub fn prepare_cleanup(&self, snapshot: &RunSnapshot) -> Result<(), RunPersistenceError> {
        let path = self.directory.join("cleanup.json");
        let bytes = serde_json::to_vec(&serde_json::json!({ "run_id": snapshot.run_id }))
            .map_err(|error| persistence_error("encode cleanup obligation", &path, error))?;
        crate::safe_fs::atomic_replace(&self.directory, Path::new("cleanup.json"), &bytes)
            .map_err(|error| persistence_error("publish cleanup obligation", &path, error))
    }

    pub fn cleanup_pending(&self, run_id: &str) -> Result<bool, RunPersistenceError> {
        let path = self.directory.join("cleanup.json");
        match crate::safe_fs::read_file(&self.directory, Path::new("cleanup.json")) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                    persistence_error("decode cleanup obligation", &path, error)
                })?;
                match value.get("run_id").and_then(Value::as_str) {
                    Some(obligation_run_id) if obligation_run_id == run_id => Ok(true),
                    _ => Err(persistence_error(
                        "validate cleanup obligation",
                        &path,
                        "run id does not match its directory",
                    )),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(persistence_error("read cleanup obligation", &path, error)),
        }
    }

    pub fn clear_cleanup(&self) -> Result<(), RunPersistenceError> {
        crate::safe_fs::remove_file(&self.directory, Path::new("cleanup.json")).map_err(|error| {
            persistence_error(
                "clear cleanup obligation",
                &self.directory.join("cleanup.json"),
                error,
            )
        })
    }

    pub fn read_snapshot(&self) -> Result<RunSnapshot, RunPersistenceError> {
        let path = Path::new("run.json");
        let bytes = crate::safe_fs::read_file(&self.directory, path).map_err(|error| {
            persistence_error("read run snapshot", &self.directory.join(path), error)
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            persistence_error("decode run snapshot", &self.directory.join(path), error)
        })
    }

    pub fn read_events(&self) -> Result<Vec<RunEvent>, RunPersistenceError> {
        let path = self.directory.join("events.jsonl");
        let bytes = crate::safe_fs::read_file(&self.directory, Path::new("events.jsonl"))
            .map_err(|error| persistence_error("read run journal", &path, error))?;
        let file = std::io::Cursor::new(bytes);
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

fn ensure_real_directory(path: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "run directory is not a real directory",
        ));
    }
    Ok(())
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

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
mod fs_ops;
use fs_ops::{
    copy_entry, create_parents, invalid_path, outputs_exist as outputs_exist_impl, relative_join,
    remove_path, sync_directory, sync_tree, workspace_fingerprint as workspace_fingerprint_impl,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
const MAX_ENTRIES: usize = 3;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Complete {
    pub(super) v: u8,
    pub(super) key: String,
    pub(super) setup_wall_ms: u64,
    pub(super) outputs: Vec<String>,
    pub(super) last_used_ms: u64,
}

#[derive(Clone, Debug)]
pub(super) struct CacheEntry {
    pub(super) directory: PathBuf,
    pub(super) complete: Complete,
}

pub(super) fn read_entry(
    cache_root: &Path,
    key: &str,
    outputs: &[String],
) -> io::Result<Option<CacheEntry>> {
    let directory = cache_root.join(key);
    let bytes = match fs::read(directory.join("complete")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Ok(complete) = serde_json::from_slice::<Complete>(&bytes) else {
        return Ok(None);
    };
    if complete.v != 2 || complete.key != key || complete.outputs != outputs {
        return Ok(None);
    }
    for output in outputs {
        let Some(path) = relative_join(&directory.join("outputs"), output) else {
            return Ok(None);
        };
        if fs::symlink_metadata(path).is_err() {
            return Ok(None);
        }
    }
    Ok(Some(CacheEntry {
        directory,
        complete,
    }))
}

pub(super) fn restore(
    checkout: &Path,
    cache_root: &Path,
    key: &str,
    outputs: &[String],
) -> io::Result<()> {
    let entry = read_entry(cache_root, key, outputs)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cache entry is incomplete"))?;
    let checkout = fs::canonicalize(checkout)?;
    for output in outputs {
        let source = relative_join(&entry.directory.join("outputs"), output)
            .ok_or_else(|| invalid_path(output))?;
        let destination = relative_join(&checkout, output).ok_or_else(|| invalid_path(output))?;
        create_parents(&checkout, output)?;
        remove_path(&destination)?;
        copy_entry(&source, &destination)?;
    }
    Ok(())
}

pub(super) fn publish(
    cache_root: &Path,
    checkout: &Path,
    key: &str,
    outputs: &[String],
    setup_wall_ms: u64,
) -> io::Result<()> {
    ensure_private_dir(cache_root)?;
    let checkout = fs::canonicalize(checkout)?;
    let directory = cache_root.join(key);
    let temporary = cache_root.join(format!(".entry-{}-{}", std::process::id(), next_id()));
    fs::create_dir(&temporary)?;
    set_private_dir(&temporary)?;
    let result = (|| {
        let output_root = temporary.join("outputs");
        fs::create_dir(&output_root)?;
        for output in outputs {
            let source = relative_join(&checkout, output).ok_or_else(|| invalid_path(output))?;
            let destination =
                relative_join(&output_root, output).ok_or_else(|| invalid_path(output))?;
            create_parents(&output_root, output)?;
            copy_entry(&source, &destination)?;
        }
        let complete = Complete {
            v: 2,
            key: key.to_owned(),
            setup_wall_ms,
            outputs: outputs.to_vec(),
            last_used_ms: next_last_used(cache_root),
        };
        write_complete(&temporary, &complete)?;
        sync_tree(&temporary)?;
        sync_directory(&temporary)?;
        match fs::rename(&temporary, &directory) {
            Ok(()) => {}
            Err(error) if directory.join("complete").is_file() => {
                remove_path(&temporary)?;
                let _ = error;
            }
            Err(error) => return Err(error),
        }
        sync_directory(cache_root)?;
        prune(cache_root, key)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_path(&temporary);
    }
    result
}

pub(super) fn touch(cache_root: &Path, entry: &CacheEntry) -> io::Result<()> {
    let mut complete = entry.complete.clone();
    complete.last_used_ms = next_last_used(cache_root);
    write_complete(&entry.directory, &complete)?;
    sync_directory(&entry.directory)?;
    prune(cache_root, &complete.key)
}

pub(super) fn entry_count(cache_root: &Path) -> usize {
    valid_entries(cache_root).len()
}

pub(super) fn outputs_exist(checkout: &Path, outputs: &[String]) -> bool {
    outputs_exist_impl(checkout, outputs)
}

pub(super) fn workspace_fingerprint(
    checkout: &Path,
    outputs: &[String],
) -> io::Result<std::collections::BTreeMap<PathBuf, (u32, String)>> {
    workspace_fingerprint_impl(checkout, outputs)
}

fn valid_entries(cache_root: &Path) -> Vec<CacheEntry> {
    let Ok(entries) = fs::read_dir(cache_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let key = entry.file_name().to_str()?.to_owned();
            if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            let complete =
                serde_json::from_slice::<Complete>(&fs::read(entry.path().join("complete")).ok()?)
                    .ok()?;
            (complete.v == 2 && complete.key == key).then_some(CacheEntry {
                directory: entry.path(),
                complete,
            })
        })
        .collect()
}

fn next_last_used(cache_root: &Path) -> u64 {
    let previous = valid_entries(cache_root)
        .iter()
        .map(|entry| entry.complete.last_used_ms)
        .max()
        .unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(now)
        .unwrap_or(u64::MAX)
        .max(previous.saturating_add(1))
}

fn prune(cache_root: &Path, keep_key: &str) -> io::Result<()> {
    let mut entries = valid_entries(cache_root);
    entries.sort_by(|left, right| {
        left.complete
            .last_used_ms
            .cmp(&right.complete.last_used_ms)
            .then_with(|| left.complete.key.cmp(&right.complete.key))
    });
    let remove_count = entries.len().saturating_sub(MAX_ENTRIES);
    for entry in entries.into_iter().take(remove_count) {
        if entry.complete.key != keep_key {
            remove_path(&entry.directory)?;
        }
    }
    Ok(())
}

fn write_complete(directory: &Path, complete: &Complete) -> io::Result<()> {
    let bytes = serde_json::to_vec(complete)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = directory.join(format!(
        ".complete-{}-{}.tmp",
        std::process::id(),
        next_id()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join("complete"))?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    set_private_dir(path)
}

fn set_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn next_id() -> u64 {
    NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
}

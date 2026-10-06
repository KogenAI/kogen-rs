use super::super::model::BaselineCache;
use super::CheckError;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(super) fn read_cache(path: &Path, key: &str) -> Option<BaselineCache> {
    let bytes = fs::read(path).ok()?;
    let cache: BaselineCache = serde_json::from_slice(&bytes).ok()?;
    (cache.key == key).then_some(cache)
}

pub(super) fn write_cache(path: &Path, cache: &BaselineCache) -> Result<(), CheckError> {
    let parent = path
        .parent()
        .ok_or_else(|| CheckError::Internal("approval cache has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(internal)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(internal)?;
    }
    let bytes =
        serde_json::to_vec(cache).map_err(|error| CheckError::Internal(error.to_string()))?;
    let temporary = parent.join(format!(
        ".approval-cache-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary).map_err(internal)?;
        file.write_all(&bytes).map_err(internal)?;
        file.sync_all().map_err(internal)?;
        fs::rename(&temporary, path).map_err(internal)?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(internal)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn internal(error: std::io::Error) -> CheckError {
    CheckError::Internal(error.to_string())
}

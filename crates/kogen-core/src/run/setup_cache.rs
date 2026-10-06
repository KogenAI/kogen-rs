//! Canonical setup identity and isolated, file-backed setup reuse.

mod disk;
mod key;

#[cfg(test)]
mod tests;

pub use key::{SetupCacheKey, SetupInput, SetupKeyError, SetupKeyInput};

use disk::{entry_count, publish, read_entry, restore, touch};
use std::path::Path;

/// Request for a setup operation whose declared products may be reused.
pub struct SetupCacheRequest<'a> {
    pub checkout: &'a Path,
    pub cache_root: &'a Path,
    pub key: &'a str,
    pub outputs: Vec<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SetupCacheOutcome {
    pub reused: bool,
    /// Wall time of the setup invocation stored in the cache entry.
    pub saved_wall_ms: u64,
    pub published: bool,
}

/// Restore a hit or run setup and atomically publish its stable declared outputs.
/// Cache I/O is best-effort: an unavailable or corrupt optional cache falls back
/// to the supplied setup operation. A failed setup can never publish an entry.
pub fn run_setup<E>(
    request: SetupCacheRequest<'_>,
    setup: impl FnOnce() -> Result<u64, E>,
) -> Result<SetupCacheOutcome, E> {
    if !request.enabled || request.outputs.is_empty() || !valid_key(request.key) {
        return setup().map(|saved_wall_ms| SetupCacheOutcome {
            saved_wall_ms,
            ..SetupCacheOutcome::default()
        });
    }

    if let Ok(Some(entry)) = read_entry(request.cache_root, request.key, &request.outputs)
        && restore(
            request.checkout,
            request.cache_root,
            request.key,
            &request.outputs,
        )
        .is_ok()
    {
        let _ = touch(request.cache_root, &entry);
        return Ok(SetupCacheOutcome {
            reused: true,
            saved_wall_ms: entry.complete.setup_wall_ms,
            published: false,
        });
    }

    let before = disk::workspace_fingerprint(request.checkout, &request.outputs).ok();
    let setup_wall_ms = setup()?;
    if let Some(before) = before
        && let Ok(after) = disk::workspace_fingerprint(request.checkout, &request.outputs)
        && before == after
        && disk::outputs_exist(request.checkout, &request.outputs)
        && publish(
            request.cache_root,
            request.checkout,
            request.key,
            &request.outputs,
            setup_wall_ms,
        )
        .is_ok()
    {
        return Ok(SetupCacheOutcome {
            reused: false,
            saved_wall_ms: setup_wall_ms,
            published: true,
        });
    }

    Ok(SetupCacheOutcome {
        reused: false,
        saved_wall_ms: setup_wall_ms,
        published: false,
    })
}

/// Number of valid, complete cache entries under `cache_root`.
#[must_use]
pub fn cached_entry_count(cache_root: &Path) -> usize {
    entry_count(cache_root)
}

fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
}

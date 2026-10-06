//! Cross-process directory lock for ChatGPT refreshes.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::RngCore as _;

use super::super::super::{environment_error, provider_error};

const STALE_MS: u128 = 60_000;
const WAIT_MS: u128 = 90_000;
const POLL_MS: u64 = 25;

pub(super) struct RefreshLock {
    path: PathBuf,
    token: String,
}

impl RefreshLock {
    pub(super) fn acquire(home: &Path, label: &str) -> Result<Self, crate::error::CoreError> {
        Self::acquire_for(home, "chatgpt", label)
    }

    pub(super) fn acquire_for(
        home: &Path,
        provider: &str,
        label: &str,
    ) -> Result<Self, crate::error::CoreError> {
        let parent = home.join(".kogen/locks");
        fs::create_dir_all(&parent).map_err(|_| {
            environment_error(
                "refresh_lock_failed",
                "could not create credential lock directory",
            )
        })?;
        set_private_dir(&parent).map_err(|_| {
            environment_error(
                "refresh_lock_failed",
                "could not secure credential lock directory",
            )
        })?;
        let path = parent.join(format!("{provider}-{label}.lock"));
        let wait = scaled(WAIT_MS);
        let stale = scaled(STALE_MS);
        let started = std::time::Instant::now();

        loop {
            match fs::create_dir(&path) {
                Ok(()) => {
                    set_private_dir(&path).map_err(|_| {
                        let _ = fs::remove_dir_all(&path);
                        environment_error("refresh_lock_failed", "could not secure credential lock")
                    })?;
                    let token = random_token();
                    let owner = path.join("owner");
                    let mut options = OpenOptions::new();
                    options.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt as _;
                        options.mode(0o600);
                    }
                    let mut file = options.open(&owner).map_err(|_| {
                        let _ = fs::remove_dir_all(&path);
                        environment_error(
                            "refresh_lock_failed",
                            "could not publish credential lock",
                        )
                    })?;
                    if writeln!(file, "{} {} {token}", std::process::id(), now_ms()).is_err()
                        || file.sync_all().is_err()
                    {
                        let _ = fs::remove_dir_all(&path);
                        return Err(environment_error(
                            "refresh_lock_failed",
                            "could not publish credential lock",
                        ));
                    }
                    return Ok(Self { path, token });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(&path, stale) {
                        let _ = fs::remove_dir_all(&path);
                        continue;
                    }
                    if started.elapsed() >= wait {
                        let message = if provider == "grok" {
                            "Grok session refresh timed out."
                        } else {
                            "timed out waiting for ChatGPT token refresh"
                        };
                        return Err(provider_error("login", message));
                    }
                    thread::sleep(Duration::from_millis(POLL_MS));
                }
                Err(_) => {
                    return Err(environment_error(
                        "refresh_lock_failed",
                        "could not acquire credential lock",
                    ));
                }
            }
        }
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        let owner = self.path.join("owner");
        let contents = fs::read_to_string(&owner).unwrap_or_default();
        if contents.split_whitespace().nth(2) == Some(self.token.as_str()) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn is_stale(path: &Path, stale_after: Duration) -> bool {
    let owner = path.join("owner");
    if let Ok(text) = fs::read_to_string(&owner)
        && let Some(timestamp) = text
            .split_whitespace()
            .nth(1)
            .and_then(|part| part.parse::<u128>().ok())
    {
        return now_ms().saturating_sub(timestamp) >= stale_after.as_millis();
    }
    // Empty owner data can be observed while its creator still owns the
    // directory; use the directory timestamp rather than treating it stale.
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= stale_after)
}

fn scaled(milliseconds: u128) -> Duration {
    let scale = std::env::var("KOGEN_TIME_SCALE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(1.0);
    let millis = (milliseconds as f64 * scale).floor().max(1.0) as u64;
    Duration::from_millis(millis)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn random_token() -> String {
    let mut bytes = [0_u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn set_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

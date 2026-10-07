use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

const WAIT_LIMIT: Duration = Duration::from_secs(120);
const STALE_CHECK_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) struct CheckoutLock {
    path: PathBuf,
    owner: String,
}

impl CheckoutLock {
    pub(crate) fn acquire(state_root: &Path, checkout: &Path) -> Result<Self, CoreError> {
        let checkout = fs::canonicalize(checkout).map_err(lock_error)?;
        let digest = format!(
            "{:x}",
            Sha256::digest(checkout.as_os_str().as_encoded_bytes())
        );
        let directory = state_root.join("locks");
        fs::create_dir_all(&directory).map_err(lock_error)?;
        let path = directory.join(format!("checkout-{digest}"));
        let owner = format!("pid-{}", std::process::id());
        let started = Instant::now();
        let mut last_stale_check = Instant::now();

        loop {
            match create_lock(&path, &owner) {
                Ok(()) => return Ok(Self { path, owner }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if last_stale_check.elapsed() >= STALE_CHECK_INTERVAL {
                        remove_stale_lock(&path);
                        last_stale_check = Instant::now();
                    }
                    if started.elapsed() >= WAIT_LIMIT {
                        return Err(lock_error("another command holds the project lock"));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(lock_error(error)),
            }
        }
    }
}

impl Drop for CheckoutLock {
    fn drop(&mut self) {
        if lock_owner(&self.path).as_deref() == Some(self.owner.as_str()) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn create_lock(path: &Path, owner: &str) -> std::io::Result<()> {
    std::os::unix::fs::symlink(owner, path)
}

#[cfg(not(unix))]
fn create_lock(path: &Path, owner: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(owner.as_bytes())
}

#[cfg(unix)]
fn lock_owner(path: &Path) -> Option<String> {
    fs::read_link(path)
        .ok()
        .and_then(|target| target.to_str().map(str::to_owned))
}

#[cfg(not(unix))]
fn lock_owner(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn remove_stale_lock(path: &Path) {
    let Some(owner) = lock_owner(path) else {
        return;
    };
    let Some(pid) = owner
        .strip_prefix("pid-")
        .and_then(|value| value.parse::<u32>().ok())
    else {
        return;
    };
    let alive = Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success());
    if !alive && lock_owner(path).as_deref() == Some(owner.as_str()) {
        let _ = fs::remove_file(path);
    }
}

fn lock_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        "workspace_lock_failed",
        error.to_string(),
        ExitCode::Environment,
    )
}

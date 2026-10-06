//! Per-checkout queue.pid ownership and stop-file operations.
use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub enum QueuePidStart {
    Acquired(QueuePidLock),
    Running(u32),
}

#[derive(Debug, Eq, PartialEq)]
pub enum QueueStopState {
    Requested(u32),
    NotRunning,
}

#[derive(Debug)]
pub struct QueuePidLock {
    path: PathBuf,
    stop_path: PathBuf,
    pid: u32,
    owned: bool,
}

impl QueuePidLock {
    /// Acquire queue.pid exclusively, taking over a dead owner's file at most twice.
    pub fn acquire(state_root: impl AsRef<Path>) -> Result<QueuePidStart, CoreError> {
        let root = state_root.as_ref();
        fs::create_dir_all(root).map_err(|error| lock_error("queue_lock_failed", error))?;
        let path = root.join("queue.pid");
        let stop_path = root.join("queue.stop");
        let pid = std::process::id();
        for _ in 0..2 {
            match create_owner_file(&path, pid) {
                Ok(()) => {
                    let _ = fs::remove_file(&stop_path);
                    return Ok(QueuePidStart::Acquired(Self {
                        path,
                        stop_path,
                        pid,
                        owned: true,
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let owner = read_owner_pid(&path)
                        .map_err(|error| lock_error("queue_lock_unavailable", error))?;
                    if let Some(owner) = owner
                        && process_is_alive(owner)?
                    {
                        return Ok(QueuePidStart::Running(owner));
                    }
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(lock_error("queue_lock_unavailable", error));
                        }
                    }
                }
                Err(error) => return Err(lock_error("queue_lock_failed", error)),
            }
        }
        Err(CoreError::new(
            ErrorClass::Environment,
            "queue_lock_unavailable",
            "could not take over a stale queue lock after two attempts",
            ExitCode::Environment,
        ))
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Check the stop marker between Builds, after the current Build has finished.
    pub fn stop_requested(&self) -> Result<bool, CoreError> {
        self.stop_path
            .try_exists()
            .map_err(|error| lock_error("queue_stop_read_failed", error))
    }

    /// Ask the live owner to stop after its current Build.
    pub fn request_stop(state_root: impl AsRef<Path>) -> Result<QueueStopState, CoreError> {
        let path = state_root.as_ref().join("queue.pid");
        let stop_path = state_root.as_ref().join("queue.stop");
        for _ in 0..2 {
            let Some(owner) = read_owner_pid(&path)
                .map_err(|error| lock_error("queue_lock_unavailable", error))?
            else {
                return Ok(QueueStopState::NotRunning);
            };
            if process_is_alive(owner)? {
                let mut file = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(&stop_path)
                    .map_err(|error| lock_error("queue_stop_write_failed", error))?;
                file.write_all(b"stop\n")
                    .and_then(|()| file.sync_all())
                    .map_err(|error| lock_error("queue_stop_write_failed", error))?;
                return Ok(QueueStopState::Requested(owner));
            }
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(lock_error("queue_lock_unavailable", error)),
            }
        }
        Ok(QueueStopState::NotRunning)
    }

    pub fn release(mut self) {
        self.release_if_owner();
    }

    fn release_if_owner(&mut self) {
        if !self.owned {
            return;
        }
        if read_owner_pid(&self.path).ok().flatten() == Some(self.pid) {
            let _ = fs::remove_file(&self.path);
        }
        self.owned = false;
    }
}

impl Drop for QueuePidLock {
    fn drop(&mut self) {
        self.release_if_owner();
    }
}

fn create_owner_file(path: &Path, pid: u32) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if let Err(error) = write_owner(&mut file, pid) {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

fn write_owner(file: &mut std::fs::File, pid: u32) -> std::io::Result<()> {
    writeln!(file, "{pid}")?;
    file.sync_all()
}

fn read_owner_pid(path: &Path) -> std::io::Result<Option<u32>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    Ok(text.trim().parse::<u32>().ok().filter(|pid| *pid > 0))
}

fn process_is_alive(pid: u32) -> Result<bool, CoreError> {
    if pid == 0 {
        return Ok(false);
    }
    let output = Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| lock_error("queue_owner_probe_failed", error))?;
    if output.status.success() {
        return Ok(true);
    }
    let detail = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if detail.contains("no such process")
        || detail.contains("not found")
        || detail.contains("illegal process id")
    {
        return Ok(false);
    }
    if detail.contains("operation not permitted") || detail.contains("permission denied") {
        return Ok(true);
    }
    Err(CoreError::new(
        ErrorClass::Environment,
        "queue_owner_probe_failed",
        "could not determine whether the queue owner is live",
        ExitCode::Environment,
    ))
}

fn lock_error(reason: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        error.to_string(),
        ExitCode::Environment,
    )
}

#[cfg(test)]
mod tests {
    use super::{QueuePidLock, QueuePidStart, QueueStopState};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("kogen-queue-lock-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create unique temporary state root");
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_live_pid_holds_queue_start_and_receives_stop_after_current() {
        let root = TempRoot::new();
        let QueuePidStart::Acquired(owner) = QueuePidLock::acquire(&root.0).unwrap() else {
            panic!("first start must acquire the queue pid");
        };

        assert!(matches!(
            QueuePidLock::acquire(&root.0).unwrap(),
            QueuePidStart::Running(pid) if pid == std::process::id()
        ));
        assert_eq!(
            QueuePidLock::request_stop(&root.0).unwrap(),
            QueueStopState::Requested(std::process::id())
        );
        assert!(owner.stop_requested().unwrap());
        assert_eq!(fs::read(root.0.join("queue.stop")).unwrap(), b"stop\n");
    }

    #[test]
    fn a_dead_pid_is_taken_over_and_only_the_new_owner_releases_it() {
        let root = TempRoot::new();
        fs::write(root.0.join("queue.pid"), b"4294967295\n").unwrap();

        let QueuePidStart::Acquired(owner) = QueuePidLock::acquire(&root.0).unwrap() else {
            panic!("a dead pid must be taken over");
        };
        assert_eq!(
            fs::read_to_string(root.0.join("queue.pid")).unwrap(),
            format!("{}\n", std::process::id())
        );
        drop(owner);
        assert!(!root.0.join("queue.pid").exists());
    }
}

use super::process::{ProcessError, ensure_private_dir};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const TERM_GRACE: Duration = Duration::from_millis(200);

#[cfg(unix)]
pub(super) struct ParentWatcher {
    child: Child,
    script_path: PathBuf,
    writer_path: PathBuf,
}

#[cfg(unix)]
pub(super) fn start_parent_watcher(
    run_dir: &Path,
    target_group: u32,
) -> Result<ParentWatcher, ProcessError> {
    let script = b"#!/bin/sh\nowner=$1\ntarget=$2\nregistration=$3\nwhile /bin/kill -0 \"$owner\" 2>/dev/null; do /bin/sleep 0.1; done\n/usr/bin/pkill -TERM -g \"$target\" 2>/dev/null || :\n/bin/sleep 0.2\n/usr/bin/pkill -KILL -g \"$target\" 2>/dev/null || :\n/bin/rm -f \"$registration\"\n";
    let tmp_dir = ensure_private_dir(&run_dir.join("tmp"))?;
    let script_path = super::script::create_private_file(&tmp_dir, "parent-watch", ".sh", script)
        .map_err(|source| ProcessError::Io {
        operation: "write parent-death watcher",
        source,
    })?;
    let script_len = script_path.as_os_str().as_bytes().len();
    if script_len > 4096 {
        let _ = fs::remove_file(&script_path);
        return Err(ProcessError::ArgumentTooLong {
            index: 1,
            bytes: script_len,
        });
    }
    let group_started_ms =
        crate::recovery::process_started_ms(target_group).ok_or_else(|| ProcessError::Io {
            operation: "identify owned writer",
            source: io::Error::other("process identity unavailable"),
        })?;
    let writer_path = tmp_dir.join(format!(
        "writer-{target_group}-{}.json",
        rand::random::<u64>()
    ));
    let child = Command::new("/bin/sh")
        .arg(&script_path)
        .arg(std::process::id().to_string())
        .arg(target_group.to_string())
        .arg(&writer_path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|source| {
            let _ = fs::remove_file(&script_path);
            ProcessError::Io {
                operation: "start parent-death watcher",
                source,
            }
        })?;
    let registration = serde_json::json!({"group":target_group,"started_ms":group_started_ms,
        "watcher":child.id(),"watcher_started_ms":crate::recovery::process_started_ms(child.id())});
    if let Err(source) = crate::safe_fs::atomic_replace(
        &tmp_dir,
        Path::new(writer_path.file_name().expect("writer filename")),
        &serde_json::to_vec(&registration).expect("writer identity"),
    ) {
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(script_path);
        return Err(ProcessError::Io {
            operation: "record owned writer",
            source,
        });
    }
    Ok(ParentWatcher {
        child,
        script_path,
        writer_path,
    })
}

#[cfg(unix)]
pub(super) fn stop_watcher(mut watcher: ParentWatcher) {
    let _ = signal_group("KILL", watcher.child.id());
    let _ = watcher.child.kill();
    let _ = watcher.child.wait();
    let _ = fs::remove_file(watcher.script_path);
    let _ = fs::remove_file(watcher.writer_path);
}

#[cfg(unix)]
pub(super) fn stop_group(group: u32, force_grace: bool) {
    let Ok(group) = i32::try_from(group) else {
        return;
    };
    let Some(group) = rustix::process::Pid::from_raw(group) else {
        return;
    };
    if group.as_raw_pid() <= 1 || (!force_grace && !group_exists(group)) {
        return;
    }
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
    let deadline = std::time::Instant::now() + TERM_GRACE;
    while group_exists(group) && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if group_exists(group) {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
}

#[cfg(unix)]
fn group_exists(group: rustix::process::Pid) -> bool {
    rustix::process::test_kill_process_group(group).is_ok()
}

#[cfg(unix)]
fn signal_group(signal: &str, group: u32) -> io::Result<()> {
    let Ok(group) = i32::try_from(group) else {
        return Ok(());
    };
    let Some(group) = rustix::process::Pid::from_raw(group) else {
        return Ok(());
    };
    let signal = match signal {
        "TERM" => rustix::process::Signal::TERM,
        "KILL" => rustix::process::Signal::KILL,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported signal",
            ));
        }
    };
    rustix::process::kill_process_group(group, signal).map_err(io::Error::from)
}

/// Freeze registered writers before recovery captures or destroys any workspace.
/// Registrations are private, and a start identity protects against PID reuse.
pub(crate) fn stop_recovery_writers(directory: &Path) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() && !kind.is_symlink() {
            stop_recovery_writers(&entry.path())?;
        } else if kind.is_file() && entry.file_name().to_string_lossy().starts_with("writer-") {
            let registration: serde_json::Value = serde_json::from_slice(&fs::read(entry.path())?)?;
            let group = registration["group"]
                .as_u64()
                .and_then(|group| u32::try_from(group).ok())
                .ok_or_else(|| io::Error::other("invalid writer registration"))?;
            let expected = registration["started_ms"]
                .as_i64()
                .ok_or_else(|| io::Error::other("missing writer identity"))?;
            if crate::recovery::process_started_ms(group) == Some(expected) {
                stop_group(group, true);
            } else {
                // A dead leader may still have descendants. Its original watcher
                // must finish custody before we capture; never signal a reused PID.
                let watcher = registration["watcher"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok());
                let started = registration["watcher_started_ms"].as_i64();
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while watcher.zip(started).is_some_and(|(pid, started)| {
                    crate::recovery::process_started_ms(pid) == Some(started)
                }) && entry.path().exists()
                    && std::time::Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(20));
                }
            }
            let pid = i32::try_from(group)
                .ok()
                .and_then(rustix::process::Pid::from_raw);
            if pid.is_some_and(group_exists) {
                return Err(io::Error::other("owned writer group has not stopped"));
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

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
}

#[cfg(unix)]
pub(super) fn start_parent_watcher(
    run_dir: &Path,
    target_group: u32,
) -> Result<ParentWatcher, ProcessError> {
    let script = b"#!/bin/sh\nowner=$1\ntarget=$2\nwhile /bin/kill -0 \"$owner\" 2>/dev/null; do /bin/sleep 0.1; done\n/usr/bin/pkill -TERM -g \"$target\" 2>/dev/null || :\n/bin/sleep 0.2\n/usr/bin/pkill -KILL -g \"$target\" 2>/dev/null || :\n";
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
    let child = Command::new("/bin/sh")
        .arg(&script_path)
        .arg(std::process::id().to_string())
        .arg(target_group.to_string())
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
    Ok(ParentWatcher { child, script_path })
}

#[cfg(unix)]
pub(super) fn stop_watcher(mut watcher: ParentWatcher) {
    let _ = signal_group("KILL", watcher.child.id());
    let _ = watcher.child.kill();
    let _ = watcher.child.wait();
    let _ = fs::remove_file(watcher.script_path);
}

#[cfg(unix)]
pub(super) fn stop_group(group: u32, force_grace: bool) {
    if group <= 1 || (!force_grace && !group_exists(group)) {
        return;
    }
    let _ = signal_group("TERM", group);
    thread::sleep(TERM_GRACE);
    let _ = signal_group("KILL", group);
}

#[cfg(unix)]
fn group_exists(group: u32) -> bool {
    Command::new("/usr/bin/pkill")
        .args(["-0", "-g", &group.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn signal_group(signal: &str, group: u32) -> io::Result<()> {
    let _ = Command::new("/usr/bin/pkill")
        .args([format!("-{signal}"), "-g".to_owned(), group.to_string()])
        .status()?;
    Ok(())
}

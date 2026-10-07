//! Lossless tar fallback. Publish bytes and their manifest create-only and durably.
use super::*;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::io::Write as _;

pub(super) fn preserve(
    repo: &GitRepo,
    workspace: &Path,
    directory: &Path,
    base: &str,
    tree: &str,
    protected: &Value,
) -> Result<Value, CoreError> {
    let name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| recovery_error("archive_identity_failed", "workspace identity"))?;
    // A crash can leave archive bytes without a manifest. Retain that publication
    // and use a deterministic alternate slot; a complete slot is adopted below.
    let mut slot = 0;
    let (destination, manifest_path) = loop {
        let suffix = if slot == 0 {
            String::new()
        } else {
            format!("-retry-{slot}")
        };
        let destination = directory.join(format!("recovery-{name}-{tree}{suffix}.tar"));
        let manifest = destination.with_extension("tar.json");
        if destination.exists() == manifest.exists() {
            break (destination, manifest);
        }
        slot += 1;
    };
    let mut captured = crate::gate::workspace::WorkspaceTree::capture_excluding(workspace, &[])
        .map_err(|error| recovery_error("archive_capture_failed", error))?;
    let paths = repo
        .output(&["diff", "--name-only", "--no-renames", "-z", base, tree])
        .map_err(|error| recovery_error("archive_manifest_failed", error))?;
    let mut files = serde_json::Map::new();
    let mut reviewed_files = serde_json::Map::new();
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path)
            .map_err(|error| recovery_error("archive_manifest_failed", error))?;
        let value = match captured.entries.get(Path::new(path)) {
            None => Value::Null,
            Some(entry) => {
                let mut value = json!({"mode":format!("{:o}",entry.mode)});
                match std::str::from_utf8(&entry.bytes) {
                    Ok(text) => value["text"] = text.into(),
                    Err(_) => {
                        value["bytes_base64"] = base64::engine::general_purpose::STANDARD
                            .encode(&entry.bytes)
                            .into()
                    }
                }
                value
            }
        };
        // Ordinary files default to 100644 in the neutral projection.
        let mut value = value;
        if value.get("mode").and_then(Value::as_str) == Some("100644") {
            value.as_object_mut().expect("file entry").remove("mode");
        }
        let reviewed = captured.entries.get(Path::new(path)).is_some_and(|entry| {
            protected
                .get(path)
                .and_then(Value::as_str)
                .is_some_and(|hash| hash == format!("{:x}", Sha256::digest(&entry.bytes)))
        });
        if reviewed {
            reviewed_files.insert(path.to_owned(), value);
        } else {
            files.insert(path.to_owned(), value);
        }
    }
    if destination.exists() && manifest_path.exists() {
        let bytes =
            fs::read(&destination).map_err(|error| recovery_error("archive_read_failed", error))?;
        let saved: Value = serde_json::from_slice(
            &fs::read(&manifest_path)
                .map_err(|error| recovery_error("archive_read_failed", error))?,
        )
        .map_err(|error| recovery_error("archive_read_failed", error))?;
        if saved["tree"] == tree
            && saved["base"] == base
            && saved["files"] == Value::Object(files.clone())
            && saved["reviewed_files"] == Value::Object(reviewed_files.clone())
            && saved["sha256"] == format!("{:x}", Sha256::digest(&bytes))
        {
            return Ok(
                json!({"path":destination,"manifest":manifest_path,"sha256":saved["sha256"]}),
            );
        }
        return Err(recovery_error(
            "archive_conflict",
            "existing archive is incomplete or differs; retained unchanged",
        ));
    }
    let staging = directory.join(format!(
        "recovery-{name}-{tree}-staging-{}",
        rand::random::<u64>()
    ));
    fs::create_dir(&staging).map_err(|error| recovery_error("archive_staging_failed", error))?;
    captured.root = staging.clone();
    let result = (|| {
        captured
            .restore()
            .map_err(|error| recovery_error("archive_staging_failed", error))?;
        // Tar reads the captured filesystem, so export-ignore/filter attributes cannot discard data.
        let output = Command::new("/usr/bin/tar")
            .args(["-cf", "-", "-C"])
            .arg(&staging)
            .arg(".")
            .env_clear()
            .output()
            .map_err(|error| recovery_error("archive_create_failed", error))?;
        if !output.status.success() {
            return Err(recovery_error(
                "archive_create_failed",
                String::from_utf8_lossy(&output.stderr),
            ));
        }
        let hash = format!("{:x}", Sha256::digest(&output.stdout));
        let manifest = json!({"schema":1,"base":base,"tree":tree,"sha256":hash,"files":files,"reviewed_files":reviewed_files});
        publish(&destination, &output.stdout)?;
        publish(
            &manifest_path,
            &serde_json::to_vec(&manifest).expect("archive manifest"),
        )?;
        Ok(json!({"path":destination,"manifest":manifest_path,"sha256":hash}))
    })();
    let _ = fs::remove_dir_all(staging);
    result
}
fn publish(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let temporary = path.with_extension(format!("partial-{}", rand::random::<u64>()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| recovery_error("archive_write_failed", error))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| recovery_error("archive_sync_failed", error))?;
        // A hard link atomically publishes without replacing a prior archive.
        fs::hard_link(&temporary, path)
            .map_err(|error| recovery_error("archive_publish_failed", error))?;
        fs::File::open(path.parent().expect("archive parent"))
            .and_then(|directory| directory.sync_all())
            .map_err(|error| recovery_error("archive_sync_failed", error))
    })();
    let _ = fs::remove_file(temporary);
    result
}

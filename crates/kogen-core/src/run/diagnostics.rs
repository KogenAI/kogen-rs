//! Private runner diagnostics. Never serialize environment values or command content.
use super::{OUTPUT_TAIL_BYTES, ProcessError, ProcessRequest, ProcessResult};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub(crate) fn prepare_private_run_dir(path: &Path) -> Result<(), ProcessError> {
    super::process::ensure_private_dir(path)?;
    for directory in ["logs", "tmp", "reports", "mise-state", "mise-cache"] {
        super::process::ensure_private_dir(&path.join(directory))?;
    }
    Ok(())
}

pub(super) fn record_process(
    request: &ProcessRequest,
    sandbox: Value,
    result: &Result<ProcessResult, ProcessError>,
) -> Result<(), ProcessError> {
    if result.as_ref().is_ok_and(|result| {
        result.exit_status == Some(0) && !result.timed_out && !result.unavailable
    }) && request.log_name != "sandbox-probe"
    {
        return Ok(());
    }
    let (exit_status, timed_out, unavailable, log_path, output_tail, error) = match result {
        Ok(result) => (
            result.exit_status,
            result.timed_out,
            result.unavailable,
            Some(result.log_path.clone()),
            result.output_tail.clone(),
            None,
        ),
        Err(error) => {
            let log = latest_log(request);
            let tail = log
                .as_ref()
                .and_then(|path| read_tail(path).ok())
                .unwrap_or_default();
            (None, false, false, log, tail, Some(error.to_string()))
        }
    };
    let output_tail = &output_tail[output_tail.len().saturating_sub(OUTPUT_TAIL_BYTES)..];
    let diagnostic = json!({
        "stage": request.log_name,
        "cwd": request.cwd,
        "program": request.program.to_string_lossy(),
        "argv_summary": request.args.iter().map(|arg| arg.len()).collect::<Vec<_>>(),
        "sandbox": sandbox,
        "exit_status": exit_status,
        "timed_out": timed_out,
        "unavailable": unavailable,
        "error": error.map(|text| redact(request, &text)),
        "log_path": log_path,
        // stdout and stderr share the supervisor's private log.
        "stderr_tail": redact(request, &String::from_utf8_lossy(output_tail)),
        "probe_output": if request.log_name == "sandbox-probe" {
            Some(redact(request, &String::from_utf8_lossy(output_tail)))
        } else { None },
    });
    write_diagnostic(&request.run_dir, &diagnostic)?;
    Ok(())
}

fn read_tail(path: &Path) -> std::io::Result<Vec<u8>> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "log has no filename")
    })?;
    let mut file = crate::safe_fs::open_read_file(directory, Path::new(name))?;
    let length = file.metadata()?.len().min(OUTPUT_TAIL_BYTES as u64);
    file.seek(SeekFrom::End(-(length as i64)))?;
    let mut tail = Vec::with_capacity(length as usize);
    file.read_to_end(&mut tail)?;
    Ok(tail)
}

fn redact(request: &ProcessRequest, text: &str) -> String {
    let mut text = text.to_owned();
    for (key, value) in &request.env {
        let key = key.to_string_lossy().to_ascii_uppercase();
        if ["TOKEN", "SECRET", "PASSWORD", "AUTH", "KEY", "PROXY"]
            .iter()
            .any(|marker| key.contains(marker))
        {
            let value = value.to_string_lossy();
            if !value.is_empty() {
                text = text.replace(value.as_ref(), "<redacted>");
            }
        }
    }
    text
}

fn latest_log(request: &ProcessRequest) -> Option<PathBuf> {
    let prefix = format!("{}-{}-", request.log_name, std::process::id());
    fs::read_dir(request.run_dir.join("logs"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .max_by_key(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        })
}

fn write_diagnostic(run_dir: &Path, diagnostic: &Value) -> Result<PathBuf, ProcessError> {
    let logs = super::process::ensure_private_dir(&run_dir.join("logs"))?;
    let name = format!(
        "runner-diagnostic-{}-{:016x}.json",
        std::process::id(),
        rand::random::<u64>()
    );
    let bytes = serde_json::to_vec(diagnostic).expect("diagnostic JSON");
    crate::safe_fs::create_file(&logs, Path::new(&name), &bytes).map_err(|source| {
        ProcessError::Io {
            operation: "write runner diagnostic",
            source,
        }
    })?;
    Ok(logs.join(name))
}

pub(crate) fn failure_detail(
    run_dir: &Path,
    reason: &str,
    detail: &str,
) -> Result<String, ProcessError> {
    let mut diagnostics = Vec::new();
    let mut roots = vec![run_dir.to_path_buf()];
    if let Ok(entries) = fs::read_dir(run_dir) {
        roots.extend(
            entries
                .flatten()
                .filter(|entry| {
                    entry.file_name().to_string_lossy().starts_with("gate-")
                        && entry.file_type().is_ok_and(|kind| kind.is_dir())
                })
                .map(|entry| entry.path()),
        );
    }
    for root in roots {
        if let Ok(entries) = fs::read_dir(root.join("logs")) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("runner-diagnostic-")
                    && let Ok(bytes) =
                        crate::safe_fs::read_file(&root, &Path::new("logs").join(entry.file_name()))
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                {
                    diagnostics.push(json!({"path": entry.path(), "diagnostic": value}));
                }
            }
        }
    }
    diagnostics.sort_by_key(|value| value["path"].as_str().unwrap_or_default().to_owned());
    let failure = json!({"reason": reason, "detail": detail, "runner_diagnostics": diagnostics});
    let path = write_diagnostic(run_dir, &failure)?;
    Ok(format!(
        "{detail}\nrunner failure log: {}\n{}",
        path.display(),
        failure
    ))
}

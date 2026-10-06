use super::{PreparedSandbox, SandboxPolicy};
use crate::run::{ProcessError, ProcessRequest, SandboxObservation, SandboxStatus};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;

pub(super) fn prepare(
    request: ProcessRequest,
    policy: &SandboxPolicy,
) -> Result<PreparedSandbox, ProcessError> {
    if !policy.enabled {
        return Ok(unconfined(request, SandboxStatus::Off, None));
    }
    if policy.already_sandboxed {
        return Ok(PreparedSandbox {
            request,
            observation: SandboxObservation {
                status: SandboxStatus::Confined,
                warning_reason: None,
            },
            cleanup_paths: Vec::new(),
        });
    }
    if let Some(reason) = &policy.forced_unavailable {
        return Ok(unconfined(
            request,
            SandboxStatus::Unconfined,
            Some(reason.clone()),
        ));
    }

    #[cfg(target_os = "macos")]
    {
        if Path::new("/usr/bin/sandbox-exec").is_file() {
            return prepare_macos(request, policy);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(bwrap) = find_program("bwrap") {
            return prepare_linux(request, policy, &bwrap);
        }
    }
    Ok(unconfined(
        request,
        SandboxStatus::Unconfined,
        Some("no supported confinement tool is available".to_owned()),
    ))
}

fn unconfined(
    request: ProcessRequest,
    status: SandboxStatus,
    warning_reason: Option<String>,
) -> PreparedSandbox {
    PreparedSandbox {
        request,
        observation: SandboxObservation {
            status,
            warning_reason,
        },
        cleanup_paths: Vec::new(),
    }
}

#[cfg(target_os = "macos")]
fn prepare_macos(
    mut request: ProcessRequest,
    policy: &SandboxPolicy,
) -> Result<PreparedSandbox, ProcessError> {
    let tmp = super::super::process::ensure_private_dir(&request.run_dir.join("tmp"))?;
    let contents = macos_profile(policy);
    let profile =
        super::super::script::create_private_file(&tmp, "sandbox", ".sb", contents.as_bytes())
            .map_err(|source| ProcessError::SandboxSetup(source.to_string()))?;
    let program = std::mem::replace(
        &mut request.program,
        OsString::from("/usr/bin/sandbox-exec"),
    );
    let args = std::mem::take(&mut request.args);
    request.args = vec![
        OsString::from("-f"),
        profile.as_os_str().to_owned(),
        program,
    ];
    request.args.extend(args);
    Ok(PreparedSandbox {
        request,
        observation: SandboxObservation {
            status: SandboxStatus::Confined,
            warning_reason: None,
        },
        cleanup_paths: vec![profile],
    })
}

#[cfg(target_os = "macos")]
fn macos_profile(policy: &SandboxPolicy) -> String {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process*)\n(allow sysctl-read)\n(allow mach-lookup)\n(allow network*)\n(allow file-read*)\n(allow file-write* (literal \"/dev/null\"))\n",
    );
    for path in normalized_paths(&policy.protected_paths) {
        if let Some(path) = path.to_str() {
            profile.push_str(&format!(
                "(deny file-read* (subpath {}))\n",
                sbpl_literal(path)
            ));
        }
    }
    for path in sandbox_writable_paths(policy) {
        if let Some(path) = path.to_str() {
            profile.push_str(&format!(
                "(allow file-write* (subpath {}))\n",
                sbpl_literal(path)
            ));
        }
    }
    profile
}

#[cfg(target_os = "macos")]
fn sbpl_literal(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(target_os = "linux")]
fn prepare_linux(
    mut request: ProcessRequest,
    policy: &SandboxPolicy,
    bwrap: &Path,
) -> Result<PreparedSandbox, ProcessError> {
    let mut args = vec![
        OsString::from("--unshare-all"),
        OsString::from("--share-net"),
        OsString::from("--die-with-parent"),
        OsString::from("--ro-bind"),
        OsString::from("/"),
        OsString::from("/"),
        OsString::from("--dev"),
        OsString::from("/dev"),
        OsString::from("--proc"),
        OsString::from("/proc"),
        OsString::from("--bind"),
        OsString::from("/tmp"),
        OsString::from("/tmp"),
    ];
    let tmp_root = fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"));
    let writable = sandbox_writable_paths(policy);
    for path in writable
        .iter()
        .map(|path| canonical_path(path))
        .collect::<BTreeSet<_>>()
    {
        if path == Path::new("/tmp") || path == tmp_root || !path.exists() {
            continue;
        }
        push_path_mount(&mut args, "--bind", &path)?;
    }
    for path in policy
        .protected_paths
        .iter()
        .map(|path| canonical_path(path))
        .collect::<BTreeSet<_>>()
    {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if path.to_str().is_none() {
                return Err(ProcessError::SandboxSetup(format!(
                    "non-UTF-8 path cannot be hidden: {}",
                    path.display()
                )));
            }
            args.extend([OsString::from("--tmpfs"), path.as_os_str().to_owned()]);
        } else {
            args.extend([
                OsString::from("--ro-bind"),
                OsString::from("/dev/null"),
                path.as_os_str().to_owned(),
            ]);
        }
    }
    args.extend([
        OsString::from("--chdir"),
        request.cwd.as_os_str().to_owned(),
    ]);
    // The outer supervisor already uses env_clear and passes the exact child
    // environment to bwrap; bwrap inherits it without adding argv content.
    let program = std::mem::replace(&mut request.program, bwrap.as_os_str().to_owned());
    let command_args = std::mem::take(&mut request.args);
    args.push(OsString::from("--"));
    args.push(program);
    args.extend(command_args);
    request.args = args;
    Ok(PreparedSandbox {
        request,
        observation: SandboxObservation {
            status: SandboxStatus::Confined,
            warning_reason: None,
        },
        cleanup_paths: Vec::new(),
    })
}

#[cfg(target_os = "linux")]
fn push_path_mount(
    args: &mut Vec<OsString>,
    option: &str,
    path: &Path,
) -> Result<(), ProcessError> {
    if path.to_str().is_none() {
        return Err(ProcessError::SandboxSetup(format!(
            "non-UTF-8 path cannot be mounted: {}",
            path.display()
        )));
    }
    args.extend([OsString::from(option), path.as_os_str().to_owned()]);
    if option == "--bind" {
        args.push(path.as_os_str().to_owned());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn find_program(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|directory| {
        let candidate = directory.join(name);
        (candidate.is_file() && is_executable(&candidate)).then_some(candidate)
    })
}

#[cfg(target_os = "linux")]
fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

fn sandbox_writable_paths(policy: &SandboxPolicy) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for path in &policy.writable_paths {
        paths.insert(path.clone());
        if let Ok(canonical) = fs::canonicalize(path) {
            paths.insert(canonical);
        }
    }
    paths.insert(PathBuf::from("/tmp"));
    if let Ok(canonical) = fs::canonicalize("/tmp") {
        paths.insert(canonical);
    }
    paths
}

#[cfg(target_os = "macos")]
fn normalized_paths(paths: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut normalized = BTreeSet::new();
    for path in paths {
        normalized.insert(path.clone());
        if let Ok(canonical) = fs::canonicalize(path) {
            normalized.insert(canonical);
        }
    }
    normalized
}

#[cfg(target_os = "linux")]
fn canonical_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

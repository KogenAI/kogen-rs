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

pub(super) fn executable_visible(path: &Path, policy: &SandboxPolicy) -> bool {
    if !policy.enabled || policy.already_sandboxed || policy.forced_unavailable.is_some() {
        return true;
    }

    #[cfg(target_os = "linux")]
    {
        if find_program("bwrap").is_none() {
            return true;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if !Path::new("/usr/bin/sandbox-exec").is_file() {
            return true;
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        return true;
    }

    let Ok(resolved_path) = fs::canonicalize(path) else {
        return false;
    };
    if policy.protected_paths.iter().any(|protected| {
        let protected = canonical_path(protected);
        path.starts_with(&protected) || resolved_path.starts_with(protected)
    }) {
        return false;
    }

    #[cfg(target_os = "linux")]
    {
        return linux_executable_visible(&resolved_path, policy);
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
}

#[cfg(target_os = "linux")]
fn linux_executable_visible(path: &Path, policy: &SandboxPolicy) -> bool {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let mount_points = mountinfo
        .lines()
        .filter_map(|line| line.split(" - ").next())
        .filter_map(|fields| fields.split_ascii_whitespace().nth(4))
        .map(unescape_mount_field)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let tmp_root = canonical_path(Path::new("/tmp"));
    let mut bind_targets = sandbox_writable_paths(policy)
        .into_iter()
        .map(|path| canonical_path(&path))
        .filter(|path| path.exists() && path != Path::new("/tmp") && path != &tmp_root)
        .collect::<BTreeSet<_>>();
    bind_targets.extend(
        policy
            .write_denied_paths
            .iter()
            .map(|path| canonical_path(path))
            .filter(|path| path.exists()),
    );
    bind_targets.extend([
        PathBuf::from("/tmp"),
        PathBuf::from("/dev"),
        PathBuf::from("/proc"),
    ]);
    linux_path_visible_with_binds(path, &bind_targets, &mount_points)
}

#[cfg(target_os = "linux")]
fn linux_path_visible_with_binds(
    path: &Path,
    bind_targets: &BTreeSet<PathBuf>,
    mount_points: &[PathBuf],
) -> bool {
    let Some(mount_point) = mount_points
        .iter()
        .filter(|mount_point| path.starts_with(mount_point))
        .max_by_key(|mount_point| mount_point.components().count())
    else {
        return false;
    };
    if mount_point == Path::new("/") {
        return true;
    }
    bind_targets
        .iter()
        .any(|target| path.starts_with(target) && target.starts_with(mount_point))
}

#[cfg(target_os = "linux")]
fn unescape_mount_field(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if index + 3 < bytes.len() && bytes[index] == b'\\' {
            let escape = &bytes[index + 1..index + 4];
            let value = match escape {
                b"040" => Some(b' '),
                b"011" => Some(b'\t'),
                b"012" => Some(b'\n'),
                b"134" => Some(b'\\'),
                _ => None,
            };
            if let Some(value) = value {
                decoded.push(value);
                index += 4;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
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
    for path in normalized_paths(&policy.write_denied_paths) {
        if let Some(path) = path.to_str() {
            profile.push_str(&format!(
                "(deny file-write* (subpath {}))\n",
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
        .write_denied_paths
        .iter()
        .map(|path| canonical_path(path))
        .collect::<BTreeSet<_>>()
    {
        if path.exists() {
            push_path_mount(&mut args, "--ro-bind", &path)?;
        }
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
    if matches!(option, "--bind" | "--ro-bind") {
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

#[cfg(all(test, target_os = "linux"))]
#[test]
fn executable_on_path_outside_the_sandbox_binds_is_not_visible() {
    use std::collections::BTreeSet;
    use std::fs::{self, OpenOptions};
    use std::os::unix::fs::OpenOptionsExt;

    let root = std::env::temp_dir().join(format!(
        "kogen-unbound-mise-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let outside_bin = root.join("outside/bin");
    let bound_bin = root.join("bound/bin");
    fs::create_dir_all(&outside_bin).expect("create outside fixture bin");
    fs::create_dir_all(&bound_bin).expect("create bound fixture bin");
    for bin in [&outside_bin, &bound_bin] {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(bin.join("mise"))
            .expect("create executable mise fixture");
    }
    let path = std::env::join_paths([&outside_bin]).expect("join outside fixture PATH");
    let environment = crate::run::EnvironmentMap::from([("PATH".into(), path)]);
    assert!(
        super::super::environment::find_executable_if("mise", &environment, |executable| {
            linux_path_visible_with_binds(
                executable,
                &BTreeSet::from([PathBuf::from("/tmp")]),
                &[PathBuf::from("/"), root.clone()],
            )
        })
        .is_none()
    );

    let path = std::env::join_paths([&outside_bin, &bound_bin]).expect("join fixture PATH");
    let environment = crate::run::EnvironmentMap::from([("PATH".into(), path)]);
    let bind_targets = BTreeSet::from([bound_bin.clone()]);
    let mount_points = [PathBuf::from("/"), root.clone()];
    let executable =
        super::super::environment::find_executable_if("mise", &environment, |executable| {
            linux_path_visible_with_binds(executable, &bind_targets, &mount_points)
        })
        .expect("PATH should skip unbound mise and choose the bound executable");
    assert_eq!(executable, bound_bin.join("mise"));
    let _ = fs::remove_dir_all(root);
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

fn canonical_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

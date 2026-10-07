//! Shared sandbox selection and child-process decorator.

use super::environment::EnvironmentMap;
use super::process::{
    ProcessError, ProcessPort, ProcessRequest, ProcessResult, SandboxObservation, SandboxStatus,
};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

mod platform;

pub trait SandboxIntegrityPort: Send + Sync {
    /// Returns a stable digest covering checkout state and origin refs.
    fn snapshot(&self) -> Result<String, String>;
}

#[derive(Clone, Debug)]
pub struct SandboxPolicy {
    enabled: bool,
    already_sandboxed: bool,
    forced_unavailable: Option<String>,
    verify_integrity: bool,
    writable_paths: Vec<PathBuf>,
    write_denied_paths: Vec<PathBuf>,
    protected_paths: Vec<PathBuf>,
}

impl SandboxPolicy {
    #[must_use]
    pub fn new(enabled: bool, workspace: impl Into<PathBuf>, run_dir: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        let run_dir = run_dir.into();
        Self {
            enabled,
            already_sandboxed: false,
            forced_unavailable: None,
            verify_integrity: false,
            writable_paths: vec![workspace, run_dir, PathBuf::from("/tmp")],
            write_denied_paths: Vec::new(),
            protected_paths: Vec::new(),
        }
    }

    /// Builds the default Build policy from §5.3 and the host-only test seams.
    #[must_use]
    pub fn for_build(
        enabled: bool,
        workspace: impl Into<PathBuf>,
        run_dir: impl Into<PathBuf>,
        host: &EnvironmentMap,
    ) -> Self {
        let workspace = workspace.into();
        let run_dir = run_dir.into();
        let home = host.get(OsStr::new("HOME")).map(PathBuf::from);
        let mut policy = Self {
            enabled,
            already_sandboxed: false,
            forced_unavailable: None,
            verify_integrity: false,
            writable_paths: vec![
                workspace,
                run_dir.join("logs"),
                run_dir.join("tmp"),
                run_dir.join("reports"),
                run_dir.join("mise-state"),
                run_dir.join("mise-cache"),
                PathBuf::from("/tmp"),
            ],
            write_denied_paths: Vec::new(),
            protected_paths: Vec::new(),
        };
        policy.verify_integrity = true;
        if let Some(home) = home {
            policy.add_default_paths(&home);
        }
        if let Some(path) = host.get(OsStr::new("KOGEN_AUTH_PATH")) {
            policy.protected_paths.push(PathBuf::from(path));
        }
        if host
            .get(OsStr::new("KOGEN_SANDBOXED"))
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == "1")
        {
            policy.already_sandboxed = true;
        }
        if host
            .get(OsStr::new("KOGEN_SANDBOX"))
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == "unavailable")
        {
            policy.forced_unavailable = Some("forced by KOGEN_SANDBOX=unavailable".to_owned());
        }
        if let Some(path) = host.get(OsStr::new("GOMODCACHE")) {
            policy.writable_paths.push(PathBuf::from(path));
        }
        policy
    }

    #[must_use]
    pub fn with_integrity_check(mut self, required: bool) -> Self {
        self.verify_integrity = required;
        self
    }

    #[must_use]
    pub fn with_already_sandboxed(mut self, already: bool) -> Self {
        self.already_sandboxed = already;
        self
    }

    #[must_use]
    pub fn with_unavailable_reason(mut self, reason: impl Into<String>) -> Self {
        self.forced_unavailable = Some(reason.into());
        self
    }

    pub fn allow_write(&mut self, path: impl Into<PathBuf>) {
        self.writable_paths.push(path.into());
    }

    pub fn protect_read(&mut self, path: impl Into<PathBuf>) {
        self.protected_paths.push(path.into());
    }

    pub fn deny_write(&mut self, path: impl Into<PathBuf>) {
        self.write_denied_paths.push(path.into());
    }

    #[must_use]
    pub fn verifies_integrity(&self) -> bool {
        self.verify_integrity
    }

    fn add_default_paths(&mut self, home: &Path) {
        self.writable_paths.extend([
            home.join(".cache/mise"),
            home.join(".hex"),
            home.join(".cache/rebar3"),
            home.join(".npm"),
            home.join(".cargo/registry"),
            home.join(".cargo/git"),
            home.join(".cache/go-build"),
        ]);
        self.protected_paths.extend([
            home.join(".kogen/credentials"),
            home.join(".ssh"),
            home.join(".gnupg"),
            home.join(".codex"),
        ]);
        let kogen = home.join(".kogen");
        if let Ok(entries) = fs::read_dir(kogen) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("credentials"))
                {
                    self.protected_paths.push(path);
                }
            }
        }
    }
}

pub struct SandboxedProcessPort<'a> {
    inner: &'a dyn ProcessPort,
    policy: SandboxPolicy,
    integrity: Option<&'a dyn SandboxIntegrityPort>,
}

impl<'a> SandboxedProcessPort<'a> {
    #[must_use]
    pub fn new(
        inner: &'a dyn ProcessPort,
        policy: SandboxPolicy,
        integrity: Option<&'a dyn SandboxIntegrityPort>,
    ) -> Self {
        Self {
            inner,
            policy,
            integrity,
        }
    }
}

impl ProcessPort for SandboxedProcessPort<'_> {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        let prepared = platform::prepare(request, &self.policy)?;
        let sandbox_exec_wrapper = prepared.request.program == OsStr::new("/usr/bin/sandbox-exec");
        let before = if prepared.observation.status != SandboxStatus::Confined
            && self.policy.verify_integrity
        {
            match self.integrity {
                Some(port) => match port.snapshot() {
                    Ok(snapshot) => Some(snapshot),
                    Err(detail) => {
                        cleanup(&prepared.cleanup_paths);
                        return Err(ProcessError::SandboxIntegrityRead(detail));
                    }
                },
                None => {
                    cleanup(&prepared.cleanup_paths);
                    return Err(ProcessError::SandboxIntegrityRequired);
                }
            }
        } else {
            None
        };

        let result = self.inner.run(prepared.request);
        let after = match (before.as_ref(), self.integrity) {
            (Some(_), Some(port)) => port
                .snapshot()
                .map(Some)
                .map_err(ProcessError::SandboxIntegrityRead),
            _ => Ok(None),
        };
        cleanup(&prepared.cleanup_paths);
        let after = after?;
        let mut result = result?;
        if sandbox_exec_wrapper && sandbox_exec_target_missing(&result.output_tail) {
            result.exit_status = Some(127);
            result.unavailable = true;
        }
        if before.as_ref() != after.as_ref() {
            return Err(ProcessError::SandboxIntegrityChanged);
        }
        result.sandbox = Some(prepared.observation);
        Ok(result)
    }
}

fn sandbox_exec_target_missing(output: &[u8]) -> bool {
    String::from_utf8_lossy(output).lines().any(|line| {
        line.starts_with("sandbox-exec: execvp() of '")
            && line.ends_with(": No such file or directory")
    })
}

pub(super) struct PreparedSandbox {
    pub request: ProcessRequest,
    pub observation: SandboxObservation,
    pub cleanup_paths: Vec<PathBuf>,
}

fn cleanup(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;

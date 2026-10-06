//! Shape-time project commands, temporary staging and base acceptance reads.

mod format;

use super::snapshot::GitSnapshot;
use super::validation::ValidationFailure;
use crate::gate::ledger::{
    AcceptanceFailure, CommandAcceptanceRequest, TreeSnapshotPort, run_command_acceptance,
};
use crate::project::ProjectConfig;
use crate::run::{
    ChildEnvironment, EnvironmentRequest, ProcessPort, ProcessRequest, ProcessResult,
    ProcessSupervisor, SandboxPolicy, SandboxedProcessPort, build_child_environment,
    host_environment,
};
use serde_yaml::Value;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) struct ShapeCommands {
    pub process: ProcessSupervisor,
    pub environment: ChildEnvironment,
    pub runner_dir: PathBuf,
}

impl ShapeCommands {
    pub fn new(
        checkout: &Path,
        run_dir: &Path,
        config: Option<&ProjectConfig>,
    ) -> Result<Self, crate::error::CoreError> {
        let process = ProcessSupervisor;
        let mut request = EnvironmentRequest::new(host_environment(), run_dir, checkout, checkout);
        if let Some(config) = config
            && let Some(environment) = config.raw.get("env").and_then(Value::as_mapping)
        {
            for (key, value) in environment {
                if let (Some(key), Some(value)) = (key.as_str(), value.as_str()) {
                    request.project.insert(key.to_owned(), value.to_owned());
                }
            }
        }
        let environment = build_child_environment(&process, request).map_err(|error| {
            crate::error::CoreError::new(
                crate::error::ErrorClass::Environment,
                "shape_environment_failed",
                error.to_string(),
                crate::ExitCode::Environment,
            )
        })?;
        Ok(Self {
            process,
            environment,
            runner_dir: run_dir.to_path_buf(),
        })
    }

    pub fn setup(
        &self,
        checkout: &Path,
        config: Option<&ProjectConfig>,
    ) -> Result<(), crate::error::CoreError> {
        let Some(rows) = config
            .and_then(|config| config.raw.get("setup"))
            .and_then(Value::as_sequence)
        else {
            return Ok(());
        };
        for row in rows {
            let Some(argv) = row.get("argv").and_then(Value::as_sequence) else {
                continue;
            };
            let result = self.run_argv(
                argv,
                checkout,
                Duration::from_millis(
                    row.get("timeout_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(60_000),
                ),
                "shape-setup",
            );
            match result {
                Ok(result)
                    if !result.unavailable
                        && !result.timed_out
                        && result.exit_status == Some(0) => {}
                Ok(result) => {
                    return Err(crate::error::CoreError::new(
                        crate::error::ErrorClass::Environment,
                        "setup_failed",
                        format!(
                            "setup failed with exit {}; log: {}",
                            result.exit_status.unwrap_or(-1),
                            result.log_path.display()
                        ),
                        crate::ExitCode::Environment,
                    ));
                }
                Err(error) => {
                    return Err(crate::error::CoreError::new(
                        crate::error::ErrorClass::Environment,
                        "setup_failed",
                        error,
                        crate::ExitCode::Environment,
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn staged_acceptance_checks(
        &self,
        checkout: &Path,
        config: Option<&ProjectConfig>,
        candidate_rel: &str,
        source_bytes: &[u8],
    ) -> Result<(), ValidationFailure> {
        let candidate = checkout.join(candidate_rel);
        if fs::symlink_metadata(&candidate).is_ok() {
            return Err(ValidationFailure {
                reason: "acceptance_check_path_conflict",
                detail: format!(
                    "checkout-relative staged path {candidate_rel} is already occupied"
                ),
            });
        }
        let parent = candidate.parent().ok_or_else(|| ValidationFailure {
            reason: "acceptance_check_path_conflict",
            detail: format!("invalid candidate path {candidate_rel}"),
        })?;
        fs::create_dir_all(parent).map_err(|error| ValidationFailure {
            reason: "acceptance_check_failed",
            detail: error.to_string(),
        })?;
        fs::write(&candidate, source_bytes).map_err(|error| ValidationFailure {
            reason: "acceptance_check_failed",
            detail: error.to_string(),
        })?;
        let result = self.run_staged_checks(checkout, config, candidate_rel);
        let restore = fs::remove_file(&candidate);
        if let Err(error) = restore
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(ValidationFailure {
                reason: "acceptance_check_failed",
                detail: format!("could not restore staged acceptance path: {error}"),
            });
        }
        result
    }

    pub fn base_acceptance(
        &self,
        checkout: &Path,
        config: Option<&ProjectConfig>,
        slug: &str,
        source_rel: &str,
        report_path: &Path,
        item_ids: impl IntoIterator<Item = String>,
    ) -> Result<BTreeSet<String>, ValidationFailure> {
        let Some(acceptance) = config.and_then(|config| config.raw.get("acceptance")) else {
            return Err(ValidationFailure {
                reason: "acceptance_adapter_unavailable",
                detail: "project has no acceptance adapter".to_owned(),
            });
        };
        let command = acceptance
            .get("run")
            .and_then(Value::as_sequence)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(OsString::from)
            .collect::<Vec<_>>();
        let timeout = Duration::from_millis(
            acceptance
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(600_000),
        );
        let expected_items = item_ids.into_iter().collect();
        let request = CommandAcceptanceRequest {
            slug: slug.to_owned(),
            command,
            candidate_path: PathBuf::from(source_rel),
            workdir: checkout.to_path_buf(),
            run_dir: self.runner_dir.clone(),
            report_path: report_path.to_path_buf(),
            env: self.environment.clone(),
            timeout,
            expected_items,
            adapter_unavailable: false,
        };
        let result =
            run_command_acceptance(&self.process, &GitSnapshot, request).map_err(|error| {
                ValidationFailure {
                    reason: "acceptance_failed",
                    detail: error.to_string(),
                }
            })?;
        if result.process.unavailable || matches!(result.process.exit_status, Some(126 | 127)) {
            return Err(ValidationFailure {
                reason: "acceptance_adapter_unavailable",
                detail: format!(
                    "acceptance adapter is unavailable; log: {}",
                    result.process.log_path.display()
                ),
            });
        }
        if result
            .failures
            .iter()
            .any(|failure| matches!(failure, AcceptanceFailure::TreeMutated))
        {
            return Err(ValidationFailure {
                reason: "tree_mutated",
                detail: "base acceptance run changed the checkout tree".to_owned(),
            });
        }
        if let Some(failure) = result.failures.iter().find(|failure| {
            !matches!(
                failure,
                AcceptanceFailure::Suite | AcceptanceFailure::TreeMutated
            )
        }) {
            return Err(ValidationFailure {
                reason: "acceptance_failed",
                detail: format!("base acceptance run failed: {failure:?}"),
            });
        }
        Ok(result
            .item_pass
            .into_iter()
            .filter_map(|(id, passed)| passed.then_some(id))
            .collect())
    }

    fn run_staged_checks(
        &self,
        checkout: &Path,
        config: Option<&ProjectConfig>,
        candidate_rel: &str,
    ) -> Result<(), ValidationFailure> {
        let Some(checks) = config
            .and_then(|config| config.raw.get("acceptance_checks"))
            .and_then(Value::as_sequence)
        else {
            return Ok(());
        };
        let before = GitSnapshot
            .snapshot(checkout)
            .map_err(|detail| ValidationFailure {
                reason: "acceptance_check_failed",
                detail,
            })?;
        for (index, check) in checks.iter().enumerate() {
            let Some(argv) = check.get("argv").and_then(Value::as_sequence) else {
                continue;
            };
            let args = argv
                .iter()
                .filter_map(Value::as_str)
                .map(|arg| OsString::from(arg.replace("{path}", candidate_rel)))
                .collect::<Vec<_>>();
            let timeout = Duration::from_millis(
                check
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(600_000),
            );
            let result = self
                .run_os_argv(&args, checkout, timeout, &format!("shape-check-{index}"))
                .map_err(|detail| ValidationFailure {
                    reason: "acceptance_check_failed",
                    detail,
                })?;
            if result.unavailable || matches!(result.exit_status, Some(126 | 127)) {
                return Err(ValidationFailure {
                    reason: "acceptance_check_unavailable",
                    detail: format!(
                        "acceptance check {} is unavailable",
                        check.get("name").and_then(Value::as_str).unwrap_or("?")
                    ),
                });
            }
            if result.timed_out || result.exit_status != Some(0) {
                return Err(ValidationFailure {
                    reason: "acceptance_check_failed",
                    detail: format!(
                        "acceptance check {} failed with exit {}",
                        check.get("name").and_then(Value::as_str).unwrap_or("?"),
                        result.exit_status.unwrap_or(-1)
                    ),
                });
            }
        }
        let after = GitSnapshot
            .snapshot(checkout)
            .map_err(|detail| ValidationFailure {
                reason: "acceptance_check_failed",
                detail,
            })?;
        if before != after {
            return Err(ValidationFailure {
                reason: "tree_mutated",
                detail: "acceptance checks changed the checkout tree".to_owned(),
            });
        }
        Ok(())
    }

    fn run_argv(
        &self,
        argv: &[Value],
        cwd: &Path,
        timeout: Duration,
        log_name: &str,
    ) -> Result<ProcessResult, String> {
        let args = argv
            .iter()
            .filter_map(Value::as_str)
            .map(OsString::from)
            .collect::<Vec<_>>();
        self.run_os_argv(&args, cwd, timeout, log_name)
    }

    fn run_os_argv(
        &self,
        argv: &[OsString],
        cwd: &Path,
        timeout: Duration,
        log_name: &str,
    ) -> Result<ProcessResult, String> {
        let Some((program, args)) = argv.split_first() else {
            return Err("project command requires a non-empty argv".to_owned());
        };
        let mut request = ProcessRequest::new(program.clone(), cwd, &self.runner_dir);
        request.args = args.to_vec();
        request.env = self.environment.clone();
        request.timeout = timeout;
        request.log_name = log_name.to_owned();
        let policy = SandboxPolicy::for_build(true, cwd, &self.runner_dir, &host_environment())
            .with_integrity_check(false);
        let sandbox = SandboxedProcessPort::new(&self.process, policy, None);
        sandbox.run(request).map_err(|error| error.to_string())
    }
}

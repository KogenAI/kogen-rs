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
    acceptance: ShapeAcceptance,
}

struct ShapeAcceptance {
    adapter: String,
    extension: String,
    candidate_dir: String,
    command: Vec<OsString>,
    timeout: Duration,
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
            acceptance: shape_acceptance(config, checkout),
        })
    }

    pub fn acceptance_paths(
        config: Option<&ProjectConfig>,
        checkout: &Path,
        slug: &str,
    ) -> (String, String) {
        let acceptance = shape_acceptance(config, checkout);
        (
            format!(".kogen/acceptance/{slug}{}", acceptance.extension),
            format!(
                "{}/{}{extension}",
                acceptance.candidate_dir,
                slug,
                extension = acceptance.extension
            ),
        )
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
        let relative = Path::new(candidate_rel);
        crate::safe_fs::validate_write(checkout, relative).map_err(|error| ValidationFailure {
            reason: "acceptance_check_path_conflict",
            detail: error.to_string(),
        })?;
        crate::safe_fs::create_file(checkout, relative, source_bytes).map_err(|error| {
            ValidationFailure {
                reason: "acceptance_check_failed",
                detail: error.to_string(),
            }
        })?;
        let result = self.run_staged_checks(checkout, config, candidate_rel);
        let restore = crate::safe_fs::remove_file(checkout, relative);
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
        slug: &str,
        source_rel: &str,
        report_path: &Path,
        item_ids: impl IntoIterator<Item = String>,
    ) -> Result<BTreeSet<String>, ValidationFailure> {
        let use_mise = std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("mise").is_file()));
        let command_argv0 = match self.acceptance.adapter.as_str() {
            "exunit" if use_mise => "mise".to_owned(),
            "exunit" => "elixir".to_owned(),
            "rails" => "bundle".to_owned(),
            _ => self.acceptance.command.first().map_or_else(
                || "acceptance runner".to_owned(),
                |argv0| argv0.to_string_lossy().into_owned(),
            ),
        };
        let command = match self.acceptance.adapter.as_str() {
            "exunit" => Vec::new(),
            "rails" => crate::gate::adapters::rails::runner_command(),
            _ => self.acceptance.command.clone(),
        };
        let expected_items = item_ids.into_iter().collect();
        let mut environment = self.environment.clone();
        if self.acceptance.adapter == "rails" {
            environment.extend(crate::gate::adapters::rails::child_environment(
                &checkout.join("vendor/cache"),
            ));
        }
        let request = CommandAcceptanceRequest {
            slug: slug.to_owned(),
            command,
            candidate_path: PathBuf::from(source_rel),
            workdir: checkout.to_path_buf(),
            run_dir: self.runner_dir.clone(),
            report_path: report_path.to_path_buf(),
            env: environment,
            timeout: self.acceptance.timeout,
            expected_items,
            adapter_unavailable: false,
        };
        let result = if self.acceptance.adapter == "exunit" {
            crate::gate::adapters::exunit::run_acceptance(
                &self.process,
                &GitSnapshot,
                request,
                use_mise,
            )
            .map_err(|error| ValidationFailure {
                reason: "acceptance_failed",
                detail: error.to_string(),
            })?
        } else {
            run_command_acceptance(&self.process, &GitSnapshot, request).map_err(|error| {
                ValidationFailure {
                    reason: "acceptance_failed",
                    detail: error.to_string(),
                }
            })?
        };
        if self.acceptance.adapter == "exunit"
            && !result.process.timed_out
            && (result.process.unavailable
                || result.process.exit_status.is_some_and(|status| status != 0))
            && let Some(failure) = crate::gate::adapters::exunit::process_environment_failure(
                &result.process,
                use_mise,
            )
        {
            return Err(ValidationFailure {
                reason: failure.reason,
                detail: failure.detail,
            });
        }
        if result.process.unavailable || matches!(result.process.exit_status, Some(126 | 127)) {
            return Err(ValidationFailure {
                reason: "tool_missing",
                detail: format!("{command_argv0} is not available"),
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

fn shape_acceptance(config: Option<&ProjectConfig>, checkout: &Path) -> ShapeAcceptance {
    let acceptance = config.and_then(|config| config.raw.get("acceptance"));
    let adapter = acceptance
        .and_then(|acceptance| acceptance.get("adapter"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if crate::gate::adapters::rails::detected(checkout) {
                "rails".to_owned()
            } else {
                "exunit".to_owned()
            }
        });
    let extension = acceptance
        .and_then(|acceptance| acceptance.get("ext"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| match adapter.as_str() {
            "command" => ".test".to_owned(),
            "rails" => crate::gate::adapters::rails::ACCEPTANCE_EXTENSION.to_owned(),
            _ => crate::gate::adapters::exunit::ACCEPTANCE_EXTENSION.to_owned(),
        });
    let candidate_dir = acceptance
        .and_then(|acceptance| acceptance.get("candidate_dir"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| match adapter.as_str() {
            "command" => "test/acceptance".to_owned(),
            "rails" => crate::gate::adapters::rails::CANDIDATE_DIRECTORY.to_owned(),
            _ => crate::gate::adapters::exunit::CANDIDATE_DIRECTORY.to_owned(),
        });
    let command = acceptance
        .and_then(|acceptance| acceptance.get("run"))
        .and_then(Value::as_sequence)
        .and_then(|items| items.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        .unwrap_or_default()
        .into_iter()
        .map(OsString::from)
        .collect();
    let timeout = Duration::from_millis(
        acceptance
            .and_then(|acceptance| acceptance.get("timeout_ms"))
            .and_then(Value::as_u64)
            .unwrap_or(600_000),
    );
    ShapeAcceptance {
        adapter,
        extension,
        candidate_dir,
        command,
        timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::{ShapeAcceptance, ShapeCommands};
    use crate::project::ProjectConfig;
    use crate::run::{ChildEnvironment, ProcessSupervisor};
    use std::time::Duration;

    #[test]
    fn shape_paths_follow_builtin_and_command_acceptance_adapters() {
        let checkout = std::env::temp_dir().join(format!(
            "kogen-shape-acceptance-paths-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&checkout).unwrap();

        assert_eq!(
            ShapeCommands::acceptance_paths(None, &checkout, "greet"),
            (
                ".kogen/acceptance/greet_test.exs".to_owned(),
                "test/acceptance/greet_test.exs".to_owned(),
            )
        );

        let config = ProjectConfig {
            name: "test".to_owned(),
            base: None,
            raw: serde_yaml::from_str(
                "acceptance:\n  adapter: command\n  ext: .t.sh\n  candidate_dir: test/acceptance\n  run: [sh, run.sh, '{path}']\n",
            )
            .unwrap(),
        };
        assert_eq!(
            ShapeCommands::acceptance_paths(Some(&config), &checkout, "greet"),
            (
                ".kogen/acceptance/greet.t.sh".to_owned(),
                "test/acceptance/greet.t.sh".to_owned(),
            )
        );

        std::fs::remove_dir_all(checkout).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn shape_staging_rejects_a_symlink_parent_before_writing() {
        use std::os::unix::fs::symlink;

        let checkout = std::env::temp_dir().join(format!(
            "kogen-shape-staging-symlink-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let outside = checkout.with_extension("outside");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, checkout.join("test")).unwrap();
        let commands = ShapeCommands {
            process: ProcessSupervisor,
            environment: ChildEnvironment::new(),
            runner_dir: checkout.clone(),
            acceptance: ShapeAcceptance {
                adapter: "command".to_owned(),
                extension: ".t.sh".to_owned(),
                candidate_dir: "test/acceptance".to_owned(),
                command: Vec::new(),
                timeout: Duration::from_secs(1),
            },
        };

        let failure = commands
            .staged_acceptance_checks(
                &checkout,
                None,
                "test/acceptance/greet.t.sh",
                b"approved bytes",
            )
            .expect_err("staging must reject the symlinked parent");
        assert_eq!(failure.reason, "acceptance_check_path_conflict");
        assert!(!outside.join("acceptance/greet.t.sh").exists());
        let _ = std::fs::remove_dir_all(checkout);
        let _ = std::fs::remove_dir_all(outside);
    }
}

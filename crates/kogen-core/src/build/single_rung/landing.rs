use super::super::approval::ApprovedBuild;
use super::super::config::BuildOptions;
use super::super::provider::BuildProvider;
use super::super::support::{self, IntegritySnapshot};
use super::super::{controller_error, environment_error};
use crate::error::{CoreError, ErrorClass};
use crate::git::landing::{
    IntegrationGate, IntegrationResult, LandingOutcome, LandingRepository, LandingRequest,
    LandingWait, RebaseAttempt, RebaseKind, RepairResult,
};
use crate::project::ProjectResolution;
use crate::run::{ChildEnvironment, ProcessSupervisor, RunEvent, RunSnapshot, RunStore};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) struct LandResult {
    pub outcome: LandingOutcome,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn land_candidate(
    project: &ProjectResolution,
    approved: &ApprovedBuild,
    options: &BuildOptions,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
    candidate: &LandingRepository,
    expected_parent: &str,
    tree: &str,
    integrity: &IntegritySnapshot,
    supervisor: &ProcessSupervisor,
    provider: &mut BuildProvider<'_>,
    baseline_tree: &str,
    excluded_paths: &[PathBuf],
) -> Result<LandResult, CoreError> {
    let mut integration = Reverify {
        project,
        approved,
        options,
        store,
        candidate,
        run_dir: store.directory().to_path_buf(),
        candidate_workspace: candidate.workspace().to_path_buf(),
        integrity,
        supervisor,
        provider,
        baseline_tree,
        excluded_paths,
    };
    let mut wait = ScaledLandingWait;
    let setup_outputs = options
        .setup_outputs
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let outcome = crate::git::landing::land_excluding(
        LandingRequest {
            repository: candidate,
            store,
            snapshot,
            title: &approved.intent.frontmatter.title,
            expected_parent,
            verified_tree: tree,
        },
        &setup_outputs,
        &mut integration,
        &mut wait,
    )
    .map_err(|error| match error.kind {
        crate::git::landing::LandingErrorKind::Controller
        | crate::git::landing::LandingErrorKind::InvalidRequest => {
            controller_error("landing_failed", error.to_string())
        }
        _ => environment_error("landing_failed", error.to_string()),
    })?;
    Ok(LandResult { outcome })
}

struct Reverify<'a, 'p, 'build> {
    project: &'a ProjectResolution,
    approved: &'a ApprovedBuild,
    options: &'a BuildOptions,
    store: &'a RunStore,
    candidate: &'a LandingRepository,
    run_dir: PathBuf,
    candidate_workspace: PathBuf,
    integrity: &'a IntegritySnapshot,
    supervisor: &'a ProcessSupervisor,
    provider: &'p mut BuildProvider<'build>,
    baseline_tree: &'a str,
    excluded_paths: &'a [PathBuf],
}

impl IntegrationGate for Reverify<'_, '_, '_> {
    fn reverify_and_repair(
        &mut self,
        _workspace: &Path,
        new_parent: &str,
        rebase: &RebaseAttempt,
        snapshot: &mut RunSnapshot,
        deadline: Instant,
    ) -> Result<IntegrationResult, crate::git::landing::LandingError> {
        if matches!(rebase, RebaseAttempt::Impossible { .. }) {
            return Ok(IntegrationResult {
                rebase: RebaseKind::Impossible,
                repairs: Vec::new(),
                verified_tree: None,
            });
        }

        let base_path = self
            .project
            .state_root
            .join(format!("{}-moved-base", snapshot.run_id));
        let base = LandingRepository::clone_fresh(&self.project.origin, &base_path, new_parent)
            .map_err(landing_failure)?;
        let candidate_env = support::child_environment(
            self.project,
            &self.run_dir,
            &self.candidate_workspace,
            self.options,
        )
        .map_err(|error| landing_failure(core_error_text(error)))?;

        let (mut rebase_kind, mut feedback, mut count, conflict_paths) = match rebase {
            RebaseAttempt::Impossible { .. } => unreachable!(),
            RebaseAttempt::Conflict { paths, detail } => {
                let feedback = format!(
                    "Moved-base rebase conflict. Conflicting paths: {}\n{}",
                    paths.join(", "),
                    detail
                );
                (RebaseKind::Conflict, feedback, paths.len(), paths.clone())
            }
            RebaseAttempt::Clean => {
                let report =
                    self.verify_moved_base(&base, new_parent, &candidate_env, snapshot, &[])?;
                if report.is_landable() {
                    let tree = report.verified_tree.clone();
                    let _ = base.cleanup_workspace();
                    return Ok(IntegrationResult {
                        rebase: RebaseKind::Green,
                        repairs: Vec::new(),
                        verified_tree: tree,
                    });
                }
                (
                    RebaseKind::Red,
                    support::gate_feedback(self.approved, &report, &self.run_dir),
                    support::gate_failure_count(&report),
                    Vec::new(),
                )
            }
        };

        let mut repairs = Vec::new();
        let mut verified_tree = None;
        loop {
            if Instant::now() >= deadline {
                repairs.push(RepairResult::Spent);
                break;
            }
            self.record_repair(snapshot, &rebase_kind, count, deadline)?;
            let protection =
                support::protected_workspace(self.project, self.approved, self.options, new_parent)
                    .map_err(|error| landing_failure(core_error_text(error)))?;
            let builder_runner = support::sandboxed(
                self.project,
                self.options,
                &self.candidate_workspace,
                &self.run_dir,
                self.supervisor,
                self.integrity,
            );
            let conflict_before = conflict_paths
                .iter()
                .map(|path| {
                    (
                        path.clone(),
                        conflict_path_state(&self.candidate_workspace, path),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let develop = loop {
                match self.provider.repair(
                    snapshot,
                    &self.run_dir,
                    &self.candidate_workspace,
                    self.baseline_tree,
                    self.excluded_paths,
                    &builder_runner,
                    candidate_env.clone(),
                    &protection,
                    &feedback,
                    Some(deadline),
                ) {
                    Ok(result) => break Some(result),
                    Err(error)
                        if error.class == ErrorClass::Provider && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_error) if Instant::now() >= deadline => break None,
                    Err(error) => {
                        let _ = base.cleanup_workspace();
                        return Err(landing_failure(core_error_text(error)));
                    }
                }
            };
            let Some(develop) = develop else {
                repairs.push(RepairResult::Spent);
                break;
            };
            if develop.reason == "landing_allowance_spent" {
                repairs.push(RepairResult::Spent);
                break;
            }

            let unresolved =
                stage_resolved_conflicts(self.candidate, &conflict_paths, &conflict_before)?;
            let report =
                self.verify_moved_base(&base, new_parent, &candidate_env, snapshot, &unresolved)?;
            if report.is_landable() && unresolved.is_empty() {
                let tree = report.verified_tree.clone().ok_or_else(|| {
                    landing_failure("green moved-base verification omitted its tree")
                })?;
                verified_tree = Some(tree.clone());
                repairs.push(RepairResult::Green {
                    verified_tree: tree,
                });
                break;
            }
            count = support::gate_failure_count(&report);
            feedback = support::gate_feedback(self.approved, &report, &self.run_dir);
            if !unresolved.is_empty() {
                count = count.saturating_add(unresolved.len());
                feedback.push_str(&format!(
                    "\nMoved-base rebase still has unresolved conflicts: {}",
                    unresolved.join(", ")
                ));
            }
            repairs.push(RepairResult::Red);
            rebase_kind = match rebase_kind {
                RebaseKind::Conflict => RebaseKind::Conflict,
                _ => RebaseKind::Red,
            };
        }

        let _ = base.cleanup_workspace();
        Ok(IntegrationResult {
            rebase: rebase_kind,
            repairs,
            verified_tree,
        })
    }
}

impl Reverify<'_, '_, '_> {
    fn verify_moved_base(
        &self,
        base: &LandingRepository,
        new_parent: &str,
        candidate_env: &ChildEnvironment,
        snapshot: &mut RunSnapshot,
        guard_findings: &[String],
    ) -> Result<crate::gate::GateReport, crate::git::landing::LandingError> {
        let protection =
            support::protected_workspace(self.project, self.approved, self.options, new_parent)
                .map_err(|error| landing_failure(core_error_text(error)))?;
        let request = support::gate_request(
            self.project,
            self.approved,
            self.options,
            &self.run_dir,
            base.workspace(),
            &self.candidate_workspace,
            candidate_env.clone(),
            protection,
        )
        .map_err(|error| landing_failure(core_error_text(error)))?;
        let runner = support::sandboxed_pair(
            self.project,
            self.options,
            &self.candidate_workspace,
            base.workspace(),
            &self.run_dir,
            self.supervisor,
            self.integrity,
        );
        let report = crate::gate::run_gate(&runner, &request).map_err(landing_failure)?;
        let tree = report.verified_tree.clone();
        let mut checks = json!(
            report
                .checks
                .iter()
                .map(|check| json!({
                    "name":check.name,
                    "exit_status":check.exit_status,
                    "status":check.status.as_str(),
                    "excused":check.excused,
                    "findings":check.findings.iter().map(|finding| json!({
                        "path":finding.path,
                        "rule":finding.rule,
                        "symbol":finding.symbol,
                        "message":finding.message,
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        );
        if !guard_findings.is_empty() {
            checks
                .as_array_mut()
                .expect("serialized checks are an array")
                .push(json!({
                    "name":"rebase",
                    "exit_status":null,
                    "status":"red",
                    "excused":false,
                    "findings":guard_findings.iter().map(|path| json!({
                        "path":path,
                        "rule":"rebase/unresolved",
                        "symbol":"",
                        "message":"moved-base conflict has not been resolved",
                    })).collect::<Vec<_>>(),
                }));
        }
        let acceptance = json!(
            report
                .acceptance
                .item_pass
                .iter()
                .map(|(id, passed)| json!({
                    "id":id,
                    "status":if *passed {"passed"} else {"failed"},
                    "demoted":false,
                }))
                .collect::<Vec<_>>()
        );
        let event = RunEvent::new("verification", now_ms())
            .with("rung", json!("R1"))
            .with("tree", json!(tree))
            .with(
                "result",
                json!(if report.is_landable() && guard_findings.is_empty() {
                    "green"
                } else {
                    "red"
                }),
            )
            .with("checks", checks)
            .with("acceptance", acceptance)
            .with(
                "count",
                json!(support::gate_failure_count(&report) + guard_findings.len()),
            );
        self.store
            .record(&event, snapshot)
            .map_err(landing_failure)?;
        Ok(report)
    }

    fn record_repair(
        &self,
        snapshot: &mut RunSnapshot,
        rebase: &RebaseKind,
        count: usize,
        deadline: Instant,
    ) -> Result<(), crate::git::landing::LandingError> {
        let reason = if *rebase == RebaseKind::Conflict {
            "rebase_conflict"
        } else {
            "verification_red"
        };
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let event = RunEvent::new("repair", now_ms())
            .with("rung", json!("R1"))
            .with("reason", json!(reason))
            .with("repairs_left", json!(remaining_ms))
            .with("count", json!(count));
        self.store.record(&event, snapshot).map_err(landing_failure)
    }
}

fn landing_failure(error: impl std::fmt::Display) -> crate::git::landing::LandingError {
    crate::git::landing::LandingError {
        kind: crate::git::landing::LandingErrorKind::Controller,
        operation: "reverify moved base",
        detail: error.to_string(),
    }
}

fn core_error_text(error: CoreError) -> String {
    format!(
        "{}/{}: {}",
        error.class.as_str(),
        error.reason,
        error.detail
    )
}

fn conflict_path_state(workspace: &Path, relative: &str) -> Option<(u32, Vec<u8>)> {
    let path = workspace.join(relative);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o177_777
    };
    #[cfg(not(unix))]
    let mode = u32::from(metadata.permissions().readonly());
    let bytes = if metadata.file_type().is_symlink() {
        std::fs::read_link(path)
            .ok()?
            .to_string_lossy()
            .as_bytes()
            .to_vec()
    } else if metadata.is_file() {
        std::fs::read(path).ok()?
    } else {
        Vec::new()
    };
    Some((mode, bytes))
}

fn stage_resolved_conflicts(
    repository: &LandingRepository,
    conflict_paths: &[String],
    before: &BTreeMap<String, Option<(u32, Vec<u8>)>>,
) -> Result<Vec<String>, crate::git::landing::LandingError> {
    repository
        .reset_workspace_git_settings()
        .map_err(landing_failure)?;
    let workspace = repository.workspace();
    let repo = crate::git::GitRepo::workspace(workspace);
    for path in conflict_paths {
        let current = conflict_path_state(workspace, path);
        if before.get(path) != Some(&current) {
            repo.output(&["add", "--", path]).map_err(landing_failure)?;
        }
    }
    let unmerged = repo
        .output(&["ls-files", "-u", "-z"])
        .map_err(landing_failure)?;
    let unresolved = unmerged
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (_, path) = entry.split_once('\t')?;
            Some(path.to_owned())
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(unresolved)
}

#[derive(Default)]
struct ScaledLandingWait;

impl LandingWait for ScaledLandingWait {
    fn wait(&mut self, delay: Duration) {
        let scale = std::env::var("KOGEN_TIME_SCALE")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(1.0);
        std::thread::sleep(Duration::from_millis(
            (delay.as_millis() as f64 * scale).floor().max(1.0) as u64,
        ));
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitRepo;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn conflict_repair_clears_candidate_filter_config_before_staging() {
        let root = test_dir();
        let origin = root.join("origin.git");
        let seed = root.join("seed");
        let workspace = root.join("workspace");
        git(
            &root,
            &["init", "--bare", "--initial-branch=main", path(&origin)],
        );
        git(&root, &["init", "--initial-branch=main", path(&seed)]);
        kogen_test_support::set_identity(&seed, "Kogen Test", "test@kogen.invalid")
            .expect("set fixture identity");
        fs::write(seed.join("tracked.txt"), b"base\n").expect("write base file");
        git(&seed, &["add", "tracked.txt"]);
        git(&seed, &["commit", "--quiet", "-m", "base"]);
        git(&seed, &["remote", "add", "origin", path(&origin)]);
        git(&seed, &["push", "--quiet", "origin", "main"]);
        let base = GitRepo::new(&origin)
            .resolve_commit("refs/heads/main")
            .expect("resolve base");
        let repository =
            LandingRepository::clone_fresh(&origin, &workspace, &base).expect("clone workspace");
        let before = conflict_path_state(&workspace, "tracked.txt");
        fs::write(workspace.join("tracked.txt"), b"repaired\n").expect("write repair");
        fs::write(
            workspace.join(".gitattributes"),
            "tracked.txt filter=evil\n",
        )
        .expect("install candidate attribute");
        let marker = root.join("filter-ran");
        let filter = format!("sh -c 'touch \"{}\"; cat'", path(&marker));
        git(&workspace, &["config", "filter.evil.clean", &filter]);
        git(&workspace, &["config", "filter.evil.smudge", "cat"]);

        let unresolved = stage_resolved_conflicts(
            &repository,
            &["tracked.txt".to_owned()],
            &BTreeMap::from([("tracked.txt".to_owned(), before)]),
        )
        .expect("stage resolved repair without candidate Git controls");
        assert!(unresolved.is_empty());
        assert!(
            !marker.exists(),
            "candidate clean filter ran in the controller"
        );
        let staged = GitRepo::workspace(&workspace)
            .output(&["show", ":tracked.txt"])
            .expect("read staged repair blob");
        assert_eq!(staged, b"repaired\n");
        let _ = fs::remove_dir_all(root);
    }

    fn git(directory: &Path, args: &[&str]) {
        let output = kogen_test_support::git_command()
            .args(args)
            .current_dir(directory)
            .output()
            .expect("run fixture Git command");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn path(path: &Path) -> &str {
        path.to_str().expect("temporary path is UTF-8")
    }

    fn test_dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "kogen-conflict-filter-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("create fixture root");
        root
    }
}

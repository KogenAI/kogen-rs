use super::commit::{commit_identity, signing_config, validate_identity};
use super::error::LandingError;
use super::gitops::{arg, args, git, git_with_config, path_arg};
use super::rebase;
use super::refs::{base_ref, check_commit, incoming_ref, validate_run_id};
use super::worktree;
use crate::gate::snapshot_tree;
use crate::git::GitRepo;
use std::fs;
use std::path::{Path, PathBuf};

const CLAIM_REF: &str = "refs/kogen/claim";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateCommit {
    pub commit: String,
    pub parent: String,
    pub tree: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebaseAttempt {
    Clean,
    Conflict { paths: Vec<String>, detail: String },
    Impossible { detail: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeUpdate {
    pub path: PathBuf,
    pub dirty: bool,
    pub updated: bool,
}

#[derive(Clone, Debug)]
pub struct LandingRepository {
    workspace: PathBuf,
    origin: PathBuf,
}

impl LandingRepository {
    /// Create a private detached clone using the §5.4 clone flags.
    pub fn clone_fresh(
        origin: impl AsRef<Path>,
        workspace: impl AsRef<Path>,
        base_commit: &str,
    ) -> Result<Self, LandingError> {
        check_commit(base_commit)?;
        let origin = fs::canonicalize(origin.as_ref())
            .map_err(|error| LandingError::io("resolve landing origin", origin.as_ref(), error))?;
        let workspace = workspace.as_ref().to_path_buf();
        if workspace.exists() {
            return Err(LandingError::invalid(
                "clone landing workspace",
                format!("workspace already exists: {}", workspace.display()),
            ));
        }
        if let Some(parent) = workspace.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LandingError::io("create workspace parent", parent, error))?;
        }
        let cwd = workspace.parent().unwrap_or_else(|| Path::new("."));
        let clone_args = [
            arg("clone"),
            arg("--local"),
            arg("--no-hardlinks"),
            arg("--no-checkout"),
            arg("--template="),
            arg("--"),
            path_arg(&origin),
            path_arg(&workspace),
        ];
        if let Err(error) = git(cwd, &clone_args, None, &[]) {
            let _ = fs::remove_dir_all(&workspace);
            return Err(error);
        }
        let repo = Self { workspace, origin };
        let checkout = [
            arg("checkout"),
            arg("--detach"),
            arg("--force"),
            arg(base_commit),
        ];
        if let Err(error) = git(&repo.workspace, &checkout, None, &[]) {
            let _ = fs::remove_dir_all(&repo.workspace);
            return Err(error);
        }
        Ok(repo)
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    #[must_use]
    pub fn origin(&self) -> &Path {
        &self.origin
    }

    pub fn current_base(&self, branch: &str) -> Result<String, LandingError> {
        let reference = base_ref(branch)?;
        GitRepo::new(&self.origin)
            .resolve_commit(&reference)
            .map_err(Into::into)
    }

    pub fn verify_claim(&self, run_id: &str) -> Result<(), LandingError> {
        validate_run_id(run_id)?;
        let origin = GitRepo::new(&self.origin);
        let Some(commit) = origin.ref_target(CLAIM_REF)? else {
            return Err(LandingError::invalid(
                "verify origin claim",
                "origin claim is absent",
            ));
        };
        let owner = origin
            .blob_at(&commit, ".kogen/claim")?
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned());
        if owner.as_deref() == Some(run_id) {
            Ok(())
        } else {
            Err(LandingError::invalid(
                "verify origin claim",
                "origin claim is not owned by this run",
            ))
        }
    }

    pub fn release_claim(&self, run_id: &str) -> Result<bool, LandingError> {
        validate_run_id(run_id)?;
        let origin = GitRepo::new(&self.origin);
        let Some(commit) = origin.ref_target(CLAIM_REF)? else {
            return Ok(false);
        };
        let owner = origin
            .blob_at(&commit, ".kogen/claim")?
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned());
        if owner.as_deref() != Some(run_id) {
            return Ok(false);
        }
        origin
            .delete_ref_cas(CLAIM_REF, &commit)
            .map_err(Into::into)
    }

    pub fn candidate_commit(
        &self,
        expected_parent: &str,
        verified_tree: &str,
        title: &str,
        slug: &str,
    ) -> Result<CandidateCommit, LandingError> {
        validate_identity(title, slug)?;
        check_commit(expected_parent)?;
        let actual_tree = snapshot_tree(&self.workspace)?;
        if actual_tree != verified_tree {
            return Err(LandingError::controller(
                "verify landing tree",
                format!("tree_mismatch: verified {verified_tree}, found {actual_tree}"),
            ));
        }
        let message = format!("{title}\n\nKogen-Intent: {slug}\n");
        let identity = commit_identity(&self.origin)?;
        let sign = signing_config(&self.origin);
        let config = sign.config;
        let mut commit_args = args(&["commit-tree", verified_tree, "-p", expected_parent]);
        if sign.enabled {
            commit_args.push(arg("-S"));
        }
        let commit = String::from_utf8_lossy(&git_with_config(
            &self.workspace,
            &config,
            &commit_args,
            Some(message.as_bytes()),
            &identity,
        )?)
        .trim()
        .to_owned();
        self.verify_candidate(&commit, expected_parent, verified_tree)?;
        Ok(CandidateCommit {
            commit,
            parent: expected_parent.to_owned(),
            tree: verified_tree.to_owned(),
        })
    }

    pub fn verify_candidate(
        &self,
        commit: &str,
        expected_parent: &str,
        expected_tree: &str,
    ) -> Result<(), LandingError> {
        check_commit(commit)?;
        check_commit(expected_parent)?;
        check_commit(expected_tree)?;
        let repo = GitRepo::new(&self.workspace);
        let parents = repo.text(&["show", "-s", "--format=%P", commit])?;
        let tree = repo.resolve_tree(commit)?;
        if parents != expected_parent {
            return Err(LandingError::controller(
                "verify landing parent",
                format!(
                    "not_fast_forward: expected one parent {expected_parent}, found {parents:?}"
                ),
            ));
        }
        if tree != expected_tree {
            return Err(LandingError::controller(
                "verify landing tree",
                format!("tree_mismatch: expected {expected_tree}, found {tree}"),
            ));
        }
        Ok(())
    }

    pub fn push_incoming(&self, run_id: &str, commit: &str) -> Result<(), LandingError> {
        validate_run_id(run_id)?;
        let incoming = incoming_ref(run_id);
        if GitRepo::new(&self.origin).ref_target(&incoming)?.is_some() {
            return Err(LandingError::invalid(
                "create incoming ref",
                format!("{incoming} already exists"),
            ));
        }
        let lease = format!("--force-with-lease={incoming}:");
        let source = format!("{commit}:{incoming}");
        git(
            &self.workspace,
            &args(&[
                "push",
                "--porcelain",
                "--no-recurse-submodules",
                &lease,
                "origin",
                &source,
            ]),
            None,
            &[],
        )?;
        Ok(())
    }

    pub fn base_locked(&self, branch: &str) -> Result<bool, LandingError> {
        let reference = base_ref(branch)?;
        let lock = GitRepo::new(&self.origin).text(&[
            "rev-parse",
            "--git-path",
            &format!("{reference}.lock"),
        ])?;
        let lock_path = PathBuf::from(lock);
        let lock_path = if lock_path.is_absolute() {
            lock_path
        } else {
            self.origin.join(lock_path)
        };
        Ok(lock_path.exists())
    }

    pub fn cas_base(
        &self,
        branch: &str,
        commit: &str,
        expected_parent: &str,
    ) -> Result<bool, LandingError> {
        let reference = base_ref(branch)?;
        GitRepo::new(&self.origin)
            .cas_ref(&reference, commit, Some(expected_parent))
            .map_err(Into::into)
    }

    pub fn delete_incoming(&self, run_id: &str, expected: &str) -> Result<bool, LandingError> {
        validate_run_id(run_id)?;
        GitRepo::new(&self.origin)
            .delete_ref_cas(&incoming_ref(run_id), expected)
            .map_err(Into::into)
    }

    pub fn park(&self, run_id: &str, commit: &str) -> Result<(), LandingError> {
        validate_run_id(run_id)?;
        let parked = format!("refs/kogen/parked/{run_id}");
        if !GitRepo::new(&self.origin).cas_ref(&parked, commit, None)? {
            return Err(LandingError::invalid(
                "publish parked candidate",
                "parked ref already exists",
            ));
        }
        Ok(())
    }

    pub fn rebase_candidate(
        &self,
        candidate: &CandidateCommit,
        branch: &str,
        new_parent: &str,
    ) -> Result<RebaseAttempt, LandingError> {
        rebase::rebase_candidate(self, candidate, branch, new_parent)
    }

    pub fn finish_rebase(&self) -> Result<(), LandingError> {
        rebase::finish_rebase(self)
    }

    pub fn inspect_checked_out(&self, branch: &str) -> Result<Vec<WorktreeUpdate>, LandingError> {
        worktree::inspect_checked_out(&self.origin, branch)
    }

    pub fn update_checked_out(
        &self,
        checked_out: &[WorktreeUpdate],
        commit: &str,
        expected_parent: &str,
        branch: &str,
    ) -> Vec<WorktreeUpdate> {
        worktree::update_checked_out(checked_out, commit, expected_parent, branch)
    }

    pub fn cleanup_workspace(&self) -> Result<(), LandingError> {
        fs::remove_dir_all(&self.workspace)
            .map_err(|error| LandingError::io("remove landing workspace", &self.workspace, error))
    }
}

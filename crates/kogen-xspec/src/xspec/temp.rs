mod approval;

use kogen_core::git::{GitError, GitRepo};
use kogen_core::project::{ProjectConfig, ProjectOptions, ProjectResolution};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);
static NEXT_RUN_STATUS: AtomicU64 = AtomicU64::new(0);
static NEXT_UNESTABLISHED_CACHE: AtomicU64 = AtomicU64::new(0);

pub(super) struct TempProject {
    root: PathBuf,
    checkout: PathBuf,
    origin_path: PathBuf,
    home: PathBuf,
    base_sha: String,
    resolution: Option<ProjectResolution>,
    pub(super) origin: GitRepo,
    pub(super) checkout_repo: GitRepo,
}

#[derive(Clone, Debug)]
pub(super) struct SourceBytes {
    pub intent: Vec<u8>,
    pub acceptance: Vec<u8>,
}

impl TempProject {
    #[cfg(test)]
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn new() -> Result<Self, String> {
        let id = NEXT_PROJECT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("kogen-xspec-{}-{id}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        }
        let checkout = root.join("checkout");
        let origin_path = root.join("origin.git");
        let home = root.join("home");
        fs::create_dir_all(&checkout).map_err(|error| error.to_string())?;
        fs::create_dir_all(&origin_path).map_err(|error| error.to_string())?;
        fs::create_dir_all(&home).map_err(|error| error.to_string())?;
        let mut project = Self {
            root,
            checkout,
            origin: GitRepo::new(&origin_path),
            origin_path,
            home,
            base_sha: String::new(),
            resolution: None,
            checkout_repo: GitRepo::new("."),
        };
        project.checkout_repo = GitRepo::new(&project.checkout);
        project.initialize_git()?;
        project.seed_project()?;
        project.resolution = Some(
            ProjectResolution::resolve(&ProjectOptions {
                project: Some(project.checkout.clone()),
                origin: Some(project.origin_path.clone()),
                base: Some("main".to_owned()),
                home: Some(project.home.clone()),
                ..ProjectOptions::default()
            })
            .map_err(|error| error.to_string())?,
        );
        Ok(project)
    }

    pub(super) fn reset(&self) -> Result<(), String> {
        self.checkout_repo
            .output(&["reset", "--hard", &self.base_sha])
            .map_err(|error| error.to_string())?;
        self.checkout_repo
            .output(&["clean", "-fdx"])
            .map_err(|error| error.to_string())?;
        let current_base = self
            .origin
            .ref_target("refs/heads/main")
            .map_err(|error| error.to_string())?;
        if current_base.as_deref() != Some(self.base_sha.as_str()) {
            if let Some(current) = current_base.as_deref() {
                if !self
                    .origin
                    .cas_ref("refs/heads/main", &self.base_sha, Some(current))
                    .map_err(|error| error.to_string())?
                {
                    return Err("base ref changed while resetting the fixture".to_owned());
                }
            } else if !self
                .origin
                .cas_ref("refs/heads/main", &self.base_sha, None)
                .map_err(|error| error.to_string())?
            {
                return Err("base ref changed while resetting the fixture".to_owned());
            }
        }
        for slug in ["alpha", "bravo"] {
            let ref_name = format!("refs/kogen/intents/{slug}");
            if let Some(expected) = self
                .origin
                .ref_target(&ref_name)
                .map_err(|error| error.to_string())?
                && !self
                    .origin
                    .delete_ref_cas(&ref_name, &expected)
                    .map_err(|error| error.to_string())?
            {
                return Err("approval ref changed while resetting the fixture".to_owned());
            }
            let witness_ref = format!("refs/kogen/witness/{slug}");
            if let Some(expected) = self
                .origin
                .ref_target(&witness_ref)
                .map_err(|error| error.to_string())?
                && !self
                    .origin
                    .delete_ref_cas(&witness_ref, &expected)
                    .map_err(|error| error.to_string())?
            {
                return Err("witness ref changed while resetting the fixture".to_owned());
            }
        }
        if let Some(claim) = self
            .origin
            .ref_target("refs/kogen/claim")
            .map_err(|error| error.to_string())?
            && !self
                .origin
                .delete_ref_cas("refs/kogen/claim", &claim)
                .map_err(|error| error.to_string())?
        {
            return Err("claim ref changed while resetting the fixture".to_owned());
        }
        fs::remove_dir_all(&self.home).map_err(|error| error.to_string())?;
        fs::create_dir_all(&self.home).map_err(|error| error.to_string())?;
        self.write_control_files("tree-b0", "k1", "ok", "green", "green")?;
        Ok(())
    }

    fn initialize_git(&mut self) -> Result<(), String> {
        run_git(
            &self.root,
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                path_arg(&self.origin_path)?,
            ],
        )?;
        run_git(&self.checkout, &["init", "--initial-branch=main"])?;
        for repo in [&self.checkout, &self.origin_path] {
            kogen_test_support::set_identity(repo, "Ann", "ann@x.io")?;
        }
        Ok(())
    }

    fn seed_project(&mut self) -> Result<(), String> {
        let config = self.checkout.join(".kogen/project.yaml");
        create_parent(&config)?;
        fs::write(
            &config,
            b"name: xspec\nbase: main\nchecks:\n  - name: baseline\n    argv: [sh, .kogen/xspec/baseline.sh]\n    timeout_ms: 1000\nacceptance_checks:\n  - name: acceptance\n    argv: [sh, .kogen/xspec/acceptance.sh, '{path}']\n    timeout_ms: 1000\nsetup:\n  - name: setup\n    argv: [sh, .kogen/xspec/setup.sh]\n    timeout_ms: 1000\nsetup_inputs: [.kogen/xspec/cache-key]\n",
        )
        .map_err(|error| error.to_string())?;
        run_git(&self.checkout, &["add", "--", ".kogen/project.yaml"])?;
        run_git(&self.checkout, &["commit", "--quiet", "-m", "xspec base"])?;
        run_git(
            &self.origin_path,
            &[
                "fetch",
                "--quiet",
                path_arg(&self.checkout)?,
                "refs/heads/main:refs/heads/main",
            ],
        )?;
        self.base_sha = self
            .checkout_repo
            .resolve_commit("HEAD")
            .map_err(|error| error.to_string())?;
        self.write_control_files("tree-b0", "k1", "ok", "green", "green")?;
        Ok(())
    }

    pub(super) fn write_sources(&self, slug: &str, bytes: &SourceBytes) -> Result<(), String> {
        safe_slug(slug)?;
        let intent = self
            .checkout
            .join(format!(".kogen/intents/{slug}/intent.md"));
        let acceptance = self.checkout.join(format!(".kogen/acceptance/{slug}.t.sh"));
        create_parent(&intent)?;
        create_parent(&acceptance)?;
        fs::write(intent, &bytes.intent).map_err(|error| error.to_string())?;
        fs::write(acceptance, &bytes.acceptance).map_err(|error| error.to_string())
    }

    pub(super) fn reset_sources(&self, slug: &str) -> Result<SourceBytes, String> {
        let bytes = default_sources(slug);
        self.write_sources(slug, &bytes)?;
        Ok(bytes)
    }

    pub(super) fn read_sources(&self, slug: &str) -> Result<SourceBytes, String> {
        safe_slug(slug)?;
        let intent = fs::read(
            self.checkout
                .join(format!(".kogen/intents/{slug}/intent.md")),
        )
        .map_err(|error| error.to_string())?;
        let acceptance = fs::read(self.checkout.join(format!(".kogen/acceptance/{slug}.t.sh")))
            .map_err(|error| error.to_string())?;
        Ok(SourceBytes { intent, acceptance })
    }

    pub(super) fn read_intent(&self, slug: &str) -> Result<Vec<u8>, String> {
        safe_slug(slug)?;
        fs::read(
            self.checkout
                .join(format!(".kogen/intents/{slug}/intent.md")),
        )
        .map_err(|error| error.to_string())
    }

    pub(super) fn read_acceptance(&self, slug: &str) -> Result<Vec<u8>, String> {
        safe_slug(slug)?;
        fs::read(self.checkout.join(format!(".kogen/acceptance/{slug}.t.sh")))
            .map_err(|error| error.to_string())
    }

    pub(super) fn acceptance_source_exists(&self, slug: &str) -> Result<bool, String> {
        safe_slug(slug)?;
        Ok(self
            .checkout
            .join(format!(".kogen/acceptance/{slug}.t.sh"))
            .is_file())
    }

    pub(super) fn intent_source_exists(&self, slug: &str) -> Result<bool, String> {
        safe_slug(slug)?;
        Ok(self
            .checkout
            .join(format!(".kogen/intents/{slug}/intent.md"))
            .is_file())
    }

    pub(super) fn remove_acceptance_source(&self, slug: &str) -> Result<(), String> {
        safe_slug(slug)?;
        match fs::remove_file(self.checkout.join(format!(".kogen/acceptance/{slug}.t.sh"))) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(super) fn base_commit(&self) -> Result<String, GitError> {
        self.origin.resolve_commit("refs/heads/main")
    }

    pub(super) fn resolve(&self) -> Result<ProjectResolution, String> {
        let mut project = self
            .resolution
            .clone()
            .ok_or_else(|| "temporary project resolution is not initialized".to_owned())?;
        let config_path = self.checkout.join(".kogen/project.yaml");
        let mut source = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
        // Baseline checks run on a fresh clone of the origin base. Keep the
        // symbolic control scripts available there without copying draft
        // source files into that base, and bind their current cache identity.
        for script in ["setup.sh", "baseline.sh", "acceptance.sh"] {
            source = source.replace(
                &format!(".kogen/xspec/{script}"),
                &self
                    .checkout
                    .join(".kogen/xspec")
                    .join(script)
                    .to_string_lossy(),
            );
        }
        let identity = fs::read(self.checkout.join(".kogen/xspec/cache-key"))
            .map_err(|error| error.to_string())?;
        let identity_hex = identity
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        source.push_str(&format!("env:\n  XSPEC_CACHE_KEY: '{identity_hex}'\n"));
        project.config = Some(
            ProjectConfig::from_bytes(&config_path, source.as_bytes())
                .map_err(|error| error.to_string())?,
        );
        Ok(project)
    }

    pub(super) fn write_control_files(
        &self,
        base_tree: &str,
        cache_key: &str,
        setup: &str,
        baseline: &str,
        acceptance: &str,
    ) -> Result<(), String> {
        let directory = self.checkout.join(".kogen/xspec");
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let cache_identity = if base_tree.is_empty() || cache_key.is_empty() {
            format!(
                "unestablished-{}",
                NEXT_UNESTABLISHED_CACHE.fetch_add(1, Ordering::Relaxed)
            )
        } else {
            format!("{base_tree}\0{cache_key}")
        };
        fs::write(directory.join("cache-key"), cache_identity.as_bytes())
            .map_err(|error| error.to_string())?;
        fs::write(directory.join("setup.sh"), script_for(setup, "setup"))
            .map_err(|error| error.to_string())?;
        fs::write(
            directory.join("baseline.sh"),
            script_for(baseline, "baseline"),
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            directory.join("acceptance.sh"),
            script_for(acceptance, "acceptance"),
        )
        .map_err(|error| error.to_string())?;
        let _ = fs::remove_file(directory.join("mutated"));
        Ok(())
    }

    pub(super) fn set_witness_fixture(
        &self,
        enabled: bool,
        slug: &str,
        actual_hash: &str,
        feasibility: &str,
    ) -> Result<(), String> {
        let config = self.checkout.join(".kogen/project.yaml");
        let base = "name: xspec\nbase: main\nchecks:\n  - name: baseline\n    argv: [sh, .kogen/xspec/baseline.sh]\n    timeout_ms: 1000\nacceptance_checks:\n  - name: acceptance\n    argv: [sh, .kogen/xspec/acceptance.sh, '{path}']\n    timeout_ms: 1000\nsetup:\n  - name: setup\n    argv: [sh, .kogen/xspec/setup.sh]\n    timeout_ms: 1000\nsetup_inputs: [.kogen/xspec/cache-key]\n";
        let config_bytes = if enabled {
            format!("{base}shaping:\n  proof: witness\n")
        } else {
            base.to_owned()
        };
        fs::write(config, config_bytes).map_err(|error| error.to_string())?;

        let ref_name = format!("refs/kogen/witness/{slug}");
        if let Some(old) = self
            .origin
            .ref_target(&ref_name)
            .map_err(|error| error.to_string())?
            && !self
                .origin
                .delete_ref_cas(&ref_name, &old)
                .map_err(|error| error.to_string())?
        {
            return Err("witness ref changed while resetting the fixture".to_owned());
        }
        if enabled && matches!(feasibility, "PROVEN" | "PROVEN with concerns") {
            let base = self.base_commit().map_err(|error| error.to_string())?;
            let witness = self
                .origin
                .create_descendant_commit(&base, "xspec witness\n")
                .map_err(|error| error.to_string())?;
            if !self
                .origin
                .cas_ref(&ref_name, &witness, None)
                .map_err(|error| error.to_string())?
            {
                return Err("witness ref already exists during fixture setup".to_owned());
            }
        }
        let warnings_path = self
            .checkout
            .join(format!(".kogen/intents/{slug}/shape-warnings.json"));
        if feasibility == "PROVEN with concerns" {
            create_parent(&warnings_path)?;
            let warning = serde_json::json!({
                "approval_sha256": actual_hash,
                "warnings": [{
                    "code": "feasibility_concern",
                    "item_ids": ["A1"],
                    "message": "xspec witness concern fixture"
                }]
            });
            fs::write(
                warnings_path,
                serde_json::to_vec(&warning).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        } else if let Err(error) = fs::remove_file(warnings_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.to_string());
        }
        Ok(())
    }

    pub(super) fn approval_cache_count(&self) -> Result<usize, String> {
        let directory = self
            .resolution
            .as_ref()
            .ok_or_else(|| "temporary project resolution is not initialized".to_owned())?
            .state_root
            .join("approval-cache");
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.to_string()),
        };
        let mut count = 0;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                count += 1;
            }
        }
        Ok(count)
    }

    pub(super) fn adopted_status(
        &self,
        slug: &str,
        approval_commit: &str,
    ) -> Result<Option<String>, String> {
        let runs = self
            .resolution
            .as_ref()
            .ok_or_else(|| "temporary project resolution is not initialized".to_owned())?
            .state_root
            .join("runs");
        let entries = match fs::read_dir(runs) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut current: Option<kogen_core::run::RunSnapshot> = None;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            let Ok(snapshot) = kogen_core::run::RunStore::new(path).read_snapshot() else {
                continue;
            };
            if snapshot.slug != slug || snapshot.approval_commit != approval_commit {
                continue;
            }
            if current
                .as_ref()
                .is_none_or(|old| old.started_ms <= snapshot.started_ms)
            {
                current = Some(snapshot);
            }
        }
        Ok(current.map(|snapshot| {
            if snapshot.status == "running" {
                "building".to_owned()
            } else {
                snapshot.status
            }
        }))
    }

    pub(super) fn set_identity(&self, ident: &str) -> Result<(), String> {
        if ident.is_empty() {
            self.checkout_repo
                .output(&["config", "--local", "user.name", ""])
                .map_err(|error| error.to_string())?;
            self.checkout_repo
                .output(&["config", "--local", "user.email", ""])
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
        let Some((name, email)) = ident
            .split_once('<')
            .and_then(|(name, rest)| rest.strip_suffix('>').map(|email| (name.trim(), email)))
        else {
            return Err(format!("invalid fixture git identity {ident:?}"));
        };
        self.checkout_repo
            .output(&["config", "--local", "user.name", name])
            .map_err(|error| error.to_string())?;
        self.checkout_repo
            .output(&["config", "--local", "user.email", email])
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) fn adopt_run_status(&self, slug: &str, status: &str) -> Result<(), String> {
        safe_slug(slug)?;
        let project = self.resolve()?;
        let reference = self
            .origin
            .ref_target(&format!("refs/kogen/intents/{slug}"))
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "cannot report a Build without an approval ref".to_owned())?;
        let summary = self
            .approval_summaries(&[slug])?
            .remove(slug)
            .ok_or_else(|| "approval disappeared while adopting Build status".to_owned())?;
        let sequence = NEXT_RUN_STATUS.fetch_add(1, Ordering::Relaxed);
        let id = format!("xspec-{}-{}-{sequence}", slug, status);
        let snapshot = kogen_core::run::RunSnapshot {
            schema: 2,
            run_id: id.clone(),
            slug: slug.to_owned(),
            approval_sha256: summary.sha,
            approval_commit: reference.clone(),
            target_branch: "main".to_owned(),
            status: if status == "building" {
                "running".to_owned()
            } else {
                status.to_owned()
            },
            landing: None,
            owner_pid: std::process::id(),
            owner_started_ms: 1,
            started_ms: i64::try_from(sequence).unwrap_or(i64::MAX),
            recovery: Vec::new(),
            cleanup_pending: false,
            fields: Default::default(),
        };
        let directory = project.state_root.join("runs").join(&id);
        kogen_core::run::RunStore::new(&directory)
            .create(&snapshot)
            .map_err(|error| error.to_string())?;
        if status == "building" {
            let claim_file = self.checkout.join(".kogen/claim");
            create_parent(&claim_file)?;
            fs::write(&claim_file, format!("{id}\n")).map_err(|error| error.to_string())?;
            run_git(&self.checkout, &["add", "--", ".kogen/claim"])?;
            run_git(&self.checkout, &["commit", "--quiet", "-m", "xspec claim"])?;
            let target = self
                .checkout_repo
                .resolve_commit("HEAD")
                .map_err(|error| error.to_string())?;
            run_git(
                &self.origin_path,
                &["fetch", "--quiet", path_arg(&self.checkout)?, &target],
            )?;
            if !self
                .origin
                .cas_ref("refs/kogen/claim", &target, None)
                .map_err(|error| error.to_string())?
            {
                return Err("claim ref already exists during Build adoption".to_owned());
            }
        } else if status == "landed" {
            if let Some(claim) = self
                .origin
                .ref_target("refs/kogen/claim")
                .map_err(|error| error.to_string())?
                && !self
                    .origin
                    .delete_ref_cas("refs/kogen/claim", &claim)
                    .map_err(|error| error.to_string())?
            {
                return Err("claim ref changed while adopting landed Build".to_owned());
            }
            let target = self
                .checkout_repo
                .resolve_commit("HEAD")
                .map_err(|error| error.to_string())?;
            run_git(
                &self.origin_path,
                &["fetch", "--quiet", path_arg(&self.checkout)?, &target],
            )?;
            let old = self.base_commit().map_err(|error| error.to_string())?;
            if !self
                .origin
                .cas_ref("refs/heads/main", &target, Some(&old))
                .map_err(|error| error.to_string())?
            {
                return Err("base ref changed while adopting landed Build".to_owned());
            }
        } else if let Some(claim) = self
            .origin
            .ref_target("refs/kogen/claim")
            .map_err(|error| error.to_string())?
            && !self
                .origin
                .delete_ref_cas("refs/kogen/claim", &claim)
                .map_err(|error| error.to_string())?
        {
            return Err("claim ref changed while adopting Build outcome".to_owned());
        }
        Ok(())
    }

    pub(super) fn persist_sources(&self, slug: &str) -> Result<(), String> {
        safe_slug(slug)?;
        // Fixture commits are infrastructure. A preceding approval event may
        // deliberately clear the checkout identity to exercise production's
        // identity-unavailable branch, so restore the isolated fixture author
        // before committing source setup for the next event.
        self.set_identity("Ann <ann@x.io>")?;
        run_git(
            &self.checkout,
            &[
                "add",
                "--",
                &format!(".kogen/intents/{slug}/intent.md"),
                &format!(".kogen/acceptance/{slug}.t.sh"),
            ],
        )?;
        if self
            .checkout_repo
            .output(&["diff", "--cached", "--quiet"])
            .is_ok()
        {
            return Ok(());
        }
        run_git(
            &self.checkout,
            &["commit", "--quiet", "-m", &format!("Shape Intent {slug}")],
        )?;
        Ok(())
    }

    pub(super) fn advance_approval_ref(&self, slug: &str) -> Result<(), String> {
        let ref_name = format!("refs/kogen/intents/{slug}");
        let old = self
            .origin
            .ref_target(&ref_name)
            .map_err(|error| error.to_string())?;
        let parent = match old.as_deref() {
            Some(parent) => parent.to_owned(),
            None => self.base_commit().map_err(|error| error.to_string())?,
        };
        let marker = self
            .origin
            .create_descendant_commit(&parent, "xspec CAS competitor\n")
            .map_err(|error| error.to_string())?;
        if !self
            .origin
            .cas_ref(&ref_name, &marker, old.as_deref())
            .map_err(|error| error.to_string())?
        {
            return Err("injected CAS competitor lost its own ref update".to_owned());
        }
        Ok(())
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn default_sources(slug: &str) -> SourceBytes {
    SourceBytes {
        intent: format!(
            "---\ntitle: {slug}\nsize: small\ndomains:\n  - platform\nchanges_gate: true\n---\nA concise intent fixture.\n\n## Acceptance\n- A1: The source bytes are bound to approval.\n\n## Verify\n- A1: test\n"
        )
        .into_bytes(),
        acceptance: b"#!/bin/sh\nexit 0\n".to_vec(),
    }
}

fn script_for(outcome: &str, kind: &str) -> String {
    match (kind, outcome) {
        ("setup", "failed") => "#!/bin/sh\necho 'xspec setup failed'\nexit 1\n".to_owned(),
        ("baseline", "red") => "#!/bin/sh\necho 'xspec baseline red'\nexit 1\n".to_owned(),
        ("baseline", "unavailable") => "#!/bin/sh\nexit 127\n".to_owned(),
        ("baseline", "timeout") | ("acceptance", "timeout") => {
            "#!/bin/sh\nsleep 2\nexit 0\n".to_owned()
        }
        ("baseline", "mutating") => {
            "#!/bin/sh\nprintf x > .kogen/xspec/mutated\nexit 0\n".to_owned()
        }
        ("acceptance", "tool_missing") => "#!/bin/sh\nexit 127\n".to_owned(),
        ("acceptance", "red") => "#!/bin/sh\necho 'xspec acceptance red'\nexit 1\n".to_owned(),
        (_, _) => "#!/bin/sh\nexit 0\n".to_owned(),
    }
}

pub(super) fn malformed_intent() -> Vec<u8> {
    b"---\ntitle: missing closing fence\n".to_vec()
}

pub(super) fn lint_invalid_intent(slug: &str) -> Vec<u8> {
    format!(
        "---\ntitle: {slug}\nsize: small\ndomains:\n  - platform\nchanges_gate: true\n---\n\n## Acceptance\n- A1: The source bytes are bound to approval.\n\n## Verify\n- A1: test keep\n"
    )
    .into_bytes()
}

pub(super) fn lint_warning_intent(slug: &str) -> Vec<u8> {
    format!(
        "---\ntitle: {slug}\nsize: small\ndomains:\n  - platform\nchanges_gate: true\n---\nA concise intent fixture.\n\n## Acceptance\n- A1: This acceptance statement contains a deliberately long sequence of plain words so the lint engine reports a style warning for this otherwise valid fixture with enough words here today.\n\n## Verify\n- A1: test\n"
    )
    .into_bytes()
}

pub(super) fn is_valid_slug(slug: &str) -> bool {
    kogen_core::project::valid_slug(slug)
}

fn safe_slug(slug: &str) -> Result<(), String> {
    if is_valid_slug(slug) {
        Ok(())
    } else {
        Err("fixture slug is not safe for a temporary path".to_owned())
    }
}

fn create_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "temporary source path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())
}

fn path_arg(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "temporary Git path is not UTF-8".to_owned())
}

fn run_git(directory: &Path, args: &[&str]) -> Result<String, String> {
    let output = kogen_test_support::git_command()
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("start git {}: {error}", args.first().copied().unwrap_or("")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

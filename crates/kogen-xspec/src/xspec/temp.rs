mod approval;

pub(super) use approval::ApprovalSummary;

use kogen_core::git::{GitError, GitRepo};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);

pub(super) struct TempProject {
    root: PathBuf,
    checkout: PathBuf,
    origin_path: PathBuf,
    base_sha: String,
    pub(super) origin: GitRepo,
    pub(super) checkout_repo: GitRepo,
}

#[derive(Clone, Debug)]
pub(super) struct SourceBytes {
    pub intent: Vec<u8>,
    pub acceptance: Vec<u8>,
}

impl TempProject {
    pub(super) fn new() -> Result<Self, String> {
        let id = NEXT_PROJECT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("kogen-xspec-{}-{id}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        }
        let checkout = root.join("checkout");
        let origin_path = root.join("origin.git");
        fs::create_dir_all(&checkout).map_err(|error| error.to_string())?;
        fs::create_dir_all(&origin_path).map_err(|error| error.to_string())?;
        let mut project = Self {
            root,
            checkout,
            origin: GitRepo::new(&origin_path),
            origin_path,
            base_sha: String::new(),
            checkout_repo: GitRepo::new("."),
        };
        project.checkout_repo = GitRepo::new(&project.checkout);
        project.initialize_git()?;
        project.seed_sources()?;
        Ok(project)
    }

    pub(super) fn reset(&self) -> Result<(), String> {
        self.checkout_repo
            .output(&["reset", "--hard", &self.base_sha])
            .map_err(|error| error.to_string())?;
        self.checkout_repo
            .output(&["clean", "-fdx"])
            .map_err(|error| error.to_string())?;
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
        }
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
            run_git(repo, &["config", "user.name", "Ann"])?;
            run_git(repo, &["config", "user.email", "ann@x.io"])?;
        }
        Ok(())
    }

    fn seed_sources(&mut self) -> Result<(), String> {
        for slug in ["alpha", "bravo"] {
            self.write_sources(slug, &default_sources(slug))?;
        }
        run_git(&self.checkout, &["add", "-A"])?;
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

    pub(super) fn remove_tracked_sources(&self, slug: &str) -> Result<String, String> {
        safe_slug(slug)?;
        let paths = vec![
            format!(".kogen/intents/{slug}/intent.md"),
            format!(".kogen/acceptance/{slug}.t.sh"),
        ];
        let (commit, _) = self
            .checkout_repo
            .remove_paths_commit(&paths, &format!("Remove Intent {slug}\n"))
            .map_err(|error| error.to_string())?;
        for path in &paths {
            let path = self.checkout.join(path);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
            if let Some(parent) = path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
        Ok(commit)
    }

    pub(super) fn persist_sources(&self, slug: &str) -> Result<(), String> {
        safe_slug(slug)?;
        run_git(
            &self.checkout,
            &[
                "add",
                "--",
                &format!(".kogen/intents/{slug}/intent.md"),
                &format!(".kogen/acceptance/{slug}.t.sh"),
            ],
        )?;
        run_git(
            &self.checkout,
            &["commit", "--quiet", "-m", &format!("Shape Intent {slug}")],
        )?;
        Ok(())
    }

    pub(super) fn remove_approval_ref(&self, slug: &str) -> Result<(), String> {
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
            return Err("approval ref changed during removal".to_owned());
        }
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
            "---\ntitle: {slug}\nsize: small\ndomains:\n  - platform\n---\nA concise intent fixture.\n\n## Acceptance\n- A1: The source bytes are bound to approval.\n\n## Verify\n- A1: test\n"
        )
        .into_bytes(),
        acceptance: b"#!/bin/sh\nexit 0\n".to_vec(),
    }
}

pub(super) fn malformed_intent() -> Vec<u8> {
    b"---\ntitle: missing closing fence\n".to_vec()
}

pub(super) fn lint_invalid_intent(slug: &str) -> Vec<u8> {
    format!(
        "---\ntitle: {slug}\nsize: small\ndomains:\n  - platform\n---\n\n## Acceptance\n- A1: The source bytes are bound to approval.\n\n## Verify\n- A1: test keep\n"
    )
    .into_bytes()
}

pub(super) fn lint_warning_intent(slug: &str) -> Vec<u8> {
    format!(
        "---\ntitle: {slug}\nsize: small\ndomains:\n  - platform\n---\nA concise intent fixture.\n\n## Acceptance\n- A1: This acceptance statement contains a deliberately long sequence of plain words so the lint engine reports a style warning for this otherwise valid fixture with enough words here today.\n\n## Verify\n- A1: test\n"
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
    let output = Command::new("git")
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

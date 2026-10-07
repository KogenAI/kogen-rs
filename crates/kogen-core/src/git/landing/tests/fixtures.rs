use crate::gate::snapshot_tree;
use crate::git::GitRepo;
use crate::git::landing::{LandingRepository, LandingRequest};
use crate::run::{RunSnapshot, RunStore};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const RUN_ID: &str = "0123456789abcdef0123456789abcdef";
pub const SLUG: &str = "greet";
static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct Fixture {
    pub root: PathBuf,
    pub origin: PathBuf,
    pub seed: PathBuf,
    pub base: String,
    pub repository: LandingRepository,
    pub store: RunStore,
    pub snapshot: RunSnapshot,
}

impl Fixture {
    pub fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "kogen-landing-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("create landing fixture root");
        let origin = root.join("origin.git");
        let seed = root.join("seed");
        git(
            &root,
            &["init", "--bare", "--initial-branch=main", path(&origin)],
        );
        git(&root, &["init", "--initial-branch=main", path(&seed)]);
        kogen_test_support::set_identity(&seed, "Landing Test Identity", "landing@example.invalid")
            .expect("configure seed identity");
        fs::write(seed.join("README.md"), b"base\n").expect("write base README");
        git(&seed, &["add", "-A"]);
        git(&seed, &["commit", "-m", "base"]);
        git(&seed, &["remote", "add", "origin", path(&origin)]);
        git(&seed, &["push", "origin", "main"]);
        kogen_test_support::set_identity(
            &origin,
            "Landing Test Identity",
            "landing@example.invalid",
        )
        .expect("configure origin identity");
        git(&origin, &["config", "commit.gpgsign", "false"]);
        let base = GitRepo::new(&origin)
            .resolve_commit("refs/heads/main")
            .expect("resolve fixture base");
        let files = [(
            ".kogen/claim".to_owned(),
            format!("{RUN_ID}\n").into_bytes(),
        )]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let claim_message = format!("Kogen project claim\n\nKogen-Run: {RUN_ID}");
        let claim = GitRepo::new(&origin)
            .create_commit(&files, None, &claim_message)
            .expect("create claim commit");
        assert!(
            GitRepo::new(&origin)
                .cas_ref("refs/kogen/claim", &claim, None)
                .expect("create claim ref")
        );

        let state_root = root.join("state");
        let workspace = state_root.join(format!("{RUN_ID}-R1"));
        let repository = LandingRepository::clone_fresh(&origin, &workspace, &base)
            .expect("create detached landing workspace");
        fs::write(repository.workspace().join("README.md"), b"candidate\n")
            .expect("write candidate README");
        fs::create_dir_all(repository.workspace().join(".kogen/intents/greet"))
            .expect("create installed Intent directory");
        fs::write(
            repository
                .workspace()
                .join(".kogen/intents/greet/intent.md"),
            b"---\ntitle: Greet\n---\n\nAdd a greeting.\n",
        )
        .expect("write approved Intent");
        fs::create_dir_all(repository.workspace().join("test/acceptance"))
            .expect("create candidate acceptance directory");
        fs::write(
            repository.workspace().join("test/acceptance/greet_test.rs"),
            b"#[test] fn greet() {}\n",
        )
        .expect("write installed acceptance test");
        let run_dir = state_root.join("runs").join(RUN_ID);
        let store = RunStore::new(&run_dir);
        let snapshot = RunSnapshot {
            schema: 2,
            run_id: RUN_ID.to_owned(),
            slug: SLUG.to_owned(),
            approval_sha256: "approval".to_owned(),
            approval_commit: base.clone(),
            target_branch: "main".to_owned(),
            status: "running".to_owned(),
            landing: None,
            owner_pid: 0,
            owner_started_ms: 0,
            started_ms: 1,
            fields: BTreeMap::new(),
        };
        store.create(&snapshot).expect("write initial run snapshot");
        Self {
            root,
            origin,
            seed,
            base,
            repository,
            store,
            snapshot,
        }
    }

    pub fn verified_tree(&self) -> String {
        snapshot_tree(self.repository.workspace()).expect("snapshot fixture candidate")
    }

    pub fn request<'a>(&'a mut self, verified_tree: &'a str) -> LandingRequest<'a> {
        LandingRequest {
            repository: &self.repository,
            store: &self.store,
            snapshot: &mut self.snapshot,
            title: "Greet",
            expected_parent: &self.base,
            verified_tree,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn git(directory: &Path, args: &[&str]) -> String {
    let output = kogen_test_support::git_command()
        .args(args)
        .current_dir(directory)
        .output()
        .expect("run fixture Git command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

pub fn path(path: &Path) -> &str {
    path.to_str().expect("temporary path is UTF-8")
}

use super::super::fixtures::{Fixture, git};
use super::support::FastWait;
use crate::gate::snapshot_tree;
use crate::git::GitRepo;
use crate::git::landing::{
    IntegrationGate, IntegrationResult, LandingError, LandingObserver, LandingPoint, RebaseAttempt,
    RepairResult, land_with_observer,
};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct MoveBase {
    seed: PathBuf,
    origin: PathBuf,
    store: crate::run::RunStore,
    old_parent: String,
    moved_tip: Option<String>,
}

impl LandingObserver for MoveBase {
    fn reached(
        &mut self,
        point: LandingPoint,
        candidate: &crate::git::landing::CandidateCommit,
    ) -> Result<(), LandingError> {
        if point != LandingPoint::BeforeBaseCas {
            return Ok(());
        }
        let snapshot = self
            .store
            .read_snapshot()
            .expect("read record before CAS attempt");
        assert_eq!(
            snapshot
                .landing
                .as_ref()
                .map(|record| record.candidate_commit.as_str()),
            Some(candidate.commit.as_str())
        );
        if self.moved_tip.is_none() {
            fs::write(self.seed.join("README.md"), b"external base change\n")
                .expect("write moved base conflict");
            fs::write(self.seed.join("external.txt"), b"new base file\n")
                .expect("write external base file");
            git(&self.seed, &["add", "-A"]);
            git(&self.seed, &["commit", "-m", "external base movement"]);
            git(&self.seed, &["push", "origin", "main"]);
            self.moved_tip = Some(
                GitRepo::new(&self.origin)
                    .resolve_commit("refs/heads/main")
                    .expect("resolve moved tip"),
            );
        }
        assert_ne!(self.moved_tip.as_deref(), Some(self.old_parent.as_str()));
        Ok(())
    }
}

#[derive(Default)]
struct RepairConflict {
    calls: usize,
    paths: Vec<String>,
}

impl IntegrationGate for RepairConflict {
    fn reverify_and_repair(
        &mut self,
        workspace: &Path,
        _new_parent: &str,
        rebase: &RebaseAttempt,
        deadline: Instant,
    ) -> Result<IntegrationResult, LandingError> {
        self.calls += 1;
        assert!(deadline > Instant::now());
        let RebaseAttempt::Conflict { paths, .. } = rebase else {
            panic!("same-path base move must conflict")
        };
        self.paths = paths.clone();
        assert!(self.paths.iter().any(|path| path == "README.md"));
        fs::write(
            workspace.join("README.md"),
            b"base plus integration repair\n",
        )
        .expect("repair conflicting file");
        let tree = snapshot_tree(workspace).expect("re-gate repaired tree");
        Ok(IntegrationResult {
            rebase: crate::git::landing::RebaseKind::Conflict,
            repairs: vec![RepairResult::Green {
                verified_tree: tree,
            }],
            verified_tree: None,
        })
    }
}

#[test]
fn moved_tip_rebases_repairs_and_regates_before_a_new_parent_cas() {
    let mut fixture = Fixture::new("moved");
    let tree = fixture.verified_tree();
    let mut observer = MoveBase {
        seed: fixture.seed.clone(),
        origin: fixture.origin.clone(),
        store: fixture.store.clone(),
        old_parent: fixture.base.clone(),
        moved_tip: None,
    };
    let mut integration = RepairConflict::default();
    let mut wait = FastWait::default();
    let outcome = land_with_observer(
        fixture.request(&tree),
        &mut integration,
        &mut wait,
        &mut observer,
    )
    .expect("rebase, repair and land moved tip");
    let crate::git::landing::LandingOutcome::Landed {
        commit,
        tree,
        observation,
        ..
    } = outcome
    else {
        panic!("repaired candidate should land")
    };
    let moved_tip = observer.moved_tip.expect("observer advanced base");
    let origin = GitRepo::new(&fixture.origin);
    assert_eq!(
        origin
            .text(&["show", "-s", "--format=%P", &commit])
            .unwrap(),
        moved_tip
    );
    assert_eq!(origin.resolve_commit("refs/heads/main").unwrap(), commit);
    assert_eq!(origin.resolve_tree(&commit).unwrap(), tree);
    assert_eq!(
        origin.blob_at(&commit, "README.md").unwrap().unwrap(),
        b"base plus integration repair\n"
    );
    assert_eq!(
        origin.blob_at(&commit, "external.txt").unwrap().unwrap(),
        b"new base file\n"
    );
    assert_eq!(integration.calls, 1);
    assert_eq!(observation.repairs, 1);
    assert_eq!(
        fixture.snapshot.landing.as_ref().unwrap().expected_parent,
        moved_tip
    );
    assert_eq!(fixture.snapshot.landing.as_ref().unwrap().final_tree, tree);
    assert_eq!(
        wait.0,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4)
        ]
    );
    let retry_delays = fixture
        .store
        .read_events()
        .unwrap()
        .into_iter()
        .filter(|event| event.event == "landing_retry")
        .filter_map(|event| event.fields.get("delay_ms").and_then(Value::as_u64))
        .collect::<Vec<_>>();
    assert_eq!(retry_delays, vec![1_000, 2_000, 4_000, 0]);
}

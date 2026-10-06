use super::super::fixtures::{Fixture, RUN_ID, git, path};
use super::support::{FastWait, NeverMoved, executable};
use crate::git::GitRepo;
use crate::git::landing::{LandingError, LandingObserver, LandingPoint, land_with_observer};
use std::fs;
use std::path::PathBuf;

struct VerifyBeforeCas {
    origin: PathBuf,
    store: crate::run::RunStore,
    expected: String,
    checks: usize,
}

impl LandingObserver for VerifyBeforeCas {
    fn reached(
        &mut self,
        point: LandingPoint,
        candidate: &crate::git::landing::CandidateCommit,
    ) -> Result<(), LandingError> {
        if point == LandingPoint::RecordDurable {
            let snapshot = self.store.read_snapshot().expect("durable run snapshot");
            assert_eq!(
                snapshot
                    .landing
                    .as_ref()
                    .map(|record| record.candidate_commit.as_str()),
                Some(candidate.commit.as_str())
            );
            assert_eq!(
                GitRepo::new(&self.origin)
                    .ref_target("refs/heads/main")
                    .unwrap()
                    .as_deref(),
                Some(self.expected.as_str())
            );
        }
        if point == LandingPoint::BeforeBaseCas {
            let snapshot = self
                .store
                .read_snapshot()
                .expect("record is durable before CAS");
            let landing = snapshot.landing.expect("landing record before CAS");
            assert_eq!(landing.candidate_commit, candidate.commit);
            assert_eq!(landing.expected_parent, self.expected);
            let origin = GitRepo::new(&self.origin);
            assert_eq!(
                origin.ref_target("refs/heads/main").unwrap().as_deref(),
                Some(self.expected.as_str())
            );
            assert_eq!(
                origin
                    .ref_target(&format!("refs/kogen/incoming/{RUN_ID}"))
                    .unwrap()
                    .as_deref(),
                Some(candidate.commit.as_str())
            );
            self.checks += 1;
        }
        Ok(())
    }
}

#[test]
fn verified_tree_lands_once_with_durable_record_and_disabled_workspace_hooks_and_filters() {
    let mut fixture = Fixture::new("happy");
    let workspace = fixture.repository.workspace().to_path_buf();
    let script = fixture.root.join("filter.sh");
    let filter_marker = fixture.root.join("filter-ran");
    let hook_marker = fixture.root.join("hook-ran");
    fs::write(
        &script,
        format!("#!/bin/sh\nprintf ran > '{}'\ncat\n", path(&filter_marker)),
    )
    .expect("write filter script");
    executable(&script);
    fs::create_dir_all(workspace.join(".git/hooks")).expect("create workspace hooks");
    fs::write(
        workspace.join(".git/hooks/pre-push"),
        format!("#!/bin/sh\nprintf ran > '{}'\n", path(&hook_marker)),
    )
    .expect("write pre-push hook");
    executable(&workspace.join(".git/hooks/pre-push"));
    git(&workspace, &["config", "core.hooksPath", ".git/hooks"]);
    git(
        &workspace,
        &["config", "filter.landing.clean", path(&script)],
    );
    git(
        &workspace,
        &["config", "filter.landing.smudge", path(&script)],
    );
    fs::write(
        workspace.join(".gitattributes"),
        "filtered.txt filter=landing\n",
    )
    .expect("write filter attributes");
    fs::write(
        workspace.join("filtered.txt"),
        b"filter input stays exact\n",
    )
    .expect("write filtered candidate");
    let tree = fixture.verified_tree();
    let mut observer = VerifyBeforeCas {
        origin: fixture.origin.clone(),
        store: fixture.store.clone(),
        expected: fixture.base.clone(),
        checks: 0,
    };
    let mut integration = NeverMoved;
    let mut wait = FastWait::default();
    let outcome = land_with_observer(
        fixture.request(&tree),
        &mut integration,
        &mut wait,
        &mut observer,
    )
    .expect("land verified candidate");
    let crate::git::landing::LandingOutcome::Landed {
        commit,
        tree: landed_tree,
        warnings,
        cleanup_failures,
        observation,
    } = outcome
    else {
        panic!("candidate should land")
    };
    let origin = GitRepo::new(&fixture.origin);
    assert_eq!(origin.resolve_tree(&commit).unwrap(), tree);
    assert_eq!(landed_tree, tree);
    assert_eq!(
        origin
            .text(&["show", "-s", "--format=%P", &commit])
            .unwrap(),
        fixture.base
    );
    assert_eq!(
        origin
            .text(&["show", "-s", "--format=%B", &commit])
            .unwrap(),
        "Greet\n\nKogen-Intent: greet"
    );
    assert_eq!(
        origin
            .text(&["show", "-s", "--format=%an <%ae>", &commit])
            .unwrap(),
        "Landing Test Identity <landing@example.invalid>"
    );
    assert_eq!(
        origin.blob_at(&commit, "filtered.txt").unwrap().unwrap(),
        b"filter input stays exact\n"
    );
    assert_eq!(observer.checks, 1);
    assert!(warnings.is_empty());
    assert!(cleanup_failures.is_empty());
    assert_eq!(observation.status, "landed");
    assert!(observation.recorded);
    assert!(!observation.incoming);
    assert!(!filter_marker.exists());
    assert!(!hook_marker.exists());
    assert!(!fixture.repository.workspace().exists());
    assert_eq!(origin.ref_target("refs/kogen/claim").unwrap(), None);
    assert_eq!(
        origin
            .ref_target(&format!("refs/kogen/incoming/{RUN_ID}"))
            .unwrap(),
        None
    );
    let events = fixture.store.read_events().unwrap();
    assert_eq!(events[0].event, "landing_prepared");
    assert!(
        events
            .iter()
            .position(|event| event.event == "landing_prepared")
            < events.iter().position(|event| event.event == "base_cas")
    );
}

use super::super::fixtures::{Fixture, RUN_ID};
use super::support::{FastWait, NeverMoved};
use crate::git::GitRepo;
use crate::git::landing::{LandingError, LandingObserver, LandingPoint, land_with_observer};
use crate::project::ProjectResolution;
use crate::recovery;

struct CrashAfterCas;

impl LandingObserver for CrashAfterCas {
    fn reached(
        &mut self,
        point: LandingPoint,
        _candidate: &crate::git::landing::CandidateCommit,
    ) -> Result<(), LandingError> {
        if point == LandingPoint::BaseCasSucceeded {
            Err(LandingError::invalid("simulate crash", "after base CAS"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn crash_after_base_cas_before_cleanup_recovers_as_landed() {
    let mut fixture = Fixture::new("crash");
    let tree = fixture.verified_tree();
    let mut observer = CrashAfterCas;
    let mut integration = NeverMoved;
    let mut wait = FastWait::default();
    let error = land_with_observer(
        fixture.request(&tree),
        &mut integration,
        &mut wait,
        &mut observer,
    )
    .expect_err("fault point simulates process death after CAS");
    assert!(error.detail.contains("after base CAS"));
    let snapshot = fixture
        .store
        .read_snapshot()
        .expect("durable pre-CAS record remains");
    assert_eq!(snapshot.status, "running");
    let candidate = snapshot.landing.unwrap().candidate_commit;
    let origin = GitRepo::new(&fixture.origin);
    assert_eq!(origin.resolve_commit("refs/heads/main").unwrap(), candidate);
    assert_eq!(
        origin
            .ref_target(&format!("refs/kogen/incoming/{RUN_ID}"))
            .unwrap()
            .as_deref(),
        Some(candidate.as_str())
    );
    let project = ProjectResolution {
        checkout: fixture.seed.clone(),
        origin: fixture.origin.clone(),
        base: "refs/heads/main".to_owned(),
        state_root: fixture.root.join("state"),
        config: None,
    };
    let report = recovery::reconcile(&project).expect("recover landed run");
    assert_eq!(report.reconciled.len(), 1);
    assert_eq!(report.reconciled[0].status, "landed");
    assert_eq!(fixture.store.read_snapshot().unwrap().status, "landed");
    assert_eq!(
        fixture.store.read_events().unwrap().last().unwrap().event,
        "reconciled"
    );
    assert_eq!(origin.ref_target("refs/kogen/claim").unwrap(), None);
    assert_eq!(
        origin
            .ref_target(&format!("refs/kogen/incoming/{RUN_ID}"))
            .unwrap(),
        None
    );
    assert!(!fixture.repository.workspace().exists());
}

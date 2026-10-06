use super::super::fixtures::Fixture;
use super::support::NeverMoved;
use crate::git::GitRepo;
use crate::git::landing::{LandingOutcome, LandingWait, land};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

struct RemoveLockWait {
    lock: PathBuf,
    delays: Vec<Duration>,
}

impl LandingWait for RemoveLockWait {
    fn wait(&mut self, delay: Duration) {
        self.delays.push(delay);
        fs::remove_file(&self.lock).expect("release base lock during retry wait");
    }
}

#[test]
fn base_lock_retries_after_one_second_and_then_lands() {
    let mut fixture = Fixture::new("lock-retry");
    let lock = PathBuf::from(
        GitRepo::new(&fixture.origin)
            .text(&["rev-parse", "--git-path", "refs/heads/main.lock"])
            .expect("resolve base lock path"),
    );
    let lock = if lock.is_absolute() {
        lock
    } else {
        fixture.origin.join(lock)
    };
    fs::write(&lock, b"held\n").expect("create base lock");
    let tree = fixture.verified_tree();
    let mut integration = NeverMoved;
    let mut wait = RemoveLockWait {
        lock,
        delays: Vec::new(),
    };
    let outcome = land(fixture.request(&tree), &mut integration, &mut wait)
        .expect("retry while base lock is held");
    let LandingOutcome::Landed { commit, .. } = outcome else {
        panic!("candidate should land after lock release")
    };
    assert_eq!(wait.delays, vec![Duration::from_secs(1)]);
    assert_eq!(
        GitRepo::new(&fixture.origin)
            .resolve_commit("refs/heads/main")
            .unwrap(),
        commit
    );
    let retries = fixture
        .store
        .read_events()
        .unwrap()
        .into_iter()
        .filter(|event| event.event == "landing_retry")
        .collect::<Vec<_>>();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].fields["delay_ms"], 1_000);
}

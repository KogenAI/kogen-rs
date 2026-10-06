use crate::git::landing::{
    IntegrationGate, IntegrationResult, LandingError, LandingWait, RebaseAttempt,
};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct FastWait(pub Vec<Duration>);
impl LandingWait for FastWait {
    fn wait(&mut self, delay: Duration) {
        self.0.push(delay);
    }
}

pub(super) struct NeverMoved;
impl IntegrationGate for NeverMoved {
    fn reverify_and_repair(
        &mut self,
        _workspace: &Path,
        _new_parent: &str,
        _rebase: &RebaseAttempt,
        _deadline: Instant,
    ) -> Result<IntegrationResult, LandingError> {
        panic!("the base did not move")
    }
}

#[cfg(unix)]
pub(super) fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("make fixture hook executable");
}
#[cfg(not(unix))]
pub(super) fn executable(_path: &Path) {}

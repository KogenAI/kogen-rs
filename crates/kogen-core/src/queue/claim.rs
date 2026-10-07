//! The single Git ref claim shared by all checkouts of an origin.
use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use crate::git::GitRepo;
use rand::RngCore;
use std::path::Path;
use std::process::Command;

const CLAIM_REF: &str = "refs/kogen/claim";

#[derive(Debug)]
pub enum ClaimStart {
    Acquired(OriginClaim),
    Held { run_id: String },
}

#[derive(Debug)]
pub struct OriginClaim {
    origin: GitRepo,
    commit: String,
    run_id: String,
    owned: bool,
}

impl OriginClaim {
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Delete the claim only when the origin still points at this owner's commit.
    pub fn release(mut self) -> Result<bool, CoreError> {
        let removed = self
            .origin
            .delete_ref_cas(CLAIM_REF, &self.commit)
            .map_err(|error| claim_error("claim_release_failed", error))?;
        self.owned = false;
        Ok(removed)
    }
}

impl Drop for OriginClaim {
    fn drop(&mut self) {
        if self.owned {
            let _ = self.origin.delete_ref_cas(CLAIM_REF, &self.commit);
            self.owned = false;
        }
    }
}

/// Make a 32-character run id that carries a process-start identity for live/stale checks.
pub fn new_run_id() -> Result<String, CoreError> {
    let pid = std::process::id();
    let started = process_start_seconds(pid).ok_or_else(|| {
        CoreError::new(
            ErrorClass::Environment,
            "queue_owner_probe_failed",
            "could not determine this process's start time",
            ExitCode::Environment,
        )
    })?;
    let mut entropy = [0_u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut entropy);
    Ok(format!("{pid:08x}{started:08x}{}", hex(&entropy)))
}

/// Take over only a stale claim. Two CAS attempts bound concurrent recovery races.
pub fn claim(origin_path: impl AsRef<Path>, run_id: &str) -> Result<ClaimStart, CoreError> {
    if !owner_marker(run_id).is_some_and(|(pid, started)| {
        pid == std::process::id() && process_start_seconds(pid) == Some(started)
    }) {
        return Err(CoreError::new(
            ErrorClass::Environment,
            "claim_owner_invalid",
            "run id does not identify the current process",
            ExitCode::Environment,
        ));
    }
    let origin = GitRepo::new(origin_path.as_ref());
    for _ in 0..2 {
        let current = origin
            .ref_target(CLAIM_REF)
            .map_err(|error| claim_error("claim_read_failed", error))?;
        if let Some(current) = current {
            let existing = claim_run_id(&origin, &current)?;
            if existing.as_deref().is_some_and(claim_owner_is_live) {
                return Ok(ClaimStart::Held {
                    run_id: existing.unwrap_or_default(),
                });
            }
            let removed = origin
                .delete_ref_cas(CLAIM_REF, &current)
                .map_err(|error| claim_error("claim_takeover_failed", error))?;
            if !removed {
                continue;
            }
        }

        let files = [(
            ".kogen/claim".to_owned(),
            format!("{run_id}\n").into_bytes(),
        )]
        .into_iter()
        .collect();
        let message = format!("Kogen project claim\n\nKogen-Run: {run_id}");
        let commit = origin
            .create_commit(&files, None, &message)
            .map_err(|error| claim_error("claim_create_failed", error))?;
        if origin
            .cas_ref(CLAIM_REF, &commit, None)
            .map_err(|error| claim_error("claim_create_failed", error))?
        {
            return Ok(ClaimStart::Acquired(OriginClaim {
                origin,
                commit,
                run_id: run_id.to_owned(),
                owned: true,
            }));
        }
    }
    Err(CoreError::new(
        ErrorClass::Environment,
        "claim_race",
        "origin claim changed during both acquisition attempts",
        ExitCode::Environment,
    ))
}

fn claim_run_id(origin: &GitRepo, commit: &str) -> Result<Option<String>, CoreError> {
    let bytes = origin
        .blob_at(commit, ".kogen/claim")
        .map_err(|error| claim_error("claim_read_failed", error))?;
    Ok(bytes
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
        .filter(|run_id| !run_id.is_empty()))
}

fn claim_owner_is_live(run_id: &str) -> bool {
    owner_marker(run_id).is_some_and(|(pid, started)| process_start_seconds(pid) == Some(started))
}

fn owner_marker(run_id: &str) -> Option<(u32, u32)> {
    if run_id.len() != 32
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let pid = u32::from_str_radix(&run_id[..8], 16).ok()?;
    let started = u32::from_str_radix(&run_id[8..16], 16).ok()?;
    (pid > 0).then_some((pid, started))
}

fn process_start_seconds(pid: u32) -> Option<u32> {
    if pid == 0 {
        return None;
    }
    let ps = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !ps.status.success() {
        return None;
    }
    let start = String::from_utf8(ps.stdout).ok()?.trim().to_owned();
    for args in [
        vec!["-j", "-f", "%a %b %e %H:%M:%S %Y", &start, "+%s"],
        vec!["-d", &start, "+%s"],
    ] {
        if let Ok(output) = Command::new("date").args(args).env("LC_ALL", "C").output()
            && output.status.success()
            && let Some(seconds) = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<u64>()
                .ok()
            && let Ok(seconds) = u32::try_from(seconds)
        {
            return Some(seconds);
        }
    }
    None
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn claim_error(reason: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Environment,
        reason,
        error.to_string(),
        ExitCode::Environment,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ClaimStart, claim, claim_owner_is_live, new_run_id, owner_marker, process_start_seconds,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ORIGIN: AtomicU64 = AtomicU64::new(0);

    struct TempOrigin(PathBuf);

    impl TempOrigin {
        fn new() -> Self {
            let id = NEXT_ORIGIN.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("kogen-queue-claim-{}-{id}.git", std::process::id()));
            let output = kogen_test_support::git_command()
                .args(["init", "--bare", "--quiet"])
                .arg(&path)
                .output()
                .expect("git is available");
            assert!(output.status.success());
            kogen_test_support::set_identity(
                &path,
                "Kogen queue test",
                "queue-test@example.invalid",
            )
            .expect("configure queue fixture identity");
            Self(path)
        }
    }

    impl Drop for TempOrigin {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn generated_claim_identity_tracks_the_live_process_start() {
        let run_id = new_run_id().expect("process identity available");
        let (pid, started) = owner_marker(&run_id).expect("well-formed run id");

        assert_eq!(pid, std::process::id());
        assert_eq!(process_start_seconds(pid), Some(started));
    }

    #[test]
    fn malformed_or_non_hex_claim_ids_are_not_live_owners() {
        assert!(owner_marker("not-a-run-id").is_none());
        assert!(owner_marker("ffffffffffffffffffffffffffffffff").is_some());
        assert!(!claim_owner_is_live("ffffffffffffffffffffffffffffffff"));
    }

    #[test]
    fn one_live_origin_claim_blocks_a_second_start_until_the_owner_releases() {
        let origin = TempOrigin::new();
        let first_id = new_run_id().expect("process identity available");
        let ClaimStart::Acquired(owner) = claim(&origin.0, &first_id).unwrap() else {
            panic!("first claim should acquire the origin");
        };

        let second_id = new_run_id().expect("process identity available");
        assert!(matches!(
            claim(&origin.0, &second_id).unwrap(),
            ClaimStart::Held { run_id } if run_id == first_id
        ));
        assert!(owner.release().unwrap());

        let retry_id = new_run_id().expect("process identity available");
        assert!(matches!(
            claim(&origin.0, &retry_id).unwrap(),
            ClaimStart::Acquired(_)
        ));
    }
}

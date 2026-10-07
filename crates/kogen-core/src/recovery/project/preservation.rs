//! Preserve a frozen workspace before permitting its removal.
use super::*;
mod archive;

pub(super) fn preserve_workspaces(
    origin: &GitRepo,
    state_root: &Path,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
) -> Result<bool, CoreError> {
    let entries =
        fs::read_dir(state_root).map_err(|error| recovery_error("workspace_read_failed", error))?;
    let prefix = format!("{}-", snapshot.run_id);
    let mut paths = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| recovery_error("workspace_read_failed", error))?;
    paths.sort_by_key(|entry| entry.file_name());
    let mut complete = true;
    for entry in paths {
        if !entry.file_name().to_string_lossy().starts_with(&prefix)
            || !entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        {
            continue;
        }
        let workspace = entry.path();
        let result = crate::run::stop_recovery_writers(store.directory())
            .map_err(|error| recovery_error("recovery_custody_failed", error))
            .and_then(|()| preserve(origin, &workspace, store, snapshot))
            .and_then(|()| {
                crate::git::forget_workspace(&workspace);
                fs::remove_dir_all(&workspace)
                    .map_err(|error| recovery_error("workspace_cleanup_failed", error))
            });
        if let Err(error) = result {
            complete = false;
            store
                .record(
                    &RunEvent::new("cleanup_failure", now_ms())
                        .with("workspace", json!(workspace))
                        .with("operation", json!("preserve workspace before cleanup"))
                        .with("detail", json!(render_recovery_error(&error))),
                    snapshot,
                )
                .map_err(|error| recovery_error("cleanup_failure_record_failed", error))?;
        }
    }
    Ok(complete)
}

fn preserve(
    origin: &GitRepo,
    workspace: &Path,
    store: &RunStore,
    snapshot: &mut RunSnapshot,
) -> Result<(), CoreError> {
    let repo = GitRepo::workspace(workspace);
    if !workspace.join(".git").exists() && covered_cleanup_remnant(origin, workspace, snapshot)? {
        return Ok(());
    }
    let head = repo
        .resolve_commit("HEAD")
        .map_err(|error| recovery_error("recovery_base_failed", error))?;
    let base = store
        .read_events()
        .ok()
        .and_then(|events| {
            events.iter().find_map(|event| {
                if event.event == "started" {
                    event
                        .fields
                        .get("base_sha")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            snapshot
                .landing
                .as_ref()
                .map(|landing| landing.expected_parent.clone())
        })
        .unwrap_or(head);
    crate::git::register_workspace_base(workspace, &base)
        .map_err(|error| recovery_error("recovery_base_failed", error))?;
    let tree = crate::gate::snapshot_tree(workspace)
        .map_err(|error| recovery_error("recovery_tree_failed", error))?;
    // An untouched scratch base has no candidate work to lose.
    if repo.resolve_tree(&base).ok().as_deref() == Some(&tree) {
        return Ok(());
    }
    let name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            recovery_error("recovery_identity_failed", "non-Unicode workspace identity")
        })?;
    let publication = (|| -> Result<String, CoreError> {
        let stem = format!("refs/kogen/candidates/{}/recovery-{name}", snapshot.run_id);
        let mut reference = stem.clone();
        if let Some(existing) = origin
            .ref_target(&reference)
            .map_err(|error| recovery_error("recovery_ref_failed", error))?
            && !matches_snapshot(origin, &existing, &base, &tree)
        {
            reference = format!("{stem}-{tree}");
        }
        let existing = origin
            .ref_target(&reference)
            .map_err(|error| recovery_error("recovery_ref_failed", error))?;
        if let Some(existing) = existing {
            if !matches_snapshot(origin, &existing, &base, &tree) {
                return Err(recovery_error(
                    "recovery_ref_conflict",
                    "published recovery does not cover current tree",
                ));
            }
        } else {
            let commit = repo
                .text(&[
                    "-c",
                    "user.name=Kogen recovery",
                    "-c",
                    "user.email=kogen@localhost",
                    "commit-tree",
                    &tree,
                    "-p",
                    &base,
                    "-m",
                    "Unverified recovery",
                ])
                .map_err(|error| recovery_error("recovery_commit_failed", error))?;
            let source = format!("{commit}:{reference}");
            let lease = format!("--force-with-lease={reference}:");
            repo.output(&[
                "-c",
                "core.fsync=committed",
                "push",
                "--no-verify",
                "--no-recurse-submodules",
                &lease,
                origin.path().to_str().ok_or_else(|| {
                    recovery_error("recovery_origin_failed", "non-Unicode origin")
                })?,
                &source,
            ])
            .map_err(|error| recovery_error("recovery_publication_failed", error))?;
            if origin.resolve_tree(&reference).ok().as_deref() != Some(&tree) {
                return Err(recovery_error(
                    "recovery_publication_failed",
                    "published tree differs",
                ));
            }
        }
        Ok(reference)
    })();
    let record = match publication {
        Ok(reference) => {
            json!({"workspace":workspace,"base":base,"tree":tree,"ref":reference,"archive":null,"verification":"unverified"})
        }
        Err(ref_error) => {
            let archive = archive::preserve(&repo, workspace, store.directory(), &base, &tree)
                .map_err(|archive_error| {
                    recovery_error(
                        "recovery_preservation_failed",
                        format!(
                            "{}; archive: {}",
                            render_recovery_error(&ref_error),
                            render_recovery_error(&archive_error)
                        ),
                    )
                })?;
            json!({"workspace":workspace,"base":base,"tree":null,"ref":null,"archive":archive,"verification":"unverified"})
        }
    };
    if !snapshot.recovery.contains(&record) {
        snapshot.recovery.push(record.clone());
        let mut event = RunEvent::new("recovery_preserved", now_ms());
        event.fields.extend(
            record
                .as_object()
                .expect("recovery record")
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        store
            .record(&event, snapshot)
            .map_err(|error| recovery_error("recovery_record_failed", error))?;
    }
    if crate::gate::snapshot_tree(workspace).ok().as_deref() != Some(&tree) {
        return Err(recovery_error(
            "recovery_workspace_changed",
            "workspace changed during preservation; keep it for another snapshot",
        ));
    }
    Ok(())
}

fn matches_snapshot(origin: &GitRepo, commit: &str, base: &str, tree: &str) -> bool {
    origin.resolve_tree(commit).ok().as_deref() == Some(tree)
        && origin
            .text(&["show", "-s", "--format=%P", commit])
            .ok()
            .as_deref()
            == Some(base)
}

/// remove_dir_all can fail after deleting .git or some files. A verified retained
/// publication still covers every surviving byte; differing later work is kept.
fn covered_cleanup_remnant(
    origin: &GitRepo,
    workspace: &Path,
    snapshot: &RunSnapshot,
) -> Result<bool, CoreError> {
    let remaining = crate::gate::workspace::WorkspaceTree::capture_excluding(workspace, &[])
        .map_err(|error| recovery_error("recovery_remnant_failed", error))?;
    for record in snapshot.recovery.iter().filter(|record| {
        record["workspace"]
            .as_str()
            .is_some_and(|path| Path::new(path) == workspace)
    }) {
        let (Some(reference), Some(tree), Some(base)) = (
            record["ref"].as_str(),
            record["tree"].as_str(),
            record["base"].as_str(),
        ) else {
            continue;
        };
        if !matches_snapshot(origin, reference, base, tree) {
            continue;
        }
        let mut covered = true;
        for (path, entry) in &remaining.entries {
            let Some(path) = path.to_str() else {
                covered = false;
                break;
            };
            let listing = origin
                .text(&["ls-tree", tree, "--", path])
                .unwrap_or_default();
            let mode = listing
                .split_whitespace()
                .next()
                .and_then(|mode| u32::from_str_radix(mode, 8).ok());
            if mode != Some(entry.mode)
                || origin.blob_at(tree, path).ok().flatten().as_deref()
                    != Some(entry.bytes.as_slice())
            {
                covered = false;
                break;
            }
        }
        if covered {
            return Ok(true);
        }
    }
    Ok(false)
}

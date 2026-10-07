use super::error::LandingError;
use super::gitops::{args, git, git_output, path_arg};
use super::refs::base_ref;
use super::repository::WorktreeUpdate;
use std::path::{Path, PathBuf};

pub(super) fn inspect_checked_out(
    origin: &Path,
    branch: &str,
) -> Result<Vec<WorktreeUpdate>, LandingError> {
    let reference = base_ref(branch)?;
    let listing = git(
        origin,
        &args(&["worktree", "list", "--porcelain"]),
        None,
        &[],
    )?;
    let worktrees = parse_worktrees(&String::from_utf8_lossy(&listing));
    let mut updates = Vec::new();
    for (path, checked_out) in worktrees {
        if checked_out.as_deref() != Some(reference.as_str()) {
            continue;
        }
        let status = git(
            &path,
            &args(&["status", "--porcelain", "--untracked-files=all"]),
            None,
            &[],
        )?;
        updates.push(WorktreeUpdate {
            dirty: !status.is_empty(),
            updated: false,
            path,
        });
    }
    Ok(updates)
}

pub(super) fn update_checked_out(
    inspected: &[WorktreeUpdate],
    commit: &str,
    expected_parent: &str,
    branch: &str,
) -> Vec<WorktreeUpdate> {
    inspected
        .iter()
        .map(|worktree| {
            if worktree.dirty {
                return worktree.clone();
            }
            let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
            let reference = format!("refs/heads/{branch}");
            let current_branch = git(
                &worktree.path,
                &args(&["symbolic-ref", "-q", "HEAD"]),
                None,
                &[],
            );
            let actual_branch = current_branch
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .map(|branch| branch.trim().to_owned());
            let clean = clean_against(&worktree.path, expected_parent);
            if actual_branch.as_deref() != Some(reference.as_str()) || !clean {
                return WorktreeUpdate {
                    path: worktree.path.clone(),
                    dirty: true,
                    updated: false,
                };
            }
            if git(
                &worktree.path,
                &args(&["update-ref", "--no-deref", "HEAD", expected_parent]),
                None,
                &[],
            )
            .is_err()
            {
                return WorktreeUpdate {
                    path: worktree.path.clone(),
                    dirty: true,
                    updated: false,
                };
            }
            let mut reset_args = args(&["reset", "--keep"]);
            reset_args.push(path_arg(Path::new(commit)));
            let reset = git(&worktree.path, &reset_args, None, &[]);
            let reattach = git(
                &worktree.path,
                &args(&["symbolic-ref", "HEAD", &reference]),
                None,
                &[],
            );
            WorktreeUpdate {
                path: worktree.path.clone(),
                dirty: reset.is_err() || reattach.is_err(),
                updated: reset.is_ok() && reattach.is_ok(),
            }
        })
        .collect()
}

fn clean_against(path: &Path, expected_parent: &str) -> bool {
    let diff = git_output(
        path,
        &args(&["diff", "--quiet", "--no-ext-diff", expected_parent, "--"]),
        None,
        &[],
    );
    if !diff.is_ok_and(|output| output.success()) {
        return false;
    }
    git(
        path,
        &args(&["ls-files", "--others", "--exclude-standard", "-z"]),
        None,
        &[],
    )
    .is_ok_and(|untracked| untracked.is_empty())
}

fn parse_worktrees(output: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut rows = Vec::new();
    let mut current_path = None;
    let mut current_branch = None;
    for line in output.lines().chain(std::iter::once("")) {
        if let Some(value) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("branch ") {
            current_branch = Some(value.to_owned());
        } else if line.is_empty() {
            if let Some(path) = current_path.take() {
                rows.push((path, current_branch.take()));
            }
            current_branch = None;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::parse_worktrees;
    use std::path::PathBuf;

    #[test]
    fn parses_worktree_paths_with_spaces_and_detached_entries() {
        let parsed = parse_worktrees(
            "worktree /tmp/main checkout\nHEAD abc\nbranch refs/heads/main\n\nworktree /tmp/detached\nHEAD def\ndetached\n\n",
        );
        assert_eq!(
            parsed,
            vec![
                (
                    PathBuf::from("/tmp/main checkout"),
                    Some("refs/heads/main".to_owned())
                ),
                (PathBuf::from("/tmp/detached"), None),
            ]
        );
    }
}

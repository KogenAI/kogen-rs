use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[test]
fn acceptance_install_removes_source_and_restorer_repairs_edits_and_absences() {
    let root = test_dir("restore");
    let accepted = b"approved test bytes\n";
    let source = ".kogen/acceptance/greet.t.sh";
    let candidate = "test/acceptance/greet.t.sh";
    fs::create_dir_all(root.join(".kogen/acceptance")).unwrap();
    fs::write(root.join(source), accepted).unwrap();
    let workspace = ProtectedWorkspace::new(
        BTreeMap::from([
            (
                candidate.to_owned(),
                ProtectedEntry {
                    sha256: intent_sha256(accepted),
                    bytes: Some(accepted.to_vec()),
                },
            ),
            (
                "SECRET.txt".to_owned(),
                ProtectedEntry {
                    sha256: ABSENT_SHA256.to_owned(),
                    bytes: None,
                },
            ),
        ]),
        vec![source.to_owned()],
    )
    .unwrap();

    install_approved_acceptance(&root, source, candidate, accepted).unwrap();
    assert!(!root.join(source).exists());
    fs::write(root.join(candidate), b"builder edit\n").unwrap();
    fs::write(root.join("SECRET.txt"), b"must remain absent").unwrap();
    assert_eq!(workspace.guard(&root).unwrap().len(), 2);

    let restored = workspace.restore_after_batch(&root).unwrap();
    assert_eq!(
        restored,
        vec!["SECRET.txt".to_owned(), candidate.to_owned()]
    );
    assert_eq!(fs::read(root.join(candidate)).unwrap(), accepted);
    assert!(!root.join("SECRET.txt").exists());
    assert!(workspace.guard(&root).unwrap().is_empty());
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn restoration_replaces_symlink_parent_without_touching_its_target() {
    use std::os::unix::fs::symlink;
    let root = test_dir("symlink");
    let outside = test_dir("outside");
    fs::write(outside.join("secret"), b"outside bytes").unwrap();
    symlink(&outside, root.join("protected")).unwrap();
    let expected = b"inside approved bytes";
    let workspace = ProtectedWorkspace::new(
        BTreeMap::from([(
            "protected/secret".to_owned(),
            ProtectedEntry {
                sha256: intent_sha256(expected),
                bytes: Some(expected.to_vec()),
            },
        )]),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(workspace.guard(&root).unwrap().len(), 1);
    workspace.restore_after_batch(&root).unwrap();
    assert_eq!(fs::read(root.join("protected/secret")).unwrap(), expected);
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside bytes");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}

fn test_dir(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "kogen-gate-protection-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

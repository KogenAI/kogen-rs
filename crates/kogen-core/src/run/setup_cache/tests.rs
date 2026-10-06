use super::{SetupCacheKey, SetupCacheRequest, cached_entry_count, run_setup};
use crate::project::ProjectConfig;
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let id = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kogen-setup-cache-test-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("temporary root");
        Self(root)
    }

    fn child(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).expect("temporary child");
        path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn request<'a>(checkout: &'a Path, cache: &'a Path, key: &'a str) -> SetupCacheRequest<'a> {
    SetupCacheRequest {
        checkout,
        cache_root: cache,
        key,
        outputs: vec!["build".to_owned()],
        enabled: true,
    }
}

#[test]
fn hit_restores_independent_writable_outputs() {
    let root = TempRoot::new();
    let cache = root.child("cache");
    let first = root.child("first");
    let second = root.child("second");
    let first_result = run_setup(request(&first, &cache, &"a".repeat(64)), || {
        fs::create_dir_all(first.join("build"))?;
        fs::write(first.join("build/result"), b"seed")?;
        Ok::<_, std::io::Error>(19)
    })
    .expect("setup and publish");
    assert!(first_result.published);

    let second_result = run_setup(
        request(&second, &cache, &"a".repeat(64)),
        || -> Result<u64, std::io::Error> { panic!("cache hit must not run setup") },
    )
    .expect("restore cache hit");
    assert!(second_result.reused);
    assert_eq!(second_result.saved_wall_ms, 19);
    fs::write(second.join("build/result"), b"workspace-two").expect("workspace output is writable");
    assert_eq!(fs::read(first.join("build/result")).unwrap(), b"seed");

    let third = root.child("third");
    let hit = run_setup(
        request(&third, &cache, &"a".repeat(64)),
        || -> Result<u64, std::io::Error> {
            panic!("mutating a restored copy must not change the saved entry")
        },
    )
    .expect("second cache restore");
    assert!(hit.reused);
    assert_eq!(fs::read(third.join("build/result")).unwrap(), b"seed");
}

#[test]
fn failed_or_mutating_setup_does_not_publish() {
    let root = TempRoot::new();
    let cache = root.child("cache");
    let workspace = root.child("workspace");
    let key = "b".repeat(64);
    let failed = run_setup(request(&workspace, &cache, &key), || {
        Err::<u64, _>("failed")
    });
    assert_eq!(failed.unwrap_err(), "failed");
    assert_eq!(cached_entry_count(&cache), 0);

    let outside = run_setup(request(&workspace, &cache, &key), || {
        fs::create_dir_all(workspace.join("build"))?;
        fs::write(workspace.join("build/result"), b"output")?;
        fs::write(workspace.join("source.txt"), b"mutated")?;
        Ok::<_, std::io::Error>(3)
    })
    .expect("successful setup still returns");
    assert!(!outside.published);
    assert_eq!(cached_entry_count(&cache), 0);
}

#[test]
fn cache_keeps_only_three_entries_and_touches_hits() {
    let root = TempRoot::new();
    let cache = root.child("cache");
    for (index, letter) in ["a", "b", "c", "d"].into_iter().enumerate() {
        let workspace = root.child(&format!("work-{index}"));
        let key = letter.repeat(64);
        run_setup(request(&workspace, &cache, &key), || {
            fs::create_dir_all(workspace.join("build"))?;
            fs::write(workspace.join("build/result"), letter)?;
            Ok::<_, std::io::Error>(1)
        })
        .expect("publish entry");
        if letter == "c" {
            let hit_workspace = root.child("hit-a");
            let hit_key = "a".repeat(64);
            assert!(
                run_setup(
                    request(&hit_workspace, &cache, &hit_key),
                    || -> Result<u64, std::io::Error> { panic!("a should still be cached") }
                )
                .unwrap()
                .reused
            );
        }
    }
    assert_eq!(cached_entry_count(&cache), 3);

    let oldest = root.child("oldest");
    let result = run_setup(request(&oldest, &cache, &"b".repeat(64)), || {
        fs::create_dir_all(oldest.join("build"))?;
        fs::write(oldest.join("build/result"), b"b")?;
        Ok::<_, std::io::Error>(1)
    })
    .unwrap();
    assert!(!result.reused);
}

#[test]
fn key_uses_compact_sorted_json_and_omits_run_specific_environment() {
    let root = TempRoot::new();
    fs::write(root.0.join("mix.exs"), b"input").unwrap();
    let config = ProjectConfig::from_bytes(
        root.0.join(".kogen/project.yaml"),
        b"name: sample\nchecks: []\nsetup:\n  - name: deps\n    argv: [mix, deps.get]\n    timeout_ms: 1000\nsetup_outputs: [deps]\nsetup_inputs: [mix.exs]\nenv:\n  CACHE_KIND: stable\n",
    )
    .unwrap();
    let env = BTreeMap::from([
        (OsString::from("PATH"), OsString::from("/bin")),
        (OsString::from("TMPDIR"), OsString::from("/tmp/one")),
        (
            OsString::from("MISE_STATE_DIR"),
            OsString::from("/tmp/mise-one"),
        ),
        (OsString::from("ELIXIR_VERSION"), OsString::from("1.18.0")),
        (OsString::from("OTP_VERSION"), OsString::from("27.2")),
    ]);
    let key = SetupCacheKey::from_project(Some(&config), &root.0, "tree-a", &env).unwrap();
    assert_eq!(key.digest().len(), 64);
    let text = String::from_utf8(key.canonical_bytes()).unwrap();
    assert!(text.starts_with("{\"arch\":\""));
    assert!(text.contains("\"base_tree\":\"\""));
    assert!(text.contains("\"elixir\":\"1.18.0\""));
    assert!(text.contains("\"otp\":\"27.2\""));
    assert!(!text.contains("TMPDIR"));
    assert!(!text.contains("MISE_STATE_DIR"));
    assert!(text.contains("\"inputs\":[{\"mode\":33188,\"path\":\"mix.exs\""));

    let checks_key = key.digest_with_checks(&json!([{"name":"lint"}])).unwrap();
    assert_ne!(key.digest(), checks_key);
}

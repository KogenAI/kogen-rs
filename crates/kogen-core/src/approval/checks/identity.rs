//! Verification cache identity, deliberately independent of setup-input narrowing.
use super::*;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};

pub(super) fn baseline_key(
    setup: &SetupCacheKey,
    tree: &str,
    checks: &JsonValue,
    checkout: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Result<Option<String>, CheckError> {
    let setup_material: JsonValue = serde_json::from_slice(&setup.canonical_bytes())
        .map_err(|error| CheckError::Internal(error.to_string()))?;
    let mut toolchain = serde_json::Map::new();
    for check in checks.as_array().into_iter().flatten() {
        let Some(program) = check["argv"]
            .as_array()
            .and_then(|argv| argv.first())
            .and_then(JsonValue::as_str)
        else {
            return Ok(None);
        };
        let path = if program.contains('/') {
            let path = Path::new(program);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                checkout.join(path)
            }
        } else {
            let Some(path) = env.get(&OsString::from("PATH")) else {
                return Ok(None);
            };
            let Some(path) = std::env::split_paths(path)
                .map(|directory| directory.join(program))
                .find(|path| executable(path))
            else {
                return Ok(None);
            };
            path
        };
        if !executable(&path) {
            return Ok(None);
        }
        let Ok(bytes) = fs::read(&path) else {
            return Ok(None);
        };
        let executable_identity = path.strip_prefix(checkout).map_or_else(
            |_| path.to_string_lossy().into_owned(),
            |relative| format!("checkout:{}", relative.display()),
        );
        toolchain.insert(
            program.to_owned(),
            json!({"path":executable_identity,"sha256":format!("{:x}",Sha256::digest(bytes))}),
        );
        // mix/exunit depend on both runtimes, not just the launcher script.
        if matches!(
            Path::new(program)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("mix" | "elixir")
        ) {
            if setup_material["elixir"]
                .as_str()
                .unwrap_or_default()
                .is_empty()
                || setup_material["otp"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty()
            {
                return Ok(None);
            }
            toolchain.insert("elixir".to_owned(), setup_material["elixir"].clone());
            toolchain.insert("otp".to_owned(), setup_material["otp"].clone());
        }
    }
    let material = json!({"v":3,"checked_base_tree":tree,"setup_key":setup.digest(),"checks":checks,
        "child_env":setup_material["child_env"],"toolchain":toolchain,"os":std::env::consts::OS,
        "arch":std::env::consts::ARCH,"adapter_version":"kogen-baseline-v3-adapters-1"});
    Ok(Some(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&material).expect("baseline identity"))
    )))
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn scratch_paths_do_not_change_identity_and_unknown_or_changed_tools_miss() {
        let root =
            std::env::temp_dir().join(format!("kogen-baseline-identity-{}", rand::random::<u64>()));
        let first = root.join("first");
        let second = root.join("second");
        for path in [&first, &second] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("check"), b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path.join("check"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let env = BTreeMap::new();
        let setup = SetupCacheKey::from_project(None, &first, "tree", &env).unwrap();
        let checks = json!([{"name":"check","argv":["./check"],"timeout_ms":1000}]);
        let key = baseline_key(&setup, "tree", &checks, &first, &env).unwrap();
        assert!(key.is_some());
        assert_eq!(
            key,
            baseline_key(&setup, "tree", &checks, &second, &env).unwrap()
        );
        fs::write(second.join("check"), b"#!/bin/sh\nexit 1\n").unwrap();
        assert_ne!(
            key,
            baseline_key(&setup, "tree", &checks, &second, &env).unwrap()
        );
        fs::remove_file(second.join("check")).unwrap();
        assert_eq!(
            baseline_key(&setup, "tree", &checks, &second, &env).unwrap(),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }
}

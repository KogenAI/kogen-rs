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
        toolchain.insert(
            program.to_owned(),
            json!({"path":path,"sha256":format!("{:x}",Sha256::digest(bytes))}),
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
        "arch":std::env::consts::ARCH,"adapter_version":"kogen-baseline-v3"});
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

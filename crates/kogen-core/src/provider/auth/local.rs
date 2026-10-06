//! Local host identity and random PKCE values for the OAuth flow.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use rand::RngCore as _;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::super::super::environment_error;

pub(super) fn host_id(home: &Path) -> Result<String, crate::error::CoreError> {
    let path = home.join(".kogen/host.json");
    if let Ok(bytes) = fs::read(&path)
        && let Ok(doc) = serde_json::from_slice::<Value>(&bytes)
        && let Some(id) = doc.get("ext_agent_host_id").and_then(Value::as_str)
        && valid_host_id(id)
    {
        return Ok(id.to_owned());
    }
    let mut bytes = [0_u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let uuid = format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    let body = serde_json::to_vec(&serde_json::json!({"ext_agent_host_id": uuid}))
        .map_err(|_| environment_error("host_write_failed", "could not encode host identity"))?;
    write_private_atomic(&path, &body).map_err(|_| {
        environment_error(
            "host_write_failed",
            format!("could not write {}", path.display()),
        )
    })?;
    Ok(uuid)
}

pub(super) fn pkce_values() -> (String, String, String, String) {
    let verifier = random_url_token(32);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (
        verifier,
        challenge,
        random_url_token(32),
        random_url_token(32),
    )
}

pub(super) fn login_wait() -> Duration {
    let scale = std::env::var("KOGEN_TIME_SCALE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(1.0);
    Duration::from_millis((300_000.0 * scale).floor().max(1.0) as u64)
}

fn random_url_token(size: usize) -> String {
    let mut bytes = vec![0_u8; size];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn valid_host_id(value: &str) -> bool {
    let Some(uuid) = value.strip_prefix("urn:uuid:") else {
        return false;
    };
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().expect("host path has parent");
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let temp = parent.join(format!(".host-{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temp, path)
}

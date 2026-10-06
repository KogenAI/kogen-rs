//! Private credential files and the macOS Keychain-backed encrypted store.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
use aes_gcm::aead::{Aead, KeyInit};
#[cfg(target_os = "macos")]
use aes_gcm::{Aes256Gcm, Nonce};
#[cfg(target_os = "macos")]
use base64::Engine as _;
#[cfg(target_os = "macos")]
use rand::RngCore as _;

use super::super::{accounts, environment_error, provider_error};
use super::Credential;

pub(crate) fn get(home: &Path, label: &str) -> Result<Option<Credential>, super::super::CoreError> {
    get_for(home, "chatgpt", label)
}

pub(crate) fn get_for<T: serde::de::DeserializeOwned>(
    home: &Path,
    provider: &str,
    label: &str,
) -> Result<Option<T>, super::super::CoreError> {
    validate_identity(provider, label)?;
    let path = credential_path(home, provider, label);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(invalid_credential(&path)),
    };
    let plaintext = if encrypted_store() {
        #[cfg(target_os = "macos")]
        {
            decrypt(home, provider, label, &bytes)?
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Err(invalid_credential(&path));
        }
    } else {
        bytes
    };
    serde_json::from_slice(&plaintext)
        .map(Some)
        .map_err(|_| invalid_credential(&path))
}

pub(crate) fn put(
    home: &Path,
    label: &str,
    credential: &Credential,
) -> Result<(), super::super::CoreError> {
    put_for(home, "chatgpt", label, credential)
}

pub(crate) fn put_for<T: serde::Serialize>(
    home: &Path,
    provider: &str,
    label: &str,
    credential: &T,
) -> Result<(), super::super::CoreError> {
    validate_identity(provider, label)?;
    let path = credential_path(home, provider, label);
    let plaintext = serde_json::to_vec(credential)
        .map_err(|_| environment_error("credential_write_failed", "could not encode credential"))?;
    let bytes = if encrypted_store() {
        #[cfg(target_os = "macos")]
        {
            encrypt(home, provider, label, &plaintext)?
        }
        #[cfg(not(target_os = "macos"))]
        {
            plaintext
        }
    } else {
        plaintext
    };
    write_private_atomic(&path, &bytes).map_err(|_| {
        environment_error(
            "credential_write_failed",
            format!("could not write {}", path.display()),
        )
    })
}

pub(crate) fn delete(home: &Path, label: &str) -> Result<(), super::super::CoreError> {
    delete_for(home, "chatgpt", label)
}

pub(crate) fn delete_for(
    home: &Path,
    provider: &str,
    label: &str,
) -> Result<(), super::super::CoreError> {
    validate_identity(provider, label)?;
    let path = credential_path(home, provider, label);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(environment_error(
                "credential_delete_failed",
                format!("could not remove {}", path.display()),
            ));
        }
    }
    #[cfg(target_os = "macos")]
    if encrypted_store() {
        keychain_delete(provider, label)?;
    }
    Ok(())
}

fn credential_path(home: &Path, provider: &str, label: &str) -> PathBuf {
    let suffix = if encrypted_store() && cfg!(target_os = "macos") {
        "enc"
    } else {
        "json"
    };
    home.join(".kogen/credentials")
        .join(format!("{provider}-{label}.{suffix}"))
}

#[cfg(test)]
fn encrypted_store() -> bool {
    false
}

#[cfg(not(test))]
fn encrypted_store() -> bool {
    if std::env::var("KOGEN_CREDENTIAL_STORE").as_deref() == Ok("file") {
        return false;
    }
    cfg!(target_os = "macos")
}

fn validate_label(label: &str) -> Result<(), super::super::CoreError> {
    if accounts::valid_label(label) {
        Ok(())
    } else {
        Err(provider_error(
            "invalid_account_label",
            "invalid account label",
        ))
    }
}

fn validate_identity(provider: &str, label: &str) -> Result<(), super::super::CoreError> {
    if !matches!(provider, "chatgpt" | "grok") {
        return Err(provider_error(
            "unsupported_provider",
            "unsupported credential provider",
        ));
    }
    validate_label(label)
}

fn invalid_credential(path: &Path) -> super::super::CoreError {
    environment_error(
        "invalid_credential_file",
        format!("{} is not valid", path.display()),
    )
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().expect("credential path has parent");
    fs::create_dir_all(parent)?;
    set_private_dir(parent)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".credential-{}-{stamp}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)
}

fn set_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn encrypt(
    home: &Path,
    provider: &str,
    label: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, super::super::CoreError> {
    let key = get_or_create_key(home, provider, label)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| environment_error("credential_store_unavailable", "invalid Keychain key"))?;
    let mut nonce_bytes = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| {
            environment_error(
                "credential_store_unavailable",
                "credential encryption failed",
            )
        })?;
    let mut output = nonce_bytes.to_vec();
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

#[cfg(target_os = "macos")]
fn decrypt(
    _home: &Path,
    provider: &str,
    label: &str,
    bytes: &[u8],
) -> Result<Vec<u8>, super::super::CoreError> {
    if bytes.len() < 12 {
        return Err(environment_error(
            "credential_store_unavailable",
            "encrypted credential is invalid",
        ));
    }
    let key = keychain_get(provider, label)?.ok_or_else(|| {
        environment_error(
            "credential_store_unavailable",
            "credential key is missing from Keychain",
        )
    })?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| environment_error("credential_store_unavailable", "invalid Keychain key"))?;
    cipher
        .decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..])
        .map_err(|_| {
            environment_error(
                "credential_store_unavailable",
                "credential decryption failed",
            )
        })
}

#[cfg(target_os = "macos")]
fn get_or_create_key(
    home: &Path,
    provider: &str,
    label: &str,
) -> Result<Vec<u8>, super::super::CoreError> {
    if let Some(key) = keychain_get(provider, label)? {
        return Ok(key);
    }
    if credential_path(home, provider, label).exists() {
        return Err(environment_error(
            "credential_store_unavailable",
            "credential key is missing from Keychain",
        ));
    }
    let mut key = vec![0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&key);
    let account = format!("{provider}:{label}:key");
    let mut child = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            "kogen",
            "-a",
            &account,
            "-w",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            environment_error("credential_store_unavailable", "Keychain is unavailable")
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| {
            environment_error("credential_store_unavailable", "Keychain is unavailable")
        })?
        .write_all(encoded.as_bytes())
        .map_err(|_| environment_error("credential_store_unavailable", "Keychain write failed"))?;
    if !child
        .wait()
        .map_err(|_| environment_error("credential_store_unavailable", "Keychain write failed"))?
        .success()
    {
        return Err(environment_error(
            "credential_store_unavailable",
            "Keychain write failed",
        ));
    }
    Ok(key)
}

#[cfg(target_os = "macos")]
fn keychain_get(provider: &str, label: &str) -> Result<Option<Vec<u8>>, super::super::CoreError> {
    let account = format!("{provider}:{label}:key");
    let output = Command::new("security")
        .args(["find-generic-password", "-s", "kogen", "-a", &account, "-w"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| {
            environment_error("credential_store_unavailable", "Keychain is unavailable")
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let encoded = String::from_utf8_lossy(&output.stdout);
    let key = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| {
            environment_error("credential_store_unavailable", "Keychain key is invalid")
        })?;
    if key.len() != 32 {
        return Err(environment_error(
            "credential_store_unavailable",
            "Keychain key is invalid",
        ));
    }
    Ok(Some(key))
}

#[cfg(target_os = "macos")]
fn keychain_delete(provider: &str, label: &str) -> Result<(), super::super::CoreError> {
    let account = format!("{provider}:{label}:key");
    let output = Command::new("security")
        .args(["delete-generic-password", "-s", "kogen", "-a", &account])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| {
            environment_error("credential_store_unavailable", "Keychain is unavailable")
        })?;
    if output.status.success() {
        Ok(())
    } else {
        // A deleted or never-created key is already the desired local state.
        Ok(())
    }
}

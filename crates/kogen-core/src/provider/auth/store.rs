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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoreFormat {
    Json,
    Encrypted,
}

#[derive(Debug)]
pub(crate) struct LoginCredential<T> {
    pub(crate) credential: Option<T>,
    pub(crate) unreadable: bool,
}

pub(crate) fn get(home: &Path, label: &str) -> Result<Option<Credential>, super::super::CoreError> {
    get_for(home, "chatgpt", label)
}

pub(crate) fn get_for<T: serde::de::DeserializeOwned>(
    home: &Path,
    provider: &str,
    label: &str,
) -> Result<Option<T>, super::super::CoreError> {
    Ok(read_for(
        home,
        provider,
        label,
        store_format(),
        false,
        |path, bytes| decrypt_for_store(home, provider, label, path, bytes),
    )?
    .credential)
}

pub(crate) fn get_for_login<T: serde::de::DeserializeOwned>(
    home: &Path,
    provider: &str,
    label: &str,
) -> Result<LoginCredential<T>, super::super::CoreError> {
    read_for(
        home,
        provider,
        label,
        store_format(),
        true,
        |path, bytes| decrypt_for_store(home, provider, label, path, bytes),
    )
}

fn read_for<T: serde::de::DeserializeOwned>(
    home: &Path,
    provider: &str,
    label: &str,
    format: StoreFormat,
    tolerate_undecryptable: bool,
    decrypt: impl FnOnce(&Path, &[u8]) -> Result<Vec<u8>, super::super::CoreError>,
) -> Result<LoginCredential<T>, super::super::CoreError> {
    validate_identity(provider, label)?;
    let path = credential_path_for(home, provider, label, format);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoginCredential {
                credential: None,
                unreadable: false,
            });
        }
        Err(_) => return Err(invalid_credential(&path, provider)),
    };
    let plaintext = if format == StoreFormat::Encrypted {
        match decrypt(&path, &bytes) {
            Ok(plaintext) => plaintext,
            Err(error)
                if tolerate_undecryptable
                    && is_undecryptable_error(&error)
                    && format == StoreFormat::Encrypted =>
            {
                return Ok(LoginCredential {
                    credential: None,
                    unreadable: true,
                });
            }
            Err(error) => return Err(with_login_hint(error, provider)),
        }
    } else {
        bytes
    };
    match serde_json::from_slice(&plaintext) {
        Ok(credential) => Ok(LoginCredential {
            credential: Some(credential),
            unreadable: false,
        }),
        Err(_) if tolerate_undecryptable && format == StoreFormat::Encrypted => {
            Ok(LoginCredential {
                credential: None,
                unreadable: true,
            })
        }
        Err(_) => Err(invalid_credential(&path, provider)),
    }
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
    put_for_mode(home, provider, label, credential, false)
}

pub(crate) fn put_for_login<T: serde::Serialize>(
    home: &Path,
    provider: &str,
    label: &str,
    credential: &T,
) -> Result<(), super::super::CoreError> {
    put_for_mode(home, provider, label, credential, true)
}

fn put_for_mode<T: serde::Serialize>(
    home: &Path,
    provider: &str,
    label: &str,
    credential: &T,
    replace_existing: bool,
) -> Result<(), super::super::CoreError> {
    let format = store_format();
    put_for_with(
        home,
        provider,
        label,
        credential,
        format,
        replace_existing,
        |plaintext, replace_existing| {
            encrypt_for_store(home, provider, label, plaintext, format, replace_existing)
        },
    )
}

fn put_for_with<T: serde::Serialize>(
    home: &Path,
    provider: &str,
    label: &str,
    credential: &T,
    format: StoreFormat,
    replace_existing: bool,
    encrypt: impl FnOnce(&[u8], bool) -> Result<Vec<u8>, super::super::CoreError>,
) -> Result<(), super::super::CoreError> {
    validate_identity(provider, label)?;
    let path = credential_path_for(home, provider, label, format);
    let plaintext = serde_json::to_vec(credential)
        .map_err(|_| environment_error("credential_write_failed", "could not encode credential"))?;
    let bytes = if format == StoreFormat::Encrypted {
        encrypt(&plaintext, replace_existing)?
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
    credential_path_for(home, provider, label, store_format())
}

fn credential_path_for(home: &Path, provider: &str, label: &str, format: StoreFormat) -> PathBuf {
    let suffix = match format {
        StoreFormat::Json => "json",
        StoreFormat::Encrypted => "enc",
    };
    home.join(".kogen/credentials")
        .join(format!("{provider}-{label}.{suffix}"))
}

fn store_format() -> StoreFormat {
    if encrypted_store() && cfg!(target_os = "macos") {
        StoreFormat::Encrypted
    } else {
        StoreFormat::Json
    }
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

fn invalid_credential(path: &Path, provider: &str) -> super::super::CoreError {
    environment_error(
        "invalid_credential_file",
        format!(
            "{} is not valid; run kogen provider login {provider}",
            path.display()
        ),
    )
}

fn with_login_hint(error: super::super::CoreError, provider: &str) -> super::super::CoreError {
    let hint = format!("run kogen provider login {provider}");
    if error.detail.contains(&hint) {
        return error;
    }
    super::super::CoreError::new(
        error.class,
        error.reason,
        format!("{}; {hint}", error.detail),
        error.exit_code,
    )
}

fn is_undecryptable_error(error: &super::super::CoreError) -> bool {
    error.reason == "credential_store_unavailable"
        && matches!(
            error.detail.as_str(),
            "encrypted credential is invalid"
                | "credential key is missing from Keychain"
                | "credential decryption failed"
                | "invalid Keychain key"
                | "Keychain key is invalid"
        )
}

fn decrypt_for_store(
    home: &Path,
    provider: &str,
    label: &str,
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<u8>, super::super::CoreError> {
    #[cfg(target_os = "macos")]
    {
        let _ = path;
        decrypt(home, provider, label, bytes)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home, provider, label, bytes);
        Err(invalid_credential(path, provider))
    }
}

fn encrypt_for_store(
    home: &Path,
    provider: &str,
    label: &str,
    plaintext: &[u8],
    format: StoreFormat,
    replace_existing: bool,
) -> Result<Vec<u8>, super::super::CoreError> {
    if format == StoreFormat::Json {
        return Ok(plaintext.to_vec());
    }
    #[cfg(target_os = "macos")]
    {
        encrypt(home, provider, label, plaintext, replace_existing)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home, provider, label, replace_existing);
        Ok(plaintext.to_vec())
    }
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
    replace_existing: bool,
) -> Result<Vec<u8>, super::super::CoreError> {
    let key = get_or_create_key(home, provider, label, replace_existing)?;
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
    replace_existing: bool,
) -> Result<Vec<u8>, super::super::CoreError> {
    match keychain_get(provider, label) {
        Ok(Some(key)) => return Ok(key),
        Ok(None) => {}
        Err(error) if replace_existing && is_invalid_keychain_key(&error) => {}
        Err(error) => return Err(error),
    }
    if credential_path(home, provider, label).exists() && !replace_existing {
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
fn is_invalid_keychain_key(error: &super::super::CoreError) -> bool {
    error.reason == "credential_store_unavailable" && error.detail == "Keychain key is invalid"
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

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::{LoginCredential, StoreFormat, credential_path_for, put_for_with, read_for};

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct FixtureCredential {
        access_token: String,
    }

    #[test]
    fn login_replaces_a_fabricated_undecryptable_enc_file() {
        let home = std::env::temp_dir().join(format!(
            "kogen-credential-store-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let path = credential_path_for(&home, "chatgpt", "default", StoreFormat::Encrypted);
        std::fs::create_dir_all(path.parent().expect("credential parent"))
            .expect("create temporary credential directory");
        std::fs::write(&path, b"fabricated ciphertext from an older format")
            .expect("write fabricated encrypted credential");

        let undecrypt = |_: &std::path::Path, _: &[u8]| {
            Err(super::super::super::environment_error(
                "credential_store_unavailable",
                "credential decryption failed",
            ))
        };
        let login_read: LoginCredential<FixtureCredential> = read_for(
            &home,
            "chatgpt",
            "default",
            StoreFormat::Encrypted,
            true,
            undecrypt,
        )
        .expect("login treats an undecryptable credential as absent");
        assert!(login_read.credential.is_none());
        assert!(login_read.unreadable);

        let read_error = read_for::<FixtureCredential>(
            &home,
            "chatgpt",
            "default",
            StoreFormat::Encrypted,
            false,
            |_, _| {
                Err(super::super::super::environment_error(
                    "credential_store_unavailable",
                    "credential decryption failed",
                ))
            },
        )
        .expect_err("commands that need the credential still fail");
        assert_eq!(read_error.reason, "credential_store_unavailable");
        assert!(
            read_error
                .detail
                .contains("run kogen provider login chatgpt")
        );

        let replacement = FixtureCredential {
            access_token: "fabricated replacement token".to_owned(),
        };
        let mut allowed_key_replacement = false;
        put_for_with(
            &home,
            "chatgpt",
            "default",
            &replacement,
            StoreFormat::Encrypted,
            true,
            |plaintext, replace_existing| {
                allowed_key_replacement = replace_existing;
                let mut sealed = b"test-sealed:".to_vec();
                sealed.extend_from_slice(plaintext);
                Ok(sealed)
            },
        )
        .expect("atomically replace fabricated encrypted credential");
        assert!(allowed_key_replacement);

        let replaced = std::fs::read(&path).expect("read fabricated test credential");
        let plaintext = replaced
            .strip_prefix(b"test-sealed:")
            .expect("replacement uses the test encryption wrapper");
        let stored: FixtureCredential =
            serde_json::from_slice(plaintext).expect("decode fabricated replacement");
        assert_eq!(stored, replacement);
        let entries = std::fs::read_dir(path.parent().expect("credential parent"))
            .expect("list temporary credential directory")
            .count();
        assert_eq!(entries, 1, "atomic replacement leaves no temporary files");

        std::fs::remove_dir_all(&home).expect("remove temporary HOME");
    }

    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    }
}

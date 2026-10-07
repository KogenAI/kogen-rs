//! ChatGPT login, logout, account listing and account selection.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::{accounts, auth, environment_error, provider_error};

const LABEL: &str = "default";
const UNREADABLE_CREDENTIAL_WARNING: &str =
    "kogen: warning: saved ChatGPT credential could not be read; login will replace it.\n";

pub fn list(home: &Path) -> Result<String, super::CoreError> {
    accounts::list(home)
}

/// Complete ChatGPT's PKCE flow. The progress callback is flushed by the CLI
/// before the browser opens so users can see and copy the authorization URL.
pub fn login(
    home: &Path,
    progress: impl FnMut(&str),
    warning: impl FnMut(&str),
) -> Result<String, super::CoreError> {
    login_with_auth(
        home,
        progress,
        warning,
        |home, previous_client, progress| auth::login_owned(home, previous_client, progress),
    )
}

fn login_with_auth(
    home: &Path,
    mut progress: impl FnMut(&str),
    mut warning: impl FnMut(&str),
    owned_login: impl FnOnce(
        &Path,
        Option<&str>,
        &mut dyn FnMut(&str),
    ) -> Result<
        (auth::Credential, String, Option<String>, Option<String>),
        super::CoreError,
    >,
) -> Result<String, super::CoreError> {
    let profiles = read_profiles(home)?;
    let old_profile = profile(&profiles, "chatgpt", LABEL).cloned();
    let show_notice = old_profile
        .as_ref()
        .and_then(|record| record.get("notice_shown"))
        .and_then(Value::as_bool)
        != Some(true);
    let login_credential = auth::get_login_credential(home, LABEL)?;
    warn_unreadable_credential(login_credential.unreadable, &mut warning);
    let old_credential = login_credential.credential;
    let previous_client = old_credential
        .as_ref()
        .map(|credential| credential.client_id.as_str());
    let (credential, subject, email, plan_usage) =
        owned_login(home, previous_client, &mut progress)?;
    if let Some(previous_subject) = old_profile
        .as_ref()
        .and_then(|record| record.get("subject"))
        .and_then(Value::as_str)
        && previous_subject != subject
    {
        return Err(provider_error(
            "login",
            "ChatGPT account subject changed for the default label; sign out before changing accounts",
        ));
    }
    auth::put_login_credential(home, LABEL, &credential)?;
    let mut profiles = profiles;
    update_profile(
        &mut profiles,
        LABEL,
        &credential,
        &subject,
        email.as_deref(),
        plan_usage.as_deref(),
        true,
        false,
        show_notice.then_some(true),
    );
    write_profiles(home, &profiles)?;
    if show_notice {
        progress("You're using your ChatGPT plan\n");
    }
    Ok(match email {
        Some(email) => format!("chatgpt:default signed in ({email})\n"),
        None => "chatgpt:default signed in\n".to_owned(),
    })
}

pub fn logout(home: &Path) -> Result<String, super::CoreError> {
    let mut profiles = read_profiles(home)?;
    let credential = auth::get_credential(home, LABEL)?;
    let remote_revoked = credential.as_ref().is_some_and(auth::revoke_owned);
    auth::delete_credential(home, LABEL)?;
    if let Some(credential) = credential.as_ref() {
        let subject = credential.subject.clone();
        let email = credential.email.clone();
        update_profile(
            &mut profiles,
            LABEL,
            credential,
            &subject,
            email.as_deref(),
            None,
            false,
            remote_revoked,
            None,
        );
    } else {
        update_profile_signed_out(&mut profiles, LABEL, remote_revoked);
    }
    write_profiles(home, &profiles)?;
    if remote_revoked {
        Ok("chatgpt:default signed out\n".to_owned())
    } else {
        Ok("chatgpt:default signed out locally; remote revocation was not confirmed. You can disconnect Kogen in ChatGPT Settings if needed.\n".to_owned())
    }
}

pub fn use_account(
    home: &Path,
    label: &str,
    project: Option<&Path>,
) -> Result<String, super::CoreError> {
    if !accounts::valid_label(label) {
        return Err(provider_error(
            "invalid_account_label",
            "invalid account label",
        ));
    }
    // Refuse malformed machine state before checking whether this label has a
    // credential, so `use` never hides a broken accounts.yaml.
    accounts::read(home)?;
    if let Some(project) = project {
        fs::canonicalize(project).map_err(|_| {
            environment_error(
                "project_not_found",
                format!("project path {} does not exist", project.display()),
            )
        })?;
    }
    if auth::get_credential(home, label)?.is_none() {
        return Err(provider_error(
            "login",
            format!(
                "Selected account {label} has no saved login; run kogen provider login chatgpt to sign in"
            ),
        ));
    }
    accounts::set_provider_use(home, "chatgpt", label, project)?;
    if let Some(project) = project {
        let path = fs::canonicalize(project).map_err(|_| {
            environment_error(
                "project_not_found",
                format!("project path {} does not exist", project.display()),
            )
        })?;
        Ok(format!(
            "chatgpt:{label} is the account for {}\n",
            path.display()
        ))
    } else {
        Ok(format!("chatgpt:{label} is the default account\n"))
    }
}

pub(crate) fn record_refresh(
    home: &Path,
    label: &str,
    credential: &auth::Credential,
) -> Result<(), super::CoreError> {
    let mut profiles = read_profiles(home)?;
    let current = profile(&profiles, "chatgpt", label)
        .cloned()
        .unwrap_or_default();
    let subject = current
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or(&credential.subject)
        .to_owned();
    let email = credential.email.clone();
    update_profile(
        &mut profiles,
        label,
        credential,
        &subject,
        email.as_deref(),
        current.get("plan_usage").and_then(Value::as_str),
        true,
        false,
        None,
    );
    write_profiles(home, &profiles)
}

pub(super) fn read_profiles(home: &Path) -> Result<Value, super::CoreError> {
    let path = accounts::profiles_path(home);
    match fs::read(&path) {
        Ok(bytes) => {
            let doc: Value = serde_json::from_slice(&bytes).map_err(|_| {
                environment_error(
                    "invalid_profiles_file",
                    format!("{} is not valid", path.display()),
                )
            })?;
            if doc.is_object() {
                Ok(doc)
            } else {
                Err(environment_error(
                    "invalid_profiles_file",
                    format!("{} is not valid", path.display()),
                ))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(_) => Err(environment_error(
            "invalid_profiles_file",
            format!("{} is not valid", path.display()),
        )),
    }
}

fn profile<'a>(profiles: &'a Value, provider: &str, label: &str) -> Option<&'a Value> {
    profiles.get(provider)?.get(label)
}

fn warn_unreadable_credential(unreadable: bool, warning: &mut impl FnMut(&str)) {
    if unreadable {
        warning(UNREADABLE_CREDENTIAL_WARNING);
    }
}

#[allow(clippy::too_many_arguments)]
fn update_profile(
    profiles: &mut Value,
    label: &str,
    credential: &auth::Credential,
    subject: &str,
    email: Option<&str>,
    plan_usage: Option<&str>,
    signed_in: bool,
    remote_revoked: bool,
    notice_shown: Option<bool>,
) {
    let profiles = profiles.as_object_mut().expect("validated profile object");
    let provider = profiles
        .entry("chatgpt".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("ChatGPT profile map");
    let previous = provider
        .get(label)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut record = previous;
    record.insert(
        "client_id".to_owned(),
        Value::String(credential.client_id.clone()),
    );
    record.insert("subject".to_owned(), Value::String(subject.to_owned()));
    record.insert(
        "email".to_owned(),
        email.map_or(Value::Null, |email| Value::String(email.to_owned())),
    );
    record.insert("expires_at".to_owned(), Value::from(credential.expires_at));
    record.insert("signed_in".to_owned(), Value::Bool(signed_in));
    record.insert(
        "plan_usage".to_owned(),
        plan_usage.map_or(Value::Null, |usage| Value::String(usage.to_owned())),
    );
    if let Some(notice_shown) = notice_shown {
        record.insert("notice_shown".to_owned(), Value::Bool(notice_shown));
    }
    record.insert("remote_revoked".to_owned(), Value::Bool(remote_revoked));
    provider.insert(label.to_owned(), Value::Object(record));
}

fn update_profile_signed_out(profiles: &mut Value, label: &str, remote_revoked: bool) {
    let Some(record) = profiles
        .get_mut("chatgpt")
        .and_then(Value::as_object_mut)
        .and_then(|provider| provider.get_mut(label))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    record.insert("signed_in".to_owned(), Value::Bool(false));
    record.insert("remote_revoked".to_owned(), Value::Bool(remote_revoked));
}

pub(super) fn write_profiles(home: &Path, profiles: &Value) -> Result<(), super::CoreError> {
    let path = accounts::profiles_path(home);
    let bytes = serde_json::to_vec(profiles)
        .map_err(|_| environment_error("profiles_write_failed", "could not encode profiles"))?;
    write_private_atomic(&path, &bytes).map_err(|_| {
        environment_error(
            "profiles_write_failed",
            format!("could not write {}", path.display()),
        )
    })
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().expect("profiles path has parent");
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp: PathBuf = parent.join(format!(".profiles-{}-{stamp}.tmp", std::process::id()));
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
    fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::{login_with_auth, warn_unreadable_credential};
    use crate::provider::auth::{self, Credential};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::{Value, json};
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use url::Url;

    const TEST_PRIVATE_KEY: &str = include_str!("auth/testdata/id-token-private.pem");
    const TEST_PUBLIC_MODULUS: &str = "pJp-vPp6IGgUoaCryWoZAJHp5rbgb1xyavqihuOaiweQAk3D3WBQNGCmnD0Q8owsJt1kbz402skh5Q10TW6X6zVwflLTnICSwCu6qD6aM6NIkuidnEAtOGoEtzpcazASnVqa8uLDJeQrTelEAzfYAYs0zsY4d7EYliPtX5fwMhdreoyhx7n6oK7OExLTknOEQ5VKXjnYpC6SZMyps9KJipTXkGcqbtqGJrS-Kj9kMSWNrEyDBv8juBiHVsUNwhI_6j8mwEVJbQPxkQ1AwpNdAqOmEDDqcu7r_ZQWel9pE-_b-zKDQmBlbL5q9wmMRn7TTMKTnhe_BnFMpfUWQ4bJlw";
    const RETRY_MESSAGE: &str =
        "Saved ChatGPT client was rejected; retrying with a fresh registration\n";

    #[test]
    fn unreadable_credential_warning_is_one_stderr_line() {
        let mut warnings = Vec::new();
        warn_unreadable_credential(true, &mut |line| warnings.push(line.to_owned()));
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0],
            "kogen: warning: saved ChatGPT credential could not be read; login will replace it.\n"
        );
        assert_eq!(warnings[0].lines().count(), 1);
    }

    #[test]
    fn readable_or_missing_credential_does_not_warn() {
        let mut warnings = Vec::new();
        warn_unreadable_credential(false, &mut |line| warnings.push(line.to_owned()));
        assert!(warnings.is_empty());
    }

    #[test]
    fn login_retries_rejected_clients_once_and_skips_profile_only_clients() {
        let server = FakeAuthServer::start();

        let credential_home = scratch_home();
        for (previous_client, error, registered_client) in [
            (
                "legacy-client-id",
                Some("3p_login_workspace_scope_denied"),
                "fresh-client-one",
            ),
            (
                "fresh-client-one",
                Some("access_denied"),
                "fresh-client-two",
            ),
        ] {
            auth::put_login_credential(
                &credential_home,
                "default",
                &Credential {
                    client_id: previous_client.to_owned(),
                    access_token: "test-access-token".to_owned(),
                    refresh_token: "test-refresh-token".to_owned(),
                    id_token: "test-id-token".to_owned(),
                    expires_at: 0,
                    scopes: Vec::new(),
                    subject: "test-subject".to_owned(),
                    email: None,
                    host_id: "test-host".to_owned(),
                },
            )
            .unwrap();
            let outcome = run_login(&credential_home, &server, error, registered_client);
            assert_eq!(
                outcome.stdout,
                "chatgpt:default signed in (owner@example.test)\n"
            );
            assert_eq!(
                outcome.authorization_clients,
                vec![previous_client, "dynamic_agent_client"]
            );
            assert_eq!(
                outcome
                    .progress
                    .iter()
                    .filter(|line| *line == RETRY_MESSAGE)
                    .count(),
                1
            );
            assert!(outcome.warnings.is_empty());
            let saved = auth::get_login_credential(&credential_home, "default")
                .unwrap()
                .credential
                .unwrap();
            assert_eq!(saved.client_id, registered_client);
            let profiles: Value = serde_json::from_slice(
                &std::fs::read(credential_home.join(".kogen/profiles.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                profiles["chatgpt"]["default"]["client_id"],
                registered_client
            );
        }

        let profile_home = scratch_home();
        std::fs::create_dir_all(profile_home.join(".kogen")).unwrap();
        std::fs::write(
            profile_home.join(".kogen/profiles.json"),
            br#"{"chatgpt":{"default":{"client_id":"signed-out-profile-client","subject":"test-subject","signed_in":false}}}"#,
        )
        .unwrap();
        let outcome = run_login(&profile_home, &server, None, "fresh-client-from-profile");
        assert_eq!(outcome.authorization_clients, ["dynamic_agent_client"]);
        assert!(!outcome.progress.iter().any(|line| line == RETRY_MESSAGE));
        let saved = auth::get_login_credential(&profile_home, "default")
            .unwrap()
            .credential
            .unwrap();
        assert_eq!(saved.client_id, "fresh-client-from-profile");

        std::fs::remove_dir_all(credential_home).unwrap();
        std::fs::remove_dir_all(profile_home).unwrap();
    }

    struct LoginOutcome {
        stdout: String,
        progress: Vec<String>,
        warnings: Vec<String>,
        authorization_clients: Vec<String>,
    }

    fn run_login(
        home: &Path,
        server: &FakeAuthServer,
        first_error: Option<&str>,
        registered_client_id: &str,
    ) -> LoginOutcome {
        let mut progress_lines = Vec::new();
        let mut warnings = Vec::new();
        let mut authorization_clients = Vec::new();
        let mut callback_workers = Vec::new();
        let stdout = login_with_auth(
            home,
            |line| progress_lines.push(line.to_owned()),
            |line| warnings.push(line.to_owned()),
            |home, previous_client, progress| {
                auth::login_owned_with_browser(
                    home,
                    previous_client,
                    progress,
                    Some(&server.base_url),
                    0,
                    |authorization_url| {
                        let authorization = Url::parse(authorization_url).unwrap();
                        let query: std::collections::HashMap<_, _> = authorization
                            .query_pairs()
                            .map(|(key, value)| (key.into_owned(), value.into_owned()))
                            .collect();
                        authorization_clients.push(query["client_id"].clone());
                        *server.nonce.lock().unwrap() = Some(query["nonce"].clone());
                        let callback_error = if authorization_clients.len() == 1 {
                            first_error
                        } else {
                            None
                        };
                        callback_workers.push(send_callback(
                            query["redirect_uri"].clone(),
                            query["state"].clone(),
                            callback_error.map(str::to_owned),
                            registered_client_id.to_owned(),
                        ));
                        Ok(())
                    },
                )
            },
        )
        .unwrap();
        for worker in callback_workers {
            worker.join().unwrap();
        }
        LoginOutcome {
            stdout,
            progress: progress_lines,
            warnings,
            authorization_clients,
        }
    }

    fn send_callback(
        redirect_uri: String,
        state: String,
        error: Option<String>,
        registered_client_id: String,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut callback = Url::parse(&redirect_uri).unwrap();
            {
                let mut query = callback.query_pairs_mut();
                query.append_pair("state", &state);
                if let Some(error) = error {
                    query.append_pair("error", &error);
                } else {
                    query.append_pair("code", "test-authorization-code");
                    query.append_pair("client_id", &registered_client_id);
                }
            }
            let target = format!("{}?{}", callback.path(), callback.query().unwrap());
            let mut stream = TcpStream::connect(("127.0.0.1", callback.port().unwrap())).unwrap();
            write!(
                stream,
                "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200 OK"));
        })
    }

    struct FakeAuthServer {
        base_url: String,
        nonce: Arc<Mutex<Option<String>>>,
        stopped: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl FakeAuthServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let nonce = Arc::new(Mutex::new(None));
            let server_nonce = Arc::clone(&nonce);
            let stopped = Arc::new(AtomicBool::new(false));
            let server_stopped = Arc::clone(&stopped);
            let server_base = base_url.clone();
            let worker = thread::spawn(move || {
                while !server_stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(5)))
                                .unwrap();
                            handle_fake_auth_request(&mut stream, &server_base, &server_nonce);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                base_url,
                nonce,
                stopped,
                worker: Some(worker),
            }
        }
    }

    impl Drop for FakeAuthServer {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    fn handle_fake_auth_request(
        stream: &mut TcpStream,
        base_url: &str,
        nonce: &Mutex<Option<String>>,
    ) {
        let (headers, body) = read_request(stream);
        let path = headers
            .lines()
            .next()
            .unwrap()
            .split_ascii_whitespace()
            .nth(1)
            .unwrap();
        let response = match path {
            "/.well-known/openid-configuration" => json!({
                "issuer": "https://auth.openai.com",
                "authorization_endpoint": format!("{base_url}/authorize"),
                "token_endpoint": format!("{base_url}/token"),
                "jwks_uri": format!("{base_url}/jwks")
            })
            .to_string(),
            "/token" => fake_token_response(&body, nonce),
            "/jwks" => json!({
                "keys": [{
                    "kty": "RSA",
                    "kid": "test-key",
                    "alg": "RS256",
                    "use": "sig",
                    "n": TEST_PUBLIC_MODULUS,
                    "e": "AQAB"
                }]
            })
            .to_string(),
            _ => "{}".to_owned(),
        };
        write_response(stream, &response);
    }

    fn fake_token_response(body: &[u8], nonce: &Mutex<Option<String>>) -> String {
        let form: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(body).into_owned().collect();
        let client_id = form.get("client_id").unwrap();
        let nonce = nonce.lock().unwrap().clone().unwrap();
        let claims = json!({
            "iss": "https://auth.openai.com",
            "aud": client_id,
            "sub": "test-subject",
            "email": "owner@example.test",
            "exp": auth::now_seconds() + 3600,
            "nonce": nonce,
            "https://api.openai.com/auth": {"chatgpt_plan_type": "plus"}
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-key".to_owned());
        let id_token = encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY.as_bytes()).unwrap(),
        )
        .unwrap();
        json!({
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token",
            "id_token": id_token,
            "expires_in": 3600,
            "scope": "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"
        })
        .to_string()
    }

    fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        // Accepted sockets inherit non-blocking mode from the listener on macOS.
        stream.set_nonblocking(false).unwrap();
        let mut header_bytes = Vec::new();
        while !header_bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).unwrap();
            header_bytes.push(byte[0]);
        }
        let headers = String::from_utf8(header_bytes).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or_default();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        (headers, body)
    }

    fn write_response(stream: &mut TcpStream, body: &str) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    fn scratch_home() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kogen-chatgpt-login-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

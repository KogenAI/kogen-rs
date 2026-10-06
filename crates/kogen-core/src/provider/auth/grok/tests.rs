use super::{GrokCredential, now_seconds, put, refresh_with};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;

#[test]
fn grok_401_refresh_rechecks_token_and_persists_rotation_before_reuse() {
    let home = scratch_home();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (headers, body) = read_request(&mut stream);
        write_response(
            &mut stream,
            br#"{"access_token":"new-access","expires_in":3600,"email":null}"#,
        );
        (headers, String::from_utf8(body).unwrap())
    });
    let current = GrokCredential {
        access_token: "old-access".to_owned(),
        refresh_token: "old-refresh".to_owned(),
        expires_at: now_seconds() - 10,
        scopes: vec!["grok-cli:access".to_owned()],
        email: Some("grok@example.test".to_owned()),
        client_id: "grok-client".to_owned(),
        token_endpoint: endpoint,
    };
    put(&home, "default", &current).unwrap();

    let newer = refresh_with(&home, "default", Some("different-rejected-token"), true).unwrap();
    assert_eq!(newer.access_token, "old-access");

    let refreshed = refresh_with(&home, "default", Some("old-access"), true).unwrap();
    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token, "old-refresh");
    assert_eq!(refreshed.scopes, ["grok-cli:access"]);
    assert_eq!(refreshed.email.as_deref(), Some("grok@example.test"));
    assert!(refreshed.expires_at > now_seconds());

    let (headers, body) = server.join().unwrap();
    assert!(headers.starts_with("POST /token HTTP/1.1\r\n"));
    assert!(body.contains("grant_type=refresh_token"));
    assert!(body.contains("client_id=grok-client"));
    assert!(body.contains("refresh_token=old-refresh"));
    let stored: GrokCredential = get_stored(&home);
    assert_eq!(stored, refreshed);
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(".kogen/profiles.json")).unwrap()).unwrap();
    assert_eq!(profile["grok"]["default"]["signed_in"], true);
    assert_eq!(profile["grok"]["default"]["email"], "grok@example.test");
    assert!(!home.join(".kogen/locks/grok-default.lock").exists());
    std::fs::remove_dir_all(home).unwrap();
}

fn get_stored(home: &std::path::Path) -> GrokCredential {
    super::get(home, "default").unwrap().unwrap()
}

fn scratch_home() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kogen-grok-refresh-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
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
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(str::to_owned)
        })
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (headers, body)
}

fn write_response(stream: &mut TcpStream, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}

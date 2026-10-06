//! Loopback callback listener used by the ChatGPT authorization-code flow.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};
use url::Url;

use super::super::super::provider_error;

pub(super) struct CallbackData {
    pub code: Option<String>,
    pub client_id: Option<String>,
}

pub(super) fn bind() -> Result<TcpListener, crate::error::CoreError> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))
        .map_err(|_| provider_error("login", "could not bind ChatGPT sign-in callback"))?;
    socket
        .set_reuse_address(true)
        .map_err(|_| provider_error("login", "could not configure ChatGPT sign-in callback"))?;
    let address = SocketAddr::from(([127, 0, 0, 1], 1455));
    socket.bind(&address.into()).map_err(|_| {
        provider_error("login", "ChatGPT sign-in callback port 1455 is unavailable")
    })?;
    socket
        .listen(16)
        .map_err(|_| provider_error("login", "could not listen for ChatGPT sign-in callback"))?;
    socket
        .set_nonblocking(true)
        .map_err(|_| provider_error("login", "could not configure ChatGPT sign-in callback"))?;
    Ok(socket.into())
}

pub(super) fn wait(
    listener: TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<CallbackData, crate::error::CoreError> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let target = read_target(&mut stream)?;
                if !target.starts_with("/auth/callback?") && target != "/auth/callback" {
                    write_response(&mut stream, false);
                    continue;
                }
                let url = Url::parse(&format!("http://127.0.0.1{target}"))
                    .map_err(|_| provider_error("login", "ChatGPT callback is invalid"))?;
                let mut query = std::collections::HashMap::new();
                for (key, value) in url.query_pairs() {
                    if query.insert(key.into_owned(), value.into_owned()).is_some() {
                        write_response(&mut stream, false);
                        return Err(provider_error("login", "ChatGPT callback is invalid"));
                    }
                }
                if query.get("state").map(String::as_str) != Some(expected_state) {
                    write_response(&mut stream, false);
                    return Err(provider_error(
                        "login",
                        "ChatGPT callback state did not match",
                    ));
                }
                if query.contains_key("error") {
                    write_response(&mut stream, false);
                    return Err(provider_error("login", "ChatGPT sign-in was declined"));
                }
                write_response(&mut stream, true);
                return Ok(CallbackData {
                    code: query.get("code").cloned(),
                    client_id: query.get("client_id").cloned(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(provider_error("login", "ChatGPT callback listener failed")),
        }
    }
    Err(provider_error(
        "login",
        "timed out waiting for ChatGPT sign-in callback",
    ))
}

fn read_target(stream: &mut TcpStream) -> Result<String, crate::error::CoreError> {
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    while bytes.len() < 8192 {
        let count = stream
            .read(&mut buffer)
            .map_err(|_| provider_error("login", "ChatGPT callback request was invalid"))?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&bytes)
        .map_err(|_| provider_error("login", "ChatGPT callback request was invalid"))?;
    let mut fields = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_ascii_whitespace();
    if fields.next() != Some("GET") {
        return Err(provider_error(
            "login",
            "ChatGPT callback request was invalid",
        ));
    }
    fields
        .next()
        .map(str::to_owned)
        .ok_or_else(|| provider_error("login", "ChatGPT callback request was invalid"))
}

fn write_response(stream: &mut TcpStream, success: bool) {
    let message = if success {
        "Kogen sign-in complete. You can close this tab."
    } else {
        "Kogen could not verify this sign-in callback."
    };
    let body = format!("<!doctype html><title>Kogen</title><p>{message}</p>");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

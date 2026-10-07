use super::{ReqwestPort, classify_grok_http_error, normalize_failure, size_limit_failure};
use crate::provider::http::client::{HttpPort, RequestDeadlines};
use crate::provider::http::wire::{ResponseMode, WireRequest};
use crate::provider::{ProviderErrorKind, ProviderFailure};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::thread;
use url::Url;

#[test]
fn grok_http_failures_follow_the_provider_contract() {
    let cases = [
        (
            401,
            "",
            ProviderErrorKind::Login,
            "Grok rejected this session; run `kogen provider login grok`.",
        ),
        (
            403,
            "",
            ProviderErrorKind::Login,
            "This Grok account cannot access the requested model.",
        ),
        (
            429,
            "",
            ProviderErrorKind::UsageLimit,
            "Grok subscription usage limit reached.",
        ),
        (
            400,
            "quota exceeded",
            ProviderErrorKind::UsageLimit,
            "Grok subscription usage limit reached.",
        ),
        (
            400,
            "server_is_overloaded",
            ProviderErrorKind::Overload,
            "Grok service is temporarily overloaded.",
        ),
        (
            503,
            "",
            ProviderErrorKind::Overload,
            "Grok service is temporarily overloaded.",
        ),
        (
            400,
            "bad request",
            ProviderErrorKind::Malformed,
            "Grok rejected the request (HTTP 400).",
        ),
    ];
    for (status, body, kind, message) in cases {
        let failure = classify_grok_http_error(status, body);
        assert_eq!(failure.kind, kind);
        assert_eq!(failure.message, message);
    }
    assert_eq!(
        size_limit_failure().message,
        "Grok response exceeded the size limit."
    );
    let malformed = normalize_failure(
        ProviderFailure::new(ProviderErrorKind::Malformed, "anything"),
        ResponseMode::Grok,
    );
    assert_eq!(
        malformed.message,
        "Grok returned a malformed response stream."
    );
}

#[test]
fn responses_transport_captures_turn_state_from_http_response_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 2\r\nX-Codex-Turn-State: sticky-route-123\r\nConnection: close\r\n\r\n{{}}"
        )
        .unwrap();
    });
    let request = WireRequest {
        endpoint: Url::parse(&endpoint).unwrap(),
        mode: ResponseMode::Injected,
        headers: Vec::new(),
        body: br#"{"model":"gpt-6-luna"}"#.to_vec(),
    };
    let port = ReqwestPort::new().unwrap();
    let attempt = port.execute(&request, RequestDeadlines::from_environment());
    assert!(attempt.response.is_err());
    assert_eq!(
        attempt.sticky_routing_token.as_deref(),
        Some("sticky-route-123")
    );
    server.join().unwrap();
}

fn read_request(stream: &mut TcpStream) {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
    }
    let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
    let body_bytes = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let mut body = vec![0; body_bytes];
    stream.read_exact(&mut body).unwrap();
}

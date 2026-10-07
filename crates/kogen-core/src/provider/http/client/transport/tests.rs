use super::{ReqwestPort, classify_grok_http_error, normalize_failure, size_limit_failure};
use crate::provider::http::client::{HttpPort, LimitedHttpAttempt, RequestDeadlines};
use crate::provider::http::wire::{ResponseMode, WireRequest};
use crate::provider::sse::{StreamLimitExceeded, StreamOutputLimits};
use crate::provider::{ProviderErrorKind, ProviderFailure};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};
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

#[test]
fn single_attempt_transport_does_not_follow_redirects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let redirect_location = format!("{endpoint}/redirected");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect_location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        drop(stream);

        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut redirected, _)) => {
                    read_request(&mut redirected);
                    write!(
                        redirected,
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                    return true;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("could not inspect redirected request: {error}"),
            }
        }
        false
    });

    let request = WireRequest {
        endpoint: Url::parse(&endpoint).unwrap(),
        mode: ResponseMode::Owned,
        headers: Vec::new(),
        body: br#"{"model":"gpt-6-luna"}"#.to_vec(),
    };
    let port = ReqwestPort::new_without_redirects().unwrap();
    let attempt = port.execute(&request, RequestDeadlines::from_environment());

    assert_eq!(attempt.status_code, Some(307));
    assert!(attempt.response.is_err());
    assert!(!server.join().unwrap(), "transport followed the redirect");
}

#[test]
fn replay_transport_cancels_a_fake_server_stream_on_output_overflow() {
    let words = std::iter::repeat_n("word", 513)
        .collect::<Vec<_>>()
        .join(" ");
    let attempt = run_fake_replay_stream(serde_json::json!({
        "type": "response.output_text.delta",
        "delta": words
    }));
    assert_eq!(
        attempt.limit_exceeded,
        Some(StreamLimitExceeded::OutputTokens)
    );
}

#[test]
fn replay_transport_cancels_a_fake_server_stream_on_reasoning_overflow() {
    let words = std::iter::repeat_n("thought", 1_025)
        .collect::<Vec<_>>()
        .join(" ");
    let attempt = run_fake_replay_stream(serde_json::json!({
        "type": "response.reasoning_summary_text.delta",
        "delta": words
    }));
    assert_eq!(
        attempt.limit_exceeded,
        Some(StreamLimitExceeded::ReasoningTokens)
    );
}

#[test]
fn replay_transport_keeps_provider_usage_from_the_frame_that_triggers_cancellation() {
    let attempt = run_fake_replay_stream(serde_json::json!({
        "type": "response.completed",
        "response": {
            "model": "gpt-6-luna",
            "status": "completed",
            "usage": {
                "input_tokens": 200,
                "output_tokens": 513,
                "input_tokens_details": {"cached_tokens": 50},
                "output_tokens_details": {"reasoning_tokens": 12}
            },
            "output": []
        }
    }));
    assert_eq!(
        attempt.limit_exceeded,
        Some(StreamLimitExceeded::OutputTokens)
    );
    assert_eq!(attempt.attempt.raw_usage.unwrap()["input_tokens"], 200);
}

fn run_fake_replay_stream(event: serde_json::Value) -> LimitedHttpAttempt {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        let frame = format!("data: {}\n\n", event);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n",
            frame.len()
        )
        .unwrap();
        stream.write_all(frame.as_bytes()).unwrap();
        stream.write_all(b"\r\n").unwrap();
        stream.flush().unwrap();
        thread::sleep(Duration::from_millis(150));
    });

    let request = WireRequest {
        endpoint: Url::parse(&endpoint).unwrap(),
        mode: ResponseMode::Owned,
        headers: Vec::new(),
        body: br#"{"model":"gpt-6-luna"}"#.to_vec(),
    };
    let port = ReqwestPort::new_without_redirects().unwrap();
    let result = port.execute_with_output_limits(
        &request,
        RequestDeadlines {
            first_byte: Duration::from_secs(2),
            idle: Duration::from_secs(2),
            total: Duration::from_secs(3),
            unscaled_idle_ms: 2_000,
        },
        StreamOutputLimits {
            output_tokens: 512,
            reasoning_tokens: 1_024,
            hard_budget_output_tokens: 2_048,
        },
    );
    server.join().unwrap();
    assert!(result.attempt.response.is_err());
    result
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

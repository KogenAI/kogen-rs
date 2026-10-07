//! Safe request metadata for Shape transcripts.

use super::ShapeRequestJournal;
use crate::provider::http::WireRequest;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

const ROUTING_HEADER_NAMES: &[&str] = &[
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-turn-state",
    "x-codex-window-id",
    "x-codex-turn-metadata",
    "x-grok-conv-id",
    "x-grok-session-id",
];

pub(super) fn request_metadata(
    wire: &WireRequest,
    cache_key: &str,
    thread_id: &str,
    previous_input_items: usize,
    started_at_ms: i64,
    ended_at_ms: i64,
) -> ShapeRequestJournal {
    let input_item_count = input_item_count(&wire.body).unwrap_or_default();
    let endpoint_host = wire
        .endpoint
        .host_str()
        .map(|host| {
            wire.endpoint.port().map_or_else(
                || host.to_owned(),
                |port| {
                    if host.contains(':') {
                        format!("[{host}]:{port}")
                    } else {
                        format!("{host}:{port}")
                    }
                },
            )
        })
        .unwrap_or_default();
    let routing_headers = wire
        .headers
        .iter()
        .filter(|(name, _)| {
            ROUTING_HEADER_NAMES
                .iter()
                .any(|routing_name| name.eq_ignore_ascii_case(routing_name))
        })
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    ShapeRequestJournal {
        cache_key: cache_key.to_owned(),
        thread_id: thread_id.to_owned(),
        endpoint_host,
        endpoint_path: wire.endpoint.path().to_owned(),
        routing_headers,
        body_bytes: wire.body.len(),
        body_prefix_sha256: input_prefix_end(&wire.body, input_item_count)
            .map(|end| sha256_hex(&wire.body[..end])),
        previous_input_items,
        previous_input_prefix_sha256: input_prefix_end(&wire.body, previous_input_items)
            .map(|end| sha256_hex(&wire.body[..end])),
        started_at_ms,
        ended_at_ms,
    }
}

pub(super) fn feedback_value(pass_index: usize, feedback_kind: &str, feedback: &str) -> Value {
    json!({
        "kind": "shape_feedback",
        "pass_index": pass_index,
        "feedback_kind": feedback_kind,
        "feedback": feedback,
    })
}

pub(super) fn append(path: &Path, value: &Value) {
    if let Ok(mut file) = OpenOptions::new().append(true).open(path) {
        let _ = writeln!(file, "{value}");
    }
}

pub(super) fn input_item_count(body: &[u8]) -> Option<usize> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("input")?
        .as_array()
        .map(Vec::len)
}

fn input_prefix_end(body: &[u8], item_count: usize) -> Option<usize> {
    const INPUT_MARKER: &[u8] = b"\"input\":[";
    let marker_start = body
        .windows(INPUT_MARKER.len())
        .position(|window| window == INPUT_MARKER)?;
    let mut cursor = marker_start + INPUT_MARKER.len();
    if item_count == 0 {
        return Some(cursor);
    }
    for index in 0..item_count {
        cursor = scan_json_value_end(body, skip_json_whitespace(body, cursor)?)?;
        if index + 1 < item_count {
            cursor = skip_json_whitespace(body, cursor)?;
            if body.get(cursor) != Some(&b',') {
                return None;
            }
            cursor += 1;
        }
    }
    Some(cursor)
}

fn skip_json_whitespace(body: &[u8], mut cursor: usize) -> Option<usize> {
    while body.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    (cursor < body.len()).then_some(cursor)
}

fn scan_json_value_end(body: &[u8], start: usize) -> Option<usize> {
    let first = *body.get(start)?;
    if first == b'"' {
        return scan_json_string_end(body, start);
    }
    if first == b'{' || first == b'[' {
        let mut depth = 0_usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, byte) in body.iter().enumerate().skip(start) {
            if in_string {
                if escaped {
                    escaped = false;
                } else if *byte == b'\\' {
                    escaped = true;
                } else if *byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(offset + 1);
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    let mut cursor = start;
    while body
        .get(cursor)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b',' | b']' | b'}'))
    {
        cursor += 1;
    }
    (cursor > start).then_some(cursor)
}

fn scan_json_string_end(body: &[u8], start: usize) -> Option<usize> {
    let mut escaped = false;
    for (offset, byte) in body.iter().enumerate().skip(start + 1) {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if *byte == b'"' {
            return Some(offset + 1);
        }
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::{feedback_value, input_item_count, request_metadata};
    use crate::intent::shaping::ShapeModelCall;
    use crate::provider::ModelUsage;
    use crate::provider::http::{ResponseMode, WireRequest};
    use serde_json::json;
    use url::Url;

    #[test]
    fn transcript_metadata_keeps_request_identity_but_never_prompt_or_header_values() {
        let body = br#"{"model":"gpt-6-luna","input":[{"role":"user","content":[{"type":"input_text","text":"prompt-do-not-journal"}]}]}"#.to_vec();
        let wire = WireRequest {
            endpoint: Url::parse("https://example.invalid:8443/v1/responses?api_key=secret")
                .unwrap(),
            mode: ResponseMode::Injected,
            headers: vec![
                ("authorization".to_owned(), "Bearer secret-token".to_owned()),
                ("thread-id".to_owned(), "thread-123".to_owned()),
                ("session-id".to_owned(), "cache-456".to_owned()),
                (
                    "x-codex-turn-state".to_owned(),
                    "route-token-do-not-journal".to_owned(),
                ),
            ],
            body,
        };
        let request = request_metadata(&wire, "cache-456", "thread-123", 0, 100, 125);
        let call = ShapeModelCall {
            role: "shaper".to_owned(),
            model: "gpt-6-luna".to_owned(),
            effort: "max".to_owned(),
            usage: ModelUsage::default(),
            wall_ms: 25,
            request,
        };
        let row = call.transcript_value();
        let serialized = row.to_string();

        assert_eq!(row["cache_key"], "cache-456");
        assert_eq!(row["thread_id"], "thread-123");
        assert_eq!(row["endpoint_host"], "example.invalid:8443");
        assert_eq!(row["endpoint_path"], "/v1/responses");
        assert_eq!(row["body_bytes"], wire.body.len());
        assert_eq!(row["started_at_ms"], 100);
        assert_eq!(row["ended_at_ms"], 125);
        assert_eq!(
            row["routing_headers"],
            json!(["session-id", "thread-id", "x-codex-turn-state"])
        );
        assert_eq!(row["body_prefix_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(input_item_count(&wire.body), Some(1));
        assert!(!serialized.contains("prompt-do-not-journal"));
        assert!(!serialized.contains("route-token-do-not-journal"));
        assert!(!serialized.contains("secret-token"));
        assert!(!serialized.contains("api_key=secret"));
    }

    #[test]
    fn prefix_digest_through_prior_items_is_stable_after_an_append() {
        let first = WireRequest {
            endpoint: Url::parse("https://example.invalid/v1/responses").unwrap(),
            mode: ResponseMode::Injected,
            headers: Vec::new(),
            body: br#"{"model":"gpt-6-luna","input":[{"role":"user","content":"first"}]}"#.to_vec(),
        };
        let second = WireRequest {
            body: br#"{"model":"gpt-6-luna","input":[{"role":"user","content":"first"},{"role":"assistant","content":"second"}]}"#.to_vec(),
            ..first.clone()
        };

        let first_row = request_metadata(&first, "cache", "thread", 0, 1, 2);
        let second_row = request_metadata(&second, "cache", "thread", 1, 3, 4);
        assert_eq!(
            first_row.body_prefix_sha256,
            second_row.previous_input_prefix_sha256
        );
        assert_eq!(input_item_count(&second.body), Some(2));
    }

    #[test]
    fn feedback_journal_records_pass_and_only_the_feedback_text() {
        let feedback =
            "candidate/intent_parse_failed: line 2: frontmatter is missing required key `size`";
        let row = feedback_value(2, "validation", feedback);

        assert_eq!(row["kind"], "shape_feedback");
        assert_eq!(row["pass_index"], 2);
        assert_eq!(row["feedback_kind"], "validation");
        assert_eq!(row["feedback"], feedback);
        assert!(row.get("instructions").is_none());
        assert!(row.get("prompt").is_none());
    }
}

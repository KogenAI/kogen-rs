use super::{MAX_RESPONSE_BYTES, SseAssembler, SseError};
use crate::provider::ProviderErrorKind;
use serde_json::{Value, json};

#[test]
fn frames_normalize_crlf_and_join_data_lines_then_flush_partial_frame() {
    let mut parser = SseAssembler::new();
    parser
        .feed(b": keepalive\r\n\r\nevent: response.completed\rdata: {\"type\":\r\ndata: \"response.completed\",\rdata: \"response\": {\"id\":\"r1\",\"status\":\"completed\"}}")
        .unwrap();
    let response = parser.finish().unwrap();
    assert_eq!(response.id, "r1");
}

#[test]
fn malformed_is_overridden_by_a_later_stream_failure() {
    let mut parser = SseAssembler::new();
    parser
        .feed(b"data: not-json\n\ndata: {\"type\":\"error\",\"error\":{\"message\":\"rate_limit\"}}\n\n")
        .unwrap();
    assert_eq!(parser.finish().unwrap_err().kind.as_str(), "usage_limit");
}

#[test]
fn incomplete_response_preserves_null_usage_and_never_runs_calls() {
    let mut parser = SseAssembler::new();
    feed_event(
        &mut parser,
        json!({
            "type":"response.completed",
            "response":{
                "id":"r",
                "status":"incomplete",
                "incomplete_details":{"reason":"max_output_tokens"}
            }
        }),
    );
    let failure = parser.finish().unwrap_err();
    assert_eq!(failure.kind, ProviderErrorKind::Incomplete);
    assert_eq!(failure.usage.as_ref().unwrap().input, None);
    assert_eq!(
        serde_json::to_value(failure.usage).unwrap()["output"],
        Value::Null
    );
}

#[test]
fn completed_output_overrides_arriving_items_and_maps_usage() {
    let mut parser = SseAssembler::new();
    feed_event(
        &mut parser,
        json!({
            "type":"response.output_item.done",
            "item":{"type":"message","content":[{"type":"output_text","text":"stale"}]}
        }),
    );
    feed_event(
        &mut parser,
        json!({
            "type":"response.completed",
            "response":{
                "id":"r",
                "status":"completed",
                "output":[
                    {"type":"message","content":[{"type":"output_text","text":"final "},{"type":"output_text","text":"text"}]},
                    {"type":"function_call","call_id":"call_1","name":"read","arguments":"{\"path\":\"file.txt\"}"}
                ],
                "usage":{
                    "input_tokens":100,
                    "output_tokens":20,
                    "input_tokens_details":{"cached_tokens":35},
                    "cache_write_tokens":4,
                    "output_tokens_details":{"reasoning_tokens":8}
                }
            }
        }),
    );
    let response = parser.finish().unwrap();
    assert_eq!(response.text, "final text");
    assert_eq!(response.raw_items.len(), 2);
    assert_eq!(response.tool_calls[0].arguments["path"], "file.txt");
    assert_eq!(response.usage.input, Some(65));
    assert_eq!(response.usage.cached_input, Some(35));
    assert_eq!(response.usage.cache_write, Some(4));
    assert_eq!(response.usage.output, Some(20));
    assert_eq!(response.usage.reasoning, Some(8));
}

#[test]
fn empty_completed_output_falls_back_to_collected_items() {
    let mut parser = SseAssembler::new();
    let item = json!({
        "type":"message",
        "content":[{"type":"output_text","text":"arrived"}]
    });
    feed_event(
        &mut parser,
        json!({"type":"response.output_item.done","item":item}),
    );
    feed_event(
        &mut parser,
        json!({"type":"response.completed","response":{"id":"r","status":"completed","output":[]}}),
    );
    let response = parser.finish().unwrap();
    assert_eq!(response.text, "arrived");
    assert_eq!(response.raw_items.len(), 1);
}

#[test]
fn malformed_tool_arguments_and_invalid_cached_usage_are_typed() {
    let mut malformed_call = SseAssembler::new();
    feed_event(
        &mut malformed_call,
        json!({
            "type":"response.completed",
            "response":{"id":"r","status":"completed","output":[{
                "type":"function_call","call_id":"c","name":"read","arguments":"[]"
            }]}
        }),
    );
    assert_eq!(
        malformed_call.finish().unwrap_err().kind,
        ProviderErrorKind::Malformed
    );

    let mut bad_usage = SseAssembler::new();
    feed_event(
        &mut bad_usage,
        json!({
            "type":"response.completed",
            "response":{"id":"r","status":"completed","output":[],"usage":{
                "input_tokens":10,"input_tokens_details":{"cached_tokens":11}
            }}
        }),
    );
    assert_eq!(
        bad_usage.finish().unwrap_err().kind,
        ProviderErrorKind::Malformed
    );
}

#[test]
fn completed_envelope_does_not_execute_in_progress_tool_items() {
    let mut parser = SseAssembler::new();
    feed_event(
        &mut parser,
        json!({
            "type":"response.completed",
            "response":{
                "id":"r",
                "status":"completed",
                "output":[{"type":"function_call","status":"in_progress"}]
            }
        }),
    );
    let response = parser.finish().unwrap();
    assert!(response.tool_calls.is_empty());
    assert_eq!(response.raw_items.len(), 1);
}

#[test]
fn response_body_limit_is_exact_and_typed() {
    let mut parser = SseAssembler::new();
    assert_eq!(
        parser.feed(&vec![b'x'; MAX_RESPONSE_BYTES + 1]),
        Err(SseError::BodyTooLarge)
    );
    assert_eq!(
        parser.finish().unwrap_err().kind,
        ProviderErrorKind::Malformed
    );
}

fn feed_event(parser: &mut SseAssembler, event: Value) {
    let frame = format!("data: {event}\n\n");
    parser.feed(frame.as_bytes()).unwrap();
}

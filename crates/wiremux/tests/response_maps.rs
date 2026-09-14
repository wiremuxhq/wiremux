//! Complete-body maps. Stream goldens stay delta-only.

use serde_json::json;
use wiremux::{
    IrStreamEvent, RawSse, ResolvedProfile, Wire, decode_response, decode_stream_events,
    parse_profile_str,
};

fn chat_profile() -> ResolvedProfile {
    parse_profile_str(
        r#"
schema_version = 1
id = "test-chat"
wire = "chat-completions"
"#,
    )
    .expect("test profile parses")
}

fn messages_profile() -> ResolvedProfile {
    parse_profile_str(
        r#"
schema_version = 1
id = "test-messages"
wire = "messages"
"#,
    )
    .expect("test profile parses")
}

fn responses_profile() -> ResolvedProfile {
    parse_profile_str(
        r#"
schema_version = 1
id = "test-responses"
wire = "responses"
"#,
    )
    .expect("test profile parses")
}

fn gemini_profile() -> ResolvedProfile {
    parse_profile_str(
        r#"
schema_version = 1
id = "test-gemini"
wire = "gemini"
"#,
    )
    .expect("test profile parses")
}

#[test]
fn chat_complete_message_content_finish_usage() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "hello from complete"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5
        }
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("complete message must decode");
    let text = events.iter().find_map(|ev| match ev {
        IrStreamEvent::TextDelta { text } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(
        text,
        Some("hello from complete"),
        "must read choices[0].message.content, not delta: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "finish_reason=stop missing: {events:?}"
    );
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            }
        )),
        "usage missing: {events:?}"
    );
}

#[test]
fn chat_complete_message_tool_calls() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_complete",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"SF\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("complete tool_calls must decode");
    let start = events.iter().find_map(|ev| match ev {
        IrStreamEvent::ToolCallStart { id, name, .. } => Some((id.as_str(), name.as_str())),
        _ => None,
    });
    assert_eq!(start, Some(("call_complete", "get_weather")));
    let args: String = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(args, r#"{"city":"SF"}"#);
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ToolCallEnd)),
        "complete tool_calls must emit ToolCallEnd: {events:?}"
    );
}

#[test]
fn stream_delta_stays_delta_only_and_complete_ignores_missing_message() {
    let delta_only = json!({
        "choices": [{
            "delta": { "content": "hi from delta" }
        }]
    });
    let raw = RawSse {
        event: None,
        data: delta_only.to_string(),
    };
    let streamed = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("delta-only stream body must still decode");
    assert!(
        streamed
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "hi from delta")),
        "decode_stream_events must keep reading delta.content: {streamed:?}"
    );

    let body = serde_json::to_vec(&delta_only).expect("json");
    let complete = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("delta-only complete body is not a map error");
    assert!(
        !complete
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "hi from delta")),
        "decode_response must not invent a complete message from missing message: {complete:?}"
    );
}

#[test]
fn responses_complete_output_text_is_text_delta() {
    let body = br#"{"id":"resp_1","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hello from responses"}]}],"usage":{"input_tokens":3,"output_tokens":2}}"#;
    let events = decode_response(Wire::Responses, body, &responses_profile()).unwrap();
    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "hello from responses")
        ),
        "{events:?}"
    );
}

#[test]
fn gemini_prompt_feedback_block_reason_is_content_filter() {
    let body = serde_json::to_vec(&json!({
        "promptFeedback": { "blockReason": "SAFETY" }
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &body, &gemini_profile())
        .expect("blocked complete must decode");
    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "content_filter")
        ),
        "complete promptFeedback.blockReason=SAFETY must be content_filter: {events:?}"
    );

    let raw = RawSse {
        event: None,
        data: r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#.into(),
    };
    let streamed = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("blocked chunk must decode");
    assert!(
        streamed.iter().any(
            |ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "content_filter")
        ),
        "stream promptFeedback.blockReason=SAFETY must be content_filter: {streamed:?}"
    );

    let unknown = serde_json::to_vec(&json!({
        "promptFeedback": { "blockReason": "NOT_A_KNOWN_REASON" }
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &unknown, &gemini_profile())
        .expect("unknown blockReason must decode");
    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "content_filter")
        ),
        "unknown blockReason must be content_filter, not stop: {events:?}"
    );
}

#[test]
fn messages_complete_redacted_thinking_is_protocol() {
    let body = serde_json::to_vec(&json!({
        "content": [{ "type": "redacted_thinking", "data": "enc" }]
    }))
    .expect("json");
    let events = decode_response(Wire::Messages, &body, &messages_profile())
        .expect("redacted_thinking must decode");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Protocol { item_type, payload }
            if item_type == "redacted_thinking"
                && payload.get("data").and_then(|v| v.as_str()) == Some("enc")
        )),
        "complete redacted_thinking must be Protocol, got {events:?}"
    );
}

#[test]
fn responses_complete_reasoning_summary_is_reasoning_delta() {
    let body = serde_json::to_vec(&json!({
        "status": "completed",
        "output": [{
            "type": "reasoning",
            "summary": [{ "type": "summary_text", "text": "hi" }]
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile())
        .expect("reasoning must decode");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ReasoningDelta { text } if text == "hi")),
        "complete reasoning summary_text must be ReasoningDelta, got {events:?}"
    );
}

#[test]
fn chat_complete_non_function_tool_call_is_protocol() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_custom",
                    "type": "custom",
                    "custom": { "name": "browser", "input": "{}" }
                }]
            }
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("non-function tool_call must decode");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::Protocol { .. })),
        "complete tool_calls type != function must be Protocol, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ToolCallStart { .. })),
        "must not invent a function ToolCallStart: {events:?}"
    );
}

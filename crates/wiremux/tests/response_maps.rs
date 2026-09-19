//! Complete-body maps. Stream goldens stay delta-only.

use serde_json::json;
use wiremux::{
    IrStreamEvent, RawSse, ResolvedProfile, Wire, decode_response, decode_stream_events,
    encode_response, parse_profile_str,
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
            IrStreamEvent::ToolCallArgDelta { delta, .. } => Some(delta.as_str()),
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
fn responses_complete_refusal_is_refusal_delta() {
    let body = serde_json::to_vec(&json!({
        "status": "completed",
        "output": [{
            "type": "message",
            "content": [{ "type": "refusal", "refusal": "nope" }]
        }]
    }))
    .expect("json");
    let events =
        decode_response(Wire::Responses, &body, &responses_profile()).expect("refusal must decode");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::RefusalDelta { text } if text == "nope")),
        "complete dest Responses refusal must be RefusalDelta, got {events:?}"
    );
}

#[test]
fn dest_chat_complete_decode_message_refusal_is_refusal_delta() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "refusal": "nope"
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("dest Chat message.refusal must decode");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::RefusalDelta { text } if text == "nope")),
        "dest Chat message.refusal must be RefusalDelta, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "nope")),
        "dest Chat refusal must not become dest Chat content TextDelta, got {events:?}"
    );
}

#[test]
fn dest_chat_complete_encode_refusal_delta_is_message_refusal() {
    let mapped = encode_response(
        Wire::ChatCompletions,
        &[IrStreamEvent::RefusalDelta {
            text: "nope".into(),
        }],
    )
    .expect("encode dest Chat complete refusal");
    assert_eq!(
        mapped
            .pointer("/choices/0/message/refusal")
            .and_then(serde_json::Value::as_str),
        Some("nope"),
        "dest Chat complete encode must write message.refusal, got {mapped}"
    );
    let content = mapped.pointer("/choices/0/message/content");
    assert!(
        content.is_none()
            || content
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
            || content.is_some_and(serde_json::Value::is_null),
        "dest Chat complete refusal must keep content empty or null, got {mapped}"
    );
    assert_ne!(
        content.and_then(serde_json::Value::as_str),
        Some("nope"),
        "dest Chat complete refusal must not write dest Chat content, got {mapped}"
    );
}

#[test]
fn dest_responses_complete_encode_refusal_delta_is_refusal_part() {
    let mapped = encode_response(
        Wire::Responses,
        &[IrStreamEvent::RefusalDelta {
            text: "nope".into(),
        }],
    )
    .expect("encode dest Responses complete refusal");
    let part = mapped
        .get("output")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                item.get("content")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|parts| {
                        parts.iter().find(|part| {
                            part.get("type").and_then(serde_json::Value::as_str) == Some("refusal")
                        })
                    })
            })
        });
    assert_eq!(
        part.and_then(|p| p.get("refusal"))
            .and_then(serde_json::Value::as_str),
        Some("nope"),
        "dest Responses complete encode must emit refusal content part, got {mapped}"
    );
    assert_ne!(
        mapped
            .get("output_text")
            .and_then(serde_json::Value::as_str),
        Some("nope"),
        "dest Responses complete refusal must not become output_text, got {mapped}"
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
fn messages_complete_encodes_chat_completion() {
    let body = serde_json::to_vec(&json!({
        "id": "msg_json",
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "text", "text": "pong" }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 3, "output_tokens": 1 }
    }))
    .expect("json");
    let events = decode_response(Wire::Messages, &body, &messages_profile()).expect("decode");
    let mapped = encode_response(Wire::ChatCompletions, &events).expect("encode Chat");
    assert_eq!(
        mapped
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str()),
        Some("pong"),
        "Chat complete must carry Messages text, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/choices/0/finish_reason")
            .and_then(|v| v.as_str()),
        Some("stop"),
        "end_turn must encode as Chat stop, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/usage/completion_tokens")
            .and_then(|v| v.as_u64()),
        Some(1),
        "Chat usage must keep output tokens, got {mapped}"
    );
}

#[test]
fn messages_complete_thinking_keeps_chat_signature() {
    let body = serde_json::to_vec(&json!({
        "content": [
            { "type": "thinking", "thinking": "plan", "signature": "sig-1" },
            { "type": "text", "text": "pong" }
        ],
        "stop_reason": "end_turn"
    }))
    .expect("json");
    let events = decode_response(Wire::Messages, &body, &messages_profile()).expect("decode");
    let mapped = encode_response(Wire::ChatCompletions, &events).expect("encode Chat");
    assert_eq!(
        mapped
            .pointer("/choices/0/message/reasoning_content")
            .and_then(|v| v.as_str()),
        Some("plan"),
        "Chat complete must keep thinking text, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/choices/0/message/reasoning_signature")
            .and_then(|v| v.as_str()),
        Some("sig-1"),
        "Chat complete must keep thinking signature, got {mapped}"
    );
}

#[test]
fn messages_complete_from_chat_events() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "pong"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 1
        }
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile()).expect("decode");
    let mapped = encode_response(Wire::Messages, &events).expect("encode Messages");
    let text = mapped
        .pointer("/content")
        .and_then(serde_json::Value::as_array)
        .and_then(|blocks| {
            blocks.iter().find_map(|block| {
                (block.get("type").and_then(|v| v.as_str()) == Some("text"))
                    .then(|| block.get("text").and_then(|v| v.as_str()))
                    .flatten()
            })
        });
    assert_eq!(
        text,
        Some("pong"),
        "Messages complete must carry Chat text, got {mapped}"
    );
    assert_eq!(
        mapped.get("stop_reason").and_then(|v| v.as_str()),
        Some("end_turn"),
        "Chat stop must encode as Messages end_turn, got {mapped}"
    );
}

#[test]
fn gemini_complete_from_chat_events() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "pong"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 1
        }
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile()).expect("decode");
    let mapped = encode_response(Wire::Gemini, &events).expect("encode Gemini");
    let text = mapped
        .pointer("/candidates/0/content/parts")
        .and_then(serde_json::Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .find_map(|part| part.get("text").and_then(|v| v.as_str()))
        });
    assert_eq!(
        text,
        Some("pong"),
        "Gemini complete must carry Chat text, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/candidates/0/finishReason")
            .and_then(|v| v.as_str()),
        Some("STOP"),
        "Chat stop must encode as Gemini STOP, got {mapped}"
    );
}

#[test]
fn responses_complete_from_chat_events() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "pong"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 1
        }
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile()).expect("decode");
    let mapped = encode_response(Wire::Responses, &events).expect("encode Responses");
    let text = mapped
        .get("output_text")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            mapped
                .get("output")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        item.get("content")
                            .and_then(serde_json::Value::as_array)
                            .and_then(|parts| {
                                parts.iter().find_map(|part| {
                                    part.get("text").and_then(serde_json::Value::as_str)
                                })
                            })
                    })
                })
        });
    assert_eq!(
        text,
        Some("pong"),
        "Responses complete must carry Chat text, got {mapped}"
    );
    assert_eq!(
        mapped.get("status").and_then(|v| v.as_str()),
        Some("completed"),
        "Chat stop must encode as Responses completed, got {mapped}"
    );
}

#[test]
fn responses_complete_length_is_incomplete() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "pong"
            },
            "finish_reason": "length"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile()).expect("decode");
    let mapped = encode_response(Wire::Responses, &events).expect("encode Responses");
    assert_eq!(
        mapped.get("status").and_then(|v| v.as_str()),
        Some("incomplete"),
        "Chat length must encode as Responses incomplete, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/incomplete_details/reason")
            .and_then(|v| v.as_str()),
        Some("max_output_tokens"),
        "Chat length must encode dest Responses incomplete_details.reason, got {mapped}"
    );
}

#[test]
fn dest_responses_complete_content_filter_is_incomplete() {
    let events = [IrStreamEvent::FinishReason {
        reason: "content_filter".into(),
    }];
    let mapped = encode_response(Wire::Responses, &events).expect("encode dest Responses");
    assert_eq!(
        mapped.get("status").and_then(|v| v.as_str()),
        Some("incomplete"),
        "IR content_filter must dest-encode dest Responses status incomplete, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/incomplete_details/reason")
            .and_then(|v| v.as_str()),
        Some("content_filter"),
        "IR content_filter must dest-encode incomplete_details.reason, got {mapped}"
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
fn responses_complete_encrypted_reasoning_is_one_protocol() {
    let body = serde_json::to_vec(&json!({
        "status": "completed",
        "usage": { "input_tokens": 1, "output_tokens": 1 },
        "output": [{
            "type": "reasoning",
            "encrypted_content": "enc"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile())
        .expect("encrypted reasoning must decode");
    let protocols = events
        .iter()
        .filter(|ev| {
            matches!(
                ev,
                IrStreamEvent::Protocol { item_type, .. } if item_type == "reasoning"
            )
        })
        .count();
    assert_eq!(
        protocols, 1,
        "encrypted reasoning must be one Protocol, got {events:?}"
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

fn converse_profile() -> ResolvedProfile {
    parse_profile_str(
        r#"
schema_version = 1
id = "amazon-bedrock"
wire = "converse"
"#,
    )
    .expect("converse profile")
}

#[test]
fn converse_complete_round_trip_text() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [{ "text": "pong" }]
            }
        },
        "stopReason": "end_turn",
        "usage": { "inputTokens": 3, "outputTokens": 1 }
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile()).expect("decode");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "pong")),
        "got {events:?}"
    );
    let mapped = encode_response(Wire::Converse, &events).expect("encode");
    assert_eq!(
        mapped
            .pointer("/output/message/content/0/text")
            .and_then(|v| v.as_str()),
        Some("pong"),
        "got {mapped}"
    );
}

#[test]
fn converse_complete_tool_calls_finish_stays_tool_use() {
    let events = [
        IrStreamEvent::ToolCallStart {
            id: "t1".into(),
            name: "lookup".into(),
            thought_signature: None,
            index: 0,
        },
        IrStreamEvent::ToolCallArgDelta {
            delta: r#"{"q":"x"}"#.into(),
            index: 0,
        },
        IrStreamEvent::ToolCallEnd,
        IrStreamEvent::FinishReason {
            reason: "tool_calls".into(),
        },
    ];
    let mapped = encode_response(Wire::Converse, &events).expect("encode");
    assert_eq!(
        mapped.get("stopReason").and_then(serde_json::Value::as_str),
        Some("tool_use"),
        "tool_calls must map to tool_use, got {mapped}"
    );
    assert_eq!(
        mapped.pointer("/output/message/content/0/toolUse/input"),
        Some(&json!({"q": "x"})),
        "got {mapped}"
    );
}

#[test]
fn converse_complete_keeps_stop_sequence_and_guardrail() {
    let stop = encode_response(
        Wire::Converse,
        &[IrStreamEvent::FinishReason {
            reason: "stop_sequence".into(),
        }],
    )
    .expect("encode stop_sequence");
    assert_eq!(
        stop.get("stopReason").and_then(serde_json::Value::as_str),
        Some("stop_sequence"),
        "got {stop}"
    );
    let guard = encode_response(
        Wire::Converse,
        &[IrStreamEvent::FinishReason {
            reason: "guardrail_intervened".into(),
        }],
    )
    .expect("encode guardrail");
    assert_eq!(
        guard.get("stopReason").and_then(serde_json::Value::as_str),
        Some("guardrail_intervened"),
        "got {guard}"
    );
}

#[test]
fn converse_complete_non_json_tool_input_stays_string() {
    let events = [
        IrStreamEvent::ToolCallStart {
            id: "t1".into(),
            name: "lookup".into(),
            thought_signature: None,
            index: 0,
        },
        IrStreamEvent::ToolCallArgDelta {
            delta: "not-json".into(),
            index: 0,
        },
        IrStreamEvent::ToolCallEnd,
        IrStreamEvent::FinishReason {
            reason: "tool_calls".into(),
        },
    ];
    let mapped = encode_response(Wire::Converse, &events).expect("encode");
    let input = mapped.pointer("/output/message/content/0/toolUse/input");
    assert_ne!(
        input,
        Some(&json!({})),
        "invalid JSON must not become empty object, got {mapped}"
    );
    assert_eq!(
        input,
        Some(&json!("not-json")),
        "invalid JSON must stay a string, got {mapped}"
    );
    assert_eq!(
        mapped.get("stopReason").and_then(serde_json::Value::as_str),
        Some("tool_use"),
        "got {mapped}"
    );
}

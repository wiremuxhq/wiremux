//! Complete-body maps. Stream goldens stay delta-only.

use serde_json::json;
use wiremux::{
    IrStreamEvent, LossAction, RawSse, ResolvedProfile, StreamEncoder, Wire, decode_response,
    decode_response_with_loss, decode_stream_events, encode_response, encode_response_with_model,
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
fn crate_root_encode_response_omits_empty_dest_model() {
    let events = [IrStreamEvent::TextDelta { text: "Hi".into() }];
    let mapped = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat");
    assert!(
        mapped.get("model").is_none(),
        "empty dest model must omit dest Chat model, got {mapped}"
    );
    assert_eq!(
        mapped.get("id").and_then(serde_json::Value::as_str),
        Some("chatcmpl-wiremux"),
        "dest identity stays chatcmpl-wiremux, got {mapped}"
    );
}

#[test]
fn crate_root_encode_response_with_model_stamps_dest_chat() {
    let events = [IrStreamEvent::TextDelta { text: "Hi".into() }];
    let mapped = encode_response_with_model(Wire::ChatCompletions, &events, "gpt-4o")
        .expect("encode dest Chat");
    assert_eq!(
        mapped.get("model").and_then(serde_json::Value::as_str),
        Some("gpt-4o"),
        "crate-root encode_response_with_model must stamp dest Chat model, got {mapped}"
    );
}

#[test]
fn crate_root_stream_encoder_with_model_stamps_dest_chat() {
    let mut enc = StreamEncoder::new(Wire::ChatCompletions).with_model("gpt-4o");
    let frames = enc
        .push(IrStreamEvent::TextDelta { text: "Hi".into() })
        .expect("push dest Chat");
    assert!(
        frames.iter().any(|frame| {
            serde_json::from_str::<serde_json::Value>(&frame.data)
                .ok()
                .and_then(|v| {
                    v.get("model")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                == Some("gpt-4o".to_string())
        }),
        "StreamEncoder::with_model must stamp dest Chat STREAM model, got {frames:?}"
    );
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
                ..
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
fn dest_chat_complete_decode_annotations_url_citation_remaps_dest_responses_stream() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "See https://example.com for more.",
                "annotations": [{
                    "type": "url_citation",
                    "url_citation": {
                        "start_index": 4,
                        "end_index": 23,
                        "title": "Example Domain",
                        "url": "https://example.com"
                    }
                }]
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete annotations");
    let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
    let mut frames = Vec::new();
    for ev in events {
        frames.extend(enc.push(ev).expect("push dest Responses"));
    }
    frames.extend(enc.finish().expect("finish dest Responses"));
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.output_text.annotation.added")
                && frame.data.contains("https://example.com")
        }),
        "dest Chat complete url_citation remapped dest Responses STREAM must emit response.output_text.annotation.added, got {frames:?}"
    );
}

#[test]
fn dest_chat_complete_decode_message_audio_remaps_dest_responses_stream() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "audio": {
                    "id": "audio_1",
                    "data": "SUQz",
                    "transcript": "hello there"
                }
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete audio");
    let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
    let mut frames = Vec::new();
    for ev in events {
        frames.extend(enc.push(ev).expect("push dest Responses"));
    }
    frames.extend(enc.finish().expect("finish dest Responses"));
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.audio.delta")
                && frame.data.contains(r#""delta":"SUQz""#)
        }),
        "dest Chat complete message.audio remapped dest Responses STREAM must emit response.audio.delta, got {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.audio.transcript.delta")
                && frame.data.contains(r#""delta":"hello there""#)
        }),
        "dest Chat complete message.audio remapped dest Responses STREAM must emit response.audio.transcript.delta, got {frames:?}"
    );
}

#[test]
fn dest_chat_complete_decode_custom_tool_call_remaps_dest_responses_stream() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_custom",
                    "type": "custom",
                    "custom": {
                        "name": "code_exec",
                        "input": "print(1)"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete custom tool");
    assert!(
        !events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Protocol { item_type, .. } if item_type == "chunk"
        )),
        "dest Chat complete type=custom must not become Protocol chunk, got {events:?}"
    );
    let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
    let mut frames = Vec::new();
    for ev in events {
        frames.extend(enc.push(ev).expect("push dest Responses"));
    }
    frames.extend(enc.finish().expect("finish dest Responses"));
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.custom_tool_call_input.delta")
                && frame.data.contains(r#""delta":"print(1)""#)
        }),
        "dest Chat complete custom tool remapped dest Responses STREAM must emit response.custom_tool_call_input.delta, got {frames:?}"
    );
}

#[test]
fn dest_chat_complete_encode_annotations_audio_and_custom_round_trip() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "See https://example.com for more.",
                "annotations": [{
                    "type": "url_citation",
                    "url_citation": {
                        "start_index": 4,
                        "end_index": 23,
                        "title": "Example Domain",
                        "url": "https://example.com"
                    }
                }],
                "audio": {
                    "data": "SUQz",
                    "transcript": "hello there"
                },
                "tool_calls": [{
                    "id": "call_custom",
                    "type": "custom",
                    "custom": {
                        "name": "code_exec",
                        "input": "print(1)"
                    }
                }]
            }
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete leftovers");
    let mapped = encode_response(Wire::ChatCompletions, &events)
        .expect("encode dest Chat complete leftovers");
    assert!(
        mapped
            .pointer("/choices/0/message/annotations")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|anns| anns.iter().any(|ann| {
                ann.pointer("/url_citation/url")
                    .and_then(serde_json::Value::as_str)
                    == Some("https://example.com")
                    || ann.get("url").and_then(serde_json::Value::as_str)
                        == Some("https://example.com")
            })),
        "dest Chat complete encode must write message.annotations, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/choices/0/message/audio/data")
            .and_then(serde_json::Value::as_str),
        Some("SUQz"),
        "dest Chat complete encode must write message.audio.data, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/choices/0/message/audio/transcript")
            .and_then(serde_json::Value::as_str),
        Some("hello there"),
        "dest Chat complete encode must write message.audio.transcript, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/choices/0/message/tool_calls/0/type")
            .and_then(serde_json::Value::as_str),
        Some("custom"),
        "dest Chat complete encode must write custom tool_calls, got {mapped}"
    );
}

#[test]
fn dest_responses_complete_encode_annotations_and_custom_tool_call() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "See https://example.com for more.",
                "annotations": [{
                    "type": "url_citation",
                    "url_citation": {
                        "start_index": 4,
                        "end_index": 23,
                        "title": "Example Domain",
                        "url": "https://example.com"
                    }
                }],
                "tool_calls": [{
                    "id": "call_custom",
                    "type": "custom",
                    "custom": {
                        "name": "code_exec",
                        "input": "print(1)"
                    }
                }]
            }
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete leftovers");
    let mapped = encode_response(Wire::Responses, &events)
        .expect("encode dest Responses complete leftovers");
    let text_part = mapped
        .get("output")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                item.get("content")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|parts| {
                        parts.iter().find(|part| {
                            part.get("type").and_then(serde_json::Value::as_str)
                                == Some("output_text")
                        })
                    })
            })
        });
    assert!(
        text_part.is_some_and(|part| part
            .get("annotations")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|anns| anns.iter().any(|ann| ann
                .get("url")
                .and_then(serde_json::Value::as_str)
                == Some("https://example.com")
                || ann
                    .pointer("/url_citation/url")
                    .and_then(serde_json::Value::as_str)
                    == Some("https://example.com")))),
        "dest Responses complete output_text must keep annotations, got {mapped}"
    );
    assert!(
        mapped
            .get("output")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().any(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("custom_tool_call")
                    && item.get("name").and_then(serde_json::Value::as_str) == Some("code_exec")
                    && item.get("input").and_then(serde_json::Value::as_str) == Some("print(1)")
            })),
        "dest Responses complete must emit custom_tool_call output, got {mapped}"
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
fn gemini_unknown_finish_reason_is_preserved_on_complete() {
    let body = serde_json::to_vec(&json!({
        "candidates": [{
            "finishReason": "FUTURE_REASON",
            "content": { "role": "model", "parts": [{ "text": "x" }] }
        }]
    }))
    .expect("json");
    let (events, loss) = decode_response_with_loss(Wire::Gemini, &body, &gemini_profile())
        .expect("unknown finish must decode");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::FinishReason { reason } if reason == "FUTURE_REASON"
        )),
        "complete FUTURE_REASON must not become stop, got {events:?}"
    );
    assert!(
        loss.events.iter().any(|event| {
            event.path == "candidates[0].finishReason"
                && event.action == LossAction::Preserve
                && event.detail == "FUTURE_REASON"
        }),
        "preserved finish reason must be on the loss report, got {loss:?}"
    );

    let stop = serde_json::to_vec(&json!({
        "candidates": [{
            "finishReason": "STOP",
            "content": { "parts": [{ "text": "x" }] }
        }]
    }))
    .expect("json");
    let (events, loss) =
        decode_response_with_loss(Wire::Gemini, &stop, &gemini_profile()).expect("STOP");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "STOP stays stop, got {events:?}"
    );
    assert!(
        loss.events.is_empty(),
        "known STOP is not a loss, got {loss:?}"
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
                    "id": "call_hosted",
                    "type": "mcp",
                    "mcp": { "name": "browser" }
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
        "complete tool_calls type other than function/custom must be Protocol, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ToolCallStart { .. })),
        "must not invent a function ToolCallStart: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::CustomToolCallStart { .. })),
        "unknown tool type must not invent CustomToolCallStart: {events:?}"
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

#[test]
fn converse_complete_parallel_tool_use_indexes() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [
                    { "text": "hi" },
                    { "toolUse": { "toolUseId": "t0", "name": "a", "input": { "q": 1 } } },
                    { "toolUse": { "toolUseId": "t1", "name": "b", "input": { "q": 2 } } }
                ]
            }
        },
        "stopReason": "tool_use"
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile()).expect("decode");
    let starts: Vec<u32> = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    let deltas: Vec<u32> = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        [0, 1],
        "toolUse blocks number 0, 1; text does not take a slot, got {events:?}"
    );
    assert_eq!(
        deltas,
        [0, 1],
        "argument deltas share the tool index, got {events:?}"
    );
}

#[test]
fn responses_complete_parallel_function_call_indexes() {
    let body = serde_json::to_vec(&json!({
        "status": "completed",
        "output": [
            {
                "type": "message",
                "content": [{ "type": "output_text", "text": "hi" }]
            },
            {
                "type": "function_call",
                "call_id": "c0",
                "name": "a",
                "arguments": "{\"q\":1}"
            },
            {
                "type": "function_call",
                "call_id": "c1",
                "name": "b",
                "arguments": "{\"q\":2}"
            }
        ]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile()).expect("decode");
    let starts: Vec<u32> = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    let deltas: Vec<(u32, &str)> = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { index, delta } => Some((*index, delta.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        [0, 1],
        "function_call items number 0, 1; a message does not take a slot, got {events:?}"
    );
    assert_eq!(
        deltas,
        [(0, r#"{"q":1}"#), (1, r#"{"q":2}"#)],
        "argument deltas share the tool index, got {events:?}"
    );
}

#[test]
fn responses_complete_custom_tool_shares_function_call_index() {
    let body = serde_json::to_vec(&json!({
        "output": [
            { "type": "message", "content": [{ "type": "output_text", "text": "hi" }] },
            { "type": "function_call", "call_id": "c0", "name": "a", "arguments": "{}" },
            { "type": "custom_tool_call", "call_id": "c1", "name": "b", "input": "x" }
        ]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile()).expect("decode");
    let slots: Vec<(&str, u32)> = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart { name, index, .. } => Some((name.as_str(), *index)),
            IrStreamEvent::CustomToolCallStart { name, index, .. } => Some((name.as_str(), *index)),
            _ => None,
        })
        .collect();
    assert_eq!(
        slots,
        [("a", 0), ("b", 1)],
        "custom_tool_call shares the tool slot counter, got {events:?}"
    );
}

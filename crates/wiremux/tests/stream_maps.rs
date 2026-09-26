//! Stream-map goldens. Fixtures are redacted SSE, not live captures.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use wiremux::{
    IrStreamEvent, MapError, RawSse, ResolvedProfile, StreamEncoder, ToolCallAssembler, Wire,
    decode_response, decode_stream_event, decode_stream_events, encode_response,
    encode_stream_event, parse_profile_str,
};

fn golden(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens/streams")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn profile(toml: &str) -> ResolvedProfile {
    parse_profile_str(toml).expect("test profile parses")
}

fn messages_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-messages"
wire = "messages"
"#,
    )
}

fn chat_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-chat"
wire = "chat-completions"
"#,
    )
}

fn responses_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-responses"
wire = "responses"
"#,
    )
}

fn gemini_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-gemini"
wire = "gemini"
"#,
    )
}

fn converse_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-converse"
wire = "converse"
"#,
    )
}

fn decode_all(
    wire: Wire,
    text: &str,
    profile: &ResolvedProfile,
) -> Result<Vec<IrStreamEvent>, MapError> {
    let mut out = Vec::new();
    for raw in RawSse::parse_all(text) {
        out.extend(decode_stream_events(wire, &raw, profile)?);
    }
    Ok(out)
}

#[test]
fn anthropic_tool_use_and_thinking_golden() {
    let events = decode_all(
        Wire::Messages,
        &golden("anthropic_tool_use_thinking.sse"),
        &messages_profile(),
    )
    .expect("decode Anthropic stream");

    let thinking = events.iter().find_map(|ev| match ev {
        IrStreamEvent::ReasoningDelta { text } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(
        thinking,
        Some("The user asked for the weather in San Francisco.")
    );

    let signature = events.iter().find_map(|ev| match ev {
        IrStreamEvent::ReasoningSignature { signature } => Some(signature.as_str()),
        _ => None,
    });
    assert_eq!(signature, Some("sig_redacted"));

    let start = events.iter().find_map(|ev| match ev {
        IrStreamEvent::ToolCallStart { id, name, .. } => Some((id.as_str(), name.as_str())),
        _ => None,
    });
    assert_eq!(start, Some(("toolu_redacted", "get_weather")));

    let args: String = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(args, r#"{"location":"San Francisco"}"#);

    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "tool_calls")
        ),
        "message_delta stop_reason must become FinishReason, got {events:?}"
    );
    assert!(
        events.iter().any(|ev| matches!(ev, IrStreamEvent::Done)),
        "message_stop must map to Done, got {events:?}"
    );
}

#[test]
fn chat_tool_call_delta_golden() {
    let events = decode_all(
        Wire::ChatCompletions,
        &golden("chat_tool_call_deltas.sse"),
        &chat_profile(),
    )
    .expect("decode Chat Completions stream");

    let start = events.iter().find_map(|ev| match ev {
        IrStreamEvent::ToolCallStart { id, name, .. } => Some((id.as_str(), name.as_str())),
        _ => None,
    });
    assert_eq!(start, Some(("call_redacted", "get_weather")));

    let args: String = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(args, r#"{"location":"San Francisco"}"#);

    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "tool_calls")
        ),
        "finish_reason tool_calls missing: {events:?}"
    );
    assert!(
        events.iter().any(|ev| matches!(ev, IrStreamEvent::Done)),
        "[DONE] must map to Done, got {events:?}"
    );
}

#[test]
fn index_above_128_is_rejected() {
    let chat_err = decode_all(
        Wire::ChatCompletions,
        &golden("index_over_cap.sse"),
        &chat_profile(),
    )
    .expect_err("hostile Chat tool index must fail");
    match chat_err {
        MapError::Invalid(msg) => {
            assert!(msg.contains("129"), "msg={msg}");
            assert!(msg.contains("128"), "msg={msg}");
        }
        other => panic!("expected Invalid, got {other}"),
    }

    let messages = RawSse {
        event: Some("content_block_start".into()),
        data: r#"{"type":"content_block_start","index":129,"content_block":{"type":"tool_use","id":"t","name":"f","input":{}}}"#.into(),
    };
    let msg_err = decode_stream_event(Wire::Messages, &messages, &messages_profile())
        .expect_err("hostile content block index must fail");
    match msg_err {
        MapError::Invalid(msg) => {
            assert!(msg.contains("129"), "msg={msg}");
            assert!(msg.contains("128"), "msg={msg}");
        }
        other => panic!("expected Invalid, got {other}"),
    }

    let ok_at_cap = RawSse {
        event: Some("content_block_start".into()),
        data: r#"{"type":"content_block_start","index":128,"content_block":{"type":"tool_use","id":"t","name":"f","input":{}}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Messages, &ok_at_cap, &messages_profile())
        .expect("index 128 is in range")
        .expect("tool_use start");
    assert!(matches!(
        ev,
        IrStreamEvent::ToolCallStart { ref name, .. } if name == "f"
    ));

    let responses = RawSse {
        event: Some("response.function_call_arguments.delta".into()),
        data: r#"{"type":"response.function_call_arguments.delta","output_index":129,"delta":"{"}"#
            .into(),
    };
    let resp_err = decode_stream_event(Wire::Responses, &responses, &responses_profile())
        .expect_err("hostile Responses output_index must fail");
    assert!(
        matches!(resp_err, MapError::Invalid(ref msg) if msg.contains("129") && msg.contains("128")),
        "{resp_err}"
    );
}

#[test]
fn usage_does_not_invent_cache_tokens() {
    let events = decode_all(
        Wire::ChatCompletions,
        &golden("chat_usage_no_cache.sse"),
        &chat_profile(),
    )
    .expect("decode usage chunk");
    let usage = events
        .iter()
        .find_map(|ev| match ev {
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                ..
            } => Some((
                *prompt_tokens,
                *completion_tokens,
                *cache_read_tokens,
                *cache_write_tokens,
                *reasoning_tokens,
            )),
            _ => None,
        })
        .expect("usage event");
    assert_eq!(usage, (20, 15, 0, 0, 0));

    let ev = IrStreamEvent::Usage {
        prompt_tokens: 20,
        completion_tokens: 15,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        audio_tokens: 0,
        completion_audio_tokens: 0,
    };
    for wire in [Wire::ChatCompletions, Wire::Messages, Wire::Responses] {
        let raw = encode_stream_event(wire, &ev).expect("encode usage");
        let json: Value = serde_json::from_str(&raw.data).expect("usage JSON");
        let dump = json.to_string();
        assert!(
            !dump.contains("cached_tokens"),
            "{wire:?} invented cached_tokens: {dump}"
        );
        assert!(
            !dump.contains("cache_read_input_tokens"),
            "{wire:?} invented cache_read_input_tokens: {dump}"
        );
        assert!(
            !dump.contains("cache_creation_input_tokens"),
            "{wire:?} invented cache_creation_input_tokens: {dump}"
        );
        assert!(
            !dump.contains("audio_tokens"),
            "{wire:?} invented audio_tokens: {dump}"
        );
    }
}

fn usage_tuple(events: &[IrStreamEvent]) -> (u32, u32, u32, u32, u32) {
    events
        .iter()
        .find_map(|ev| match ev {
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                ..
            } => Some((
                *prompt_tokens,
                *completion_tokens,
                *cache_read_tokens,
                *cache_write_tokens,
                *reasoning_tokens,
            )),
            _ => None,
        })
        .expect("usage event")
}

#[test]
fn usage_cache_token_fields_are_accurate() {
    let chat = decode_all(
        Wire::ChatCompletions,
        &golden("chat_usage_with_cache.sse"),
        &chat_profile(),
    )
    .expect("decode chat cache usage");
    // Exclusive buckets: Chat prompt/completion are inclusive on the wire.
    // 100-40 cache, 20-7 reasoning. Matches exclusive usage buckets.
    assert_eq!(usage_tuple(&chat), (60, 13, 40, 0, 7));

    let messages = decode_all(
        Wire::Messages,
        &golden("anthropic_usage_with_cache.sse"),
        &messages_profile(),
    )
    .expect("decode messages cache usage");
    // Anthropic input_tokens is already exclusive of cache. output includes
    // thinking: 12-3. Matches Anthropic exclusive usage buckets.
    assert_eq!(usage_tuple(&messages), (80, 9, 25, 9, 3));

    let responses = decode_all(
        Wire::Responses,
        &golden("responses_usage_with_cache.sse"),
        &responses_profile(),
    )
    .expect("decode responses cache usage");
    // Responses input/output are inclusive like Chat: 64-16 cache, 8-2 reasoning.
    assert_eq!(usage_tuple(&responses), (48, 6, 16, 0, 2));

    let ev = IrStreamEvent::Usage {
        prompt_tokens: 80,
        completion_tokens: 12,
        cache_read_tokens: 25,
        cache_write_tokens: 9,
        reasoning_tokens: 3,
        audio_tokens: 0,
        completion_audio_tokens: 0,
    };

    let chat_json: Value = serde_json::from_str(
        &encode_stream_event(Wire::ChatCompletions, &ev)
            .unwrap()
            .data,
    )
    .unwrap();
    assert_eq!(
        chat_json.pointer("/usage/prompt_tokens"),
        Some(&Value::from(105))
    );
    assert_eq!(
        chat_json.pointer("/usage/completion_tokens"),
        Some(&Value::from(15))
    );
    assert_eq!(
        chat_json.pointer("/usage/prompt_tokens_details/cached_tokens"),
        Some(&Value::from(25))
    );
    assert_eq!(
        chat_json.pointer("/usage/prompt_tokens_details/cache_write_tokens"),
        Some(&Value::from(9))
    );
    assert_eq!(
        chat_json.pointer("/usage/completion_tokens_details/reasoning_tokens"),
        Some(&Value::from(3))
    );
    assert!(
        chat_json
            .pointer("/usage/cache_read_input_tokens")
            .is_none(),
        "Chat must not emit Anthropic cache keys: {chat_json}"
    );

    let msg_json: Value =
        serde_json::from_str(&encode_stream_event(Wire::Messages, &ev).unwrap().data).unwrap();
    assert_eq!(
        msg_json.pointer("/usage/input_tokens"),
        Some(&Value::from(80))
    );
    assert_eq!(
        msg_json.pointer("/usage/output_tokens"),
        Some(&Value::from(15))
    );
    assert_eq!(
        msg_json.pointer("/usage/cache_read_input_tokens"),
        Some(&Value::from(25))
    );
    assert_eq!(
        msg_json.pointer("/usage/cache_creation_input_tokens"),
        Some(&Value::from(9))
    );
    assert_eq!(
        msg_json.pointer("/usage/output_tokens_details/thinking_tokens"),
        Some(&Value::from(3))
    );
    assert!(
        !msg_json.to_string().contains("cached_tokens"),
        "Messages must not emit OpenAI cached_tokens: {msg_json}"
    );

    let resp_json: Value =
        serde_json::from_str(&encode_stream_event(Wire::Responses, &ev).unwrap().data).unwrap();
    assert_eq!(
        resp_json.pointer("/response/usage/input_tokens"),
        Some(&Value::from(105))
    );
    assert_eq!(
        resp_json.pointer("/response/usage/output_tokens"),
        Some(&Value::from(15))
    );
    assert_eq!(
        resp_json.pointer("/response/usage/input_tokens_details/cached_tokens"),
        Some(&Value::from(25))
    );
    assert_eq!(
        resp_json.pointer("/response/usage/input_tokens_details/cache_write_tokens"),
        Some(&Value::from(9))
    );
    assert_eq!(
        resp_json.pointer("/response/usage/output_tokens_details/reasoning_tokens"),
        Some(&Value::from(3))
    );
    assert_eq!(
        resp_json.pointer("/response/usage/total_tokens"),
        Some(&Value::from(120))
    );
}

#[test]
fn chat_usage_matches_bline_exclusive_buckets() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":800,"prompt_tokens_details":{"cached_tokens":300,"cache_write_tokens":10},"completion_tokens_details":{"reasoning_tokens":200}}}"#.into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode")
        .expect("usage");
    match ev {
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            ..
        } => {
            assert_eq!(prompt_tokens, 700);
            assert_eq!(completion_tokens, 600);
            assert_eq!(cache_read_tokens, 300);
            assert_eq!(cache_write_tokens, 10);
            assert_eq!(reasoning_tokens, 200);
        }
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn chat_top_level_cached_tokens_alias_is_read() {
    let raw = RawSse {
        event: None,
        data:
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":1,"cached_tokens":4}}"#
                .into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode top-level cached_tokens")
        .expect("usage");
    match ev {
        IrStreamEvent::Usage {
            prompt_tokens,
            cache_read_tokens,
            ..
        } => {
            assert_eq!(cache_read_tokens, 4);
            assert_eq!(prompt_tokens, 6);
        }
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn gemini_error_chunk_is_hard_error() {
    let raw = RawSse {
        event: None,
        data: r#"{"error":{"code":400,"message":"INVALID_ARGUMENT: bad fileUri"}}"#.into(),
    };
    let err = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect_err("Gemini error object must be HardError, not Ok empty");
    match err {
        MapError::HardError { path, detail } => {
            assert_eq!(path, "error");
            assert!(
                detail.contains("INVALID_ARGUMENT") && detail.contains("bad fileUri"),
                "HardError must keep the vendor message, detail={detail}"
            );
        }
        other => panic!("expected HardError, got {other}"),
    }
}

#[test]
fn gemini_candidate_text_chunk_still_decodes() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"pong"}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode")
        .expect("event");
    match ev {
        IrStreamEvent::TextDelta { text } => assert_eq!(text, "pong"),
        other => panic!("expected TextDelta, got {other:?}"),
    }
}

#[test]
fn gemini_stream_text_usage_and_thought() {
    let profile = profile(
        r#"
schema_version = 1
id = "test-gemini"
wire = "gemini"
"#,
    );
    let text = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hello"}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &text, &profile)
        .expect("decode text")
        .expect("text");
    assert!(matches!(ev, IrStreamEvent::TextDelta { ref text } if text == "hello"));

    let thought = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &thought, &profile)
        .expect("decode thought")
        .expect("thought");
    assert!(matches!(ev, IrStreamEvent::ReasoningDelta { ref text } if text == "plan"));

    let usage = RawSse {
        event: None,
        data: r#"{"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":20,"cachedContentTokenCount":40,"thoughtsTokenCount":5}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &usage, &profile)
        .expect("decode usage")
        .expect("usage");
    match ev {
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            reasoning_tokens,
            ..
        } => {
            assert_eq!(prompt_tokens, 60);
            assert_eq!(completion_tokens, 20);
            assert_eq!(cache_read_tokens, 40);
            assert_eq!(reasoning_tokens, 5);
        }
        other => panic!("expected Usage, got {other:?}"),
    }

    let encoded = encode_stream_event(
        Wire::Gemini,
        &IrStreamEvent::Usage {
            prompt_tokens: 60,
            completion_tokens: 20,
            cache_read_tokens: 40,
            cache_write_tokens: 0,
            reasoning_tokens: 5,
            audio_tokens: 0,
            completion_audio_tokens: 0,
        },
    )
    .expect("encode usage");
    let json: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(
        json.pointer("/usageMetadata/promptTokenCount"),
        Some(&Value::from(100))
    );
    assert_eq!(
        json.pointer("/usageMetadata/cachedContentTokenCount"),
        Some(&Value::from(40))
    );
}

#[test]
fn stream_function_call_thought_signature_round_trips_on_next_request() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{}},"thoughtSignature":"sig_stream_abc"}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode")
        .expect("event");
    let encoded = encode_stream_event(Wire::Gemini, &ev).expect("encode");
    let json: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(
        json.pointer("/candidates/0/content/parts/0/thoughtSignature")
            .and_then(Value::as_str),
        Some("sig_stream_abc"),
        "streamed function-call thoughtSignature must surface, got event={ev:?} json={json}"
    );
}

#[test]
fn stream_signed_function_call_keeps_nonempty_args() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{"q":"x"}},"thoughtSignature":"sig_args"}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode")
        .expect("event");
    assert!(
        matches!(
            ev,
            IrStreamEvent::ToolCallStart {
                ref name,
                thought_signature: Some(ref sig),
                ..
            } if name == "lookup" && sig == "sig_args"
        ),
        "1:1 nonempty args stay a tool-call start, got {ev:?}"
    );

    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("fan-out");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallStart {
                name,
                thought_signature: Some(sig),
                ..
            } if name == "lookup" && sig == "sig_args"
        )),
        "fan-out must emit signed ToolCallStart, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"q":"x"}"#
        )),
        "fan-out must emit ArgDelta, got {all:?}"
    );
    let delta = all
        .iter()
        .find_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .expect("ArgDelta");
    let encoded = encode_stream_event(
        Wire::Gemini,
        &IrStreamEvent::ToolCallArgDelta {
            delta: delta.to_string(),
            index: 0,
        },
    )
    .expect("encode ArgDelta");
    let json: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(
        json.pointer("/candidates/0/content/parts/0/functionCall/args/q")
            .and_then(Value::as_str),
        Some("x"),
        "encoded fan-out ArgDelta must keep nonempty args, got {json}"
    );
}

#[test]
fn gemini_decode_stream_event_keeps_function_call_with_args() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"read_file","args":{"path":"a.rs"}},"thoughtSignature":"sig-1"}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode")
        .expect("event");
    assert!(
        matches!(
            ev,
            IrStreamEvent::ToolCallStart {
                ref name,
                thought_signature: Some(ref sig),
                index: 0,
                ..
            } if name == "read_file" && sig == "sig-1"
        ),
        "singular decode must surface the function call, got {ev:?}"
    );
    assert!(
        !matches!(ev, IrStreamEvent::Protocol { ref item_type, .. } if item_type == "chunk"),
        "nonempty args must not hide the call in Protocol chunk"
    );

    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("events");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallStart {
                name,
                thought_signature: Some(sig),
                index: 0,
                ..
            } if name == "read_file" && sig == "sig-1"
        )),
        "decode_stream_events must keep ToolCallStart, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, index: 0 } if delta == r#"{"path":"a.rs"}"#
        )),
        "args belong on ToolCallArgDelta at the same index, got {all:?}"
    );
}

#[test]
fn gemini_empty_args_do_not_invent_an_argument_delta() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file","args":{}}}]}}]}"#.into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("events");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallStart { name, index: 0, .. } if name == "read_file"
        )),
        "empty args still start at index 0, got {all:?}"
    );
    assert!(
        !all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::ToolCallArgDelta { .. })),
        "empty args must not emit an argument delta, got {all:?}"
    );
}

#[test]
fn gemini_parallel_function_calls_start_at_index_zero() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file","args":{"path":"a.rs"}}},{"functionCall":{"name":"write_file","args":{"path":"b.rs"}}}]}}]}"#.into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("events");
    let slots: Vec<_> = all
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart {
                id, name, index, ..
            } => Some((id.as_str(), name.as_str(), *index)),
            _ => None,
        })
        .collect();
    assert_eq!(
        slots,
        vec![
            ("read_file#0", "read_file", 0),
            ("write_file#1", "write_file", 1)
        ],
        "id seq and index must match, got {all:?}"
    );
    let deltas: Vec<_> = all
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta, index } => Some((delta.as_str(), *index)),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec![(r#"{"path":"a.rs"}"#, 0), (r#"{"path":"b.rs"}"#, 1)],
        "argument deltas must use the same indexes, got {all:?}"
    );
}

#[test]
fn gemini_unknown_finish_reason_is_preserved() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"finishReason":"FUTURE_REASON","content":{"role":"model","parts":[{"text":"x"}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode")
        .expect("event");
    match ev {
        IrStreamEvent::TextDelta { .. } => {}
        IrStreamEvent::FinishReason { ref reason } => {
            assert_ne!(
                reason, "stop",
                "unknown finishReason must not collapse to stop"
            );
            assert_eq!(reason, "FUTURE_REASON");
        }
        other => panic!("expected text or preserved finish, got {other:?}"),
    }
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("events");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::FinishReason { reason } if reason == "FUTURE_REASON"
        )),
        "fan-out must preserve FUTURE_REASON, got {all:?}"
    );

    let stop = RawSse {
        event: None,
        data: r#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"x"}]}}]}"#
            .into(),
    };
    let stopped = decode_stream_events(Wire::Gemini, &stop, &gemini_profile()).expect("stop");
    assert!(
        stopped.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::FinishReason { reason } if reason == "stop"
        )),
        "STOP stays stop, got {stopped:?}"
    );

    let tools = RawSse {
        event: None,
        data: r#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"functionCall":{"name":"read_file","args":{}}}]}}]}"#.into(),
    };
    let tool_stop = decode_stream_events(Wire::Gemini, &tools, &gemini_profile()).expect("tools");
    assert!(
        tool_stop.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::FinishReason { reason } if reason == "tool_calls"
        )),
        "STOP with a function call stays tool_calls, got {tool_stop:?}"
    );
}

#[test]
fn gemini_thought_then_function_call_emits_both() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true},{"functionCall":{"name":"lookup","args":{"q":"x"}}}]}}]}"#.into(),
    };
    let first = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode")
        .expect("event");
    assert!(
        matches!(first, IrStreamEvent::ReasoningDelta { ref text } if text == "plan"),
        "1:1 decode stays first-part-wins, got {first:?}"
    );

    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("fan-out");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ReasoningDelta { text } if text == "plan"
        )),
        "thought part must emit ReasoningDelta, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallStart { name, .. } if name == "lookup"
        )),
        "functionCall after thought must emit ToolCallStart, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"q":"x"}"#
        )),
        "functionCall args must emit ArgDelta, got {all:?}"
    );
}

#[test]
fn gemini_text_then_function_call_emits_both() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"text":"plan"},{"functionCall":{"name":"lookup","args":{"q":"x"}}}]}}]}"#.into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("fan-out");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::TextDelta { text } if text == "plan"
        )),
        "text part must emit TextDelta, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallStart { name, .. } if name == "lookup"
        )),
        "functionCall after text must emit ToolCallStart, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"q":"x"}"#
        )),
        "functionCall args must emit ArgDelta, got {all:?}"
    );
}

#[test]
fn gemini_last_chunk_parts_keep_finish_and_usage() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"text":"done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2}}"#.into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("fan-out");
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "done")),
        "text part must stay, got {all:?}"
    );
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "same-chunk finishReason must stay, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                ..
            }
        )),
        "same-chunk usageMetadata must stay, got {all:?}"
    );
}

#[test]
fn gemini_safety_finish_reasons_are_content_filter() {
    for reason in ["RECITATION", "SPII", "OTHER", "SAFETY"] {
        let raw = RawSse {
            event: None,
            data: format!(r#"{{"candidates":[{{"finishReason":"{reason}"}}]}}"#),
        };
        let ev = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
            .expect("decode")
            .expect("event");
        assert!(
            matches!(ev, IrStreamEvent::FinishReason { ref reason } if reason == "content_filter"),
            "{reason} must be content_filter, got {ev:?}"
        );
    }
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"finishReason":"MALFORMED_FUNCTION_CALL"}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode")
        .expect("event");
    assert!(
        matches!(
            ev,
            IrStreamEvent::FinishReason { ref reason } if reason == "malformed_function_call"
        ),
        "MALFORMED_FUNCTION_CALL must not be tool_calls, got {ev:?}"
    );
}

#[test]
fn responses_reasoning_and_refusal_deltas_are_known() {
    let reasoning = RawSse {
        event: Some("response.reasoning_summary_text.delta".into()),
        data: r#"{"type":"response.reasoning_summary_text.delta","delta":"think"}"#.into(),
    };
    let ev = decode_stream_event(Wire::Responses, &reasoning, &responses_profile())
        .expect("decode reasoning")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::ReasoningDelta { ref text } if text == "think"),
        "got {ev:?}"
    );

    let refusal = RawSse {
        event: Some("response.refusal.delta".into()),
        data: r#"{"type":"response.refusal.delta","delta":"no"}"#.into(),
    };
    let ev = decode_stream_event(Wire::Responses, &refusal, &responses_profile())
        .expect("decode refusal")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::RefusalDelta { ref text } if text == "no"),
        "dest Responses refusal.delta must be RefusalDelta, got {ev:?}"
    );
}

#[test]
fn dest_chat_stream_decode_delta_refusal_is_refusal_delta() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"refusal":"nope"}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat delta.refusal")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::RefusalDelta { ref text } if text == "nope"),
        "dest Chat delta.refusal must be RefusalDelta, got {ev:?}"
    );
    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode_all dest Chat delta.refusal");
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::RefusalDelta { text } if text == "nope")),
        "decode_all must keep dest Chat delta.refusal, got {all:?}"
    );
}

#[test]
fn dest_messages_stream_decode_stop_details_explanation_is_refusal_delta() {
    let only = RawSse {
        event: Some("message_delta".into()),
        data: json!({
            "type": "message_delta",
            "delta": {
                "stop_details": {
                    "type": "refusal",
                    "explanation": "nope"
                }
            }
        })
        .to_string(),
    };
    let ev = decode_stream_event(Wire::Messages, &only, &messages_profile())
        .expect("decode dest Messages STREAM stop_details.explanation")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::RefusalDelta { ref text } if text == "nope"),
        "dest Messages STREAM delta.stop_details.explanation must be RefusalDelta, got {ev:?}"
    );
    let both = RawSse {
        event: Some("message_delta".into()),
        data: json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "refusal",
                "stop_sequence": null,
                "stop_details": {
                    "type": "refusal",
                    "explanation": "nope"
                }
            }
        })
        .to_string(),
    };
    let all = decode_stream_events(Wire::Messages, &both, &messages_profile())
        .expect("decode_all dest Messages STREAM stop_details.explanation");
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::RefusalDelta { text } if text == "nope")),
        "decode_all must keep dest Messages STREAM delta.stop_details.explanation, got {all:?}"
    );
    assert!(
        !all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "nope")),
        "dest Messages STREAM stop_details.explanation must not fold into text, got {all:?}"
    );
}

#[test]
fn dest_messages_stream_citations_delta_remaps_dest_chat_annotations() {
    let raw = RawSse {
        event: Some("content_block_delta".into()),
        data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","url":"https://example.com","title":"Example Domain","encrypted_index":"idx","cited_text":"example.com"}}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Messages, &raw, &messages_profile())
        .expect("decode dest Messages citations_delta")
        .expect("event");
    let IrStreamEvent::AnnotationAdded { ref annotation } = ev else {
        panic!("dest Messages citations_delta must be AnnotationAdded, got {ev:?}");
    };
    assert_eq!(
        annotation.get("url").and_then(Value::as_str),
        Some("https://example.com"),
        "dest Messages web_search_result_location must lift url, got {annotation}"
    );
    let frames = encode_all(Wire::ChatCompletions, std::slice::from_ref(&ev));
    assert!(
        frames.iter().any(|frame| {
            frame.data.contains("url_citation") && frame.data.contains("https://example.com")
        }),
        "dest Messages STREAM citations_delta remapped dest Chat must emit url_citation, got {frames:?}"
    );
}

#[test]
fn dest_gemini_stream_citation_metadata_remap_dest_chat_annotations() {
    let raw = RawSse {
        event: None,
        data: json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{ "text": "see" }]
                },
                "citationMetadata": {
                    "citations": [{
                        "uri": "https://example.com/b",
                        "title": "B",
                        "startIndex": 0,
                        "endIndex": 3
                    }]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini STREAM citationMetadata");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/url")
                .or_else(|| body.pointer("/choices/0/delta/annotations/0/url"))
                .and_then(Value::as_str)
        }),
        Some("https://example.com/b"),
        "dest Gemini STREAM citationMetadata remapped dest Chat STREAM must emit url_citation, got {frames:?}"
    );
}

#[test]
fn dest_gemini_stream_citation_metadata_span_remaps_dest_chat_start_index() {
    let raw = RawSse {
        event: None,
        data: json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{ "text": "see" }]
                },
                "citationMetadata": {
                    "citations": [{
                        "uri": "https://example.com/b",
                        "title": "B",
                        "startIndex": 4,
                        "endIndex": 8
                    }]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini STREAM citationMetadata span");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/start_index")
                .and_then(Value::as_u64)
        }),
        Some(4),
        "dest Gemini STREAM citationMetadata startIndex remapped dest Chat STREAM must emit start_index 4, got {frames:?}"
    );
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/end_index")
                .and_then(Value::as_u64)
        }),
        Some(8),
        "dest Gemini STREAM citationMetadata endIndex remapped dest Chat STREAM must emit end_index 8, got {frames:?}"
    );
}

#[test]
fn dest_gemini_stream_grounding_attributions_remap_dest_chat_annotations() {
    let raw = RawSse {
        event: None,
        data: json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{ "text": "see" }]
                },
                "groundingAttributions": [{
                    "sourceId": "src1",
                    "web": { "uri": "https://example.com/a", "title": "A" }
                }]
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini STREAM groundingAttributions");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/url")
                .or_else(|| body.pointer("/choices/0/delta/annotations/0/url"))
                .and_then(Value::as_str)
        }),
        Some("https://example.com/a"),
        "dest Gemini STREAM groundingAttributions remapped dest Chat STREAM must emit url_citation, got {frames:?}"
    );
}

#[test]
fn dest_gemini_stream_grounding_chunks_remap_dest_chat_annotations() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"groundingMetadata":{"groundingChunks":[{"web":{"uri":"https://example.com","title":"Example Domain"}}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini groundingMetadata")
        .expect("event");
    let IrStreamEvent::AnnotationAdded { ref annotation } = ev else {
        panic!("dest Gemini groundingChunks.web must be AnnotationAdded, got {ev:?}");
    };
    assert_eq!(
        annotation.get("url").and_then(Value::as_str),
        Some("https://example.com"),
        "dest Gemini groundingChunks.web.uri must lift url, got {annotation}"
    );
    let frames = encode_all(Wire::ChatCompletions, std::slice::from_ref(&ev));
    assert!(
        frames.iter().any(|frame| {
            frame.data.contains("url_citation") && frame.data.contains("https://example.com")
        }),
        "dest Gemini STREAM groundingMetadata remapped dest Chat must emit url_citation, got {frames:?}"
    );
}

#[test]
fn dest_gemini_stream_grounding_chunks_non_web_remap_dest_chat_annotations() {
    let cases = [
        (
            "image",
            json!({ "image": { "sourceUri": "https://example.com/img", "title": "Img" } }),
            "https://example.com/img",
        ),
        (
            "retrievedContext",
            json!({ "retrievedContext": { "uri": "https://example.com/doc", "title": "Doc" } }),
            "https://example.com/doc",
        ),
        (
            "maps",
            json!({ "maps": { "uri": "https://maps.google.com/?cid=1", "title": "Place" } }),
            "https://maps.google.com/?cid=1",
        ),
    ];
    for (kind, chunk, url) in cases {
        let raw = RawSse {
            event: None,
            data: json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{ "text": "see" }]
                    },
                    "groundingMetadata": { "groundingChunks": [chunk] }
                }]
            })
            .to_string(),
        };
        let events =
            decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).unwrap_or_else(|err| {
                panic!("decode dest Gemini STREAM groundingChunks.{kind}: {err}")
            });
        let frames = encode_all(Wire::ChatCompletions, &events);
        let bodies = sse_json_frames(&frames);
        assert_eq!(
            bodies.iter().find_map(|body| {
                body.pointer("/choices/0/delta/annotations/0/url_citation/url")
                    .or_else(|| body.pointer("/choices/0/delta/annotations/0/url"))
                    .and_then(Value::as_str)
            }),
            Some(url),
            "dest Gemini STREAM groundingChunks.{kind} remapped dest Chat STREAM must emit url_citation, got {frames:?}"
        );
    }
}

#[test]
fn dest_gemini_stream_grounding_supports_remap_dest_chat_start_index() {
    let raw = RawSse {
        event: None,
        data: json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{ "text": "see this" }]
                },
                "groundingMetadata": {
                    "groundingChunks": [{
                        "web": { "uri": "https://example.com/s", "title": "S" }
                    }],
                    "groundingSupports": [{
                        "segment": { "startIndex": 4, "endIndex": 8, "text": "this" },
                        "groundingChunkIndices": [0]
                    }]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini STREAM groundingSupports");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/url")
                .and_then(Value::as_str)
        }),
        Some("https://example.com/s"),
        "dest Gemini STREAM groundingSupports remapped dest Chat STREAM must keep url, got {frames:?}"
    );
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/start_index")
                .and_then(Value::as_u64)
        }),
        Some(4),
        "dest Gemini STREAM groundingSupports.segment.startIndex remapped dest Chat STREAM must emit start_index 4, got {frames:?}"
    );
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/end_index")
                .and_then(Value::as_u64)
        }),
        Some(8),
        "dest Gemini STREAM groundingSupports.segment.endIndex remapped dest Chat STREAM must emit end_index 8, got {frames:?}"
    );
}

#[test]
fn dest_gemini_complete_url_context_metadata_remaps_dest_chat_url_citation() {
    let body = serde_json::to_vec(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "Hi" }]
            },
            "urlContextMetadata": {
                "urlMetadata": [{
                    "retrievedUrl": "https://example.com/doc",
                    "title": "Doc"
                }]
            },
            "finishReason": "STOP"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &body, &gemini_profile())
        .expect("decode dest Gemini complete urlContextMetadata");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/annotations/0/url_citation/url")
            .and_then(Value::as_str),
        Some("https://example.com/doc"),
        "dest Gemini complete urlContextMetadata.urlMetadata.retrievedUrl remapped dest Chat complete must write url_citation.url, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/annotations/0/url_citation/title")
            .and_then(Value::as_str),
        Some("Doc"),
        "dest Gemini complete urlContextMetadata.urlMetadata.title remapped dest Chat complete must write url_citation.title, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Gemini complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_gemini_complete_citation_sources_remaps_dest_chat_url_citation() {
    let body = serde_json::to_vec(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "Hi" }]
            },
            "citationMetadata": {
                "citationSources": [{
                    "uri": "https://example.com/src",
                    "title": "Src"
                }]
            },
            "finishReason": "STOP"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &body, &gemini_profile())
        .expect("decode dest Gemini complete citationSources");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/annotations/0/url_citation/url")
            .and_then(Value::as_str),
        Some("https://example.com/src"),
        "dest Gemini complete citationMetadata.citationSources remapped dest Chat complete must write url_citation.url, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/annotations/0/url_citation/title")
            .and_then(Value::as_str),
        Some("Src"),
        "dest Gemini complete citationMetadata.citationSources title remapped dest Chat complete must write url_citation.title, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Gemini complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_gemini_stream_citation_sources_remap_dest_chat_annotations() {
    let raw = RawSse {
        event: None,
        data: json!({
            "candidates": [{
                "citationMetadata": {
                    "citationSources": [{
                        "uri": "https://example.com/src",
                        "title": "Src"
                    }]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini STREAM citationSources");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/url")
                .or_else(|| body.pointer("/choices/0/delta/annotations/0/url"))
                .and_then(Value::as_str)
        }),
        Some("https://example.com/src"),
        "dest Gemini STREAM citationMetadata.citationSources remapped dest Chat STREAM must emit url_citation, got {frames:?}"
    );
}

#[test]
fn dest_gemini_stream_url_context_metadata_remap_dest_chat_annotations() {
    let raw = RawSse {
        event: None,
        data: json!({
            "candidates": [{
                "urlContextMetadata": {
                    "urlMetadata": [{
                        "retrievedUrl": "https://example.com/doc",
                        "title": "Doc"
                    }]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini STREAM urlContextMetadata");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/annotations/0/url_citation/url")
                .or_else(|| body.pointer("/choices/0/delta/annotations/0/url"))
                .and_then(Value::as_str)
        }),
        Some("https://example.com/doc"),
        "dest Gemini STREAM urlContextMetadata.urlMetadata remapped dest Chat STREAM must emit url_citation, got {frames:?}"
    );
}

#[test]
fn dest_converse_stream_citation_remap_dest_chat_annotations() {
    let raw = RawSse {
        event: None,
        data: r#"{"contentBlockDelta":{"contentBlockIndex":0,"delta":{"citation":{"title":"Example Domain","source":"https://example.com","location":{"web":{"url":"https://example.com"}}}}}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Converse, &raw, &{
        profile(
            r#"
schema_version = 1
id = "test-converse"
wire = "converse"
"#,
        )
    })
    .expect("decode dest Converse citation")
    .expect("event");
    let IrStreamEvent::AnnotationAdded { ref annotation } = ev else {
        panic!("dest Converse citation must be AnnotationAdded, got {ev:?}");
    };
    assert_eq!(
        annotation.get("url").and_then(Value::as_str),
        Some("https://example.com"),
        "dest Converse citation location.web.url must lift url, got {annotation}"
    );
    let frames = encode_all(Wire::ChatCompletions, std::slice::from_ref(&ev));
    assert!(
        frames.iter().any(|frame| {
            frame.data.contains("url_citation") && frame.data.contains("https://example.com")
        }),
        "dest Converse STREAM citation remapped dest Chat must emit url_citation, got {frames:?}"
    );
}

#[test]
fn dest_converse_stream_reasoning_signature_remaps_dest_chat() {
    let raw = RawSse {
        event: None,
        data: r#"{"contentBlockDelta":{"delta":{"reasoningContent":{"signature":"sig-1"}}}}"#
            .into(),
    };
    let ev = decode_stream_event(Wire::Converse, &raw, &converse_profile())
        .expect("decode dest Converse STREAM reasoningContent.signature")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::ReasoningSignature { ref signature } if signature == "sig-1"),
        "dest Converse STREAM reasoningContent.signature must be ReasoningSignature, got {ev:?}"
    );
    let encoded = encode_stream_event(Wire::ChatCompletions, &ev).expect("encode dest Chat STREAM");
    let chat: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(
        chat.pointer("/choices/0/delta/reasoning_signature")
            .and_then(Value::as_str),
        Some("sig-1"),
        "dest Converse STREAM reasoningContent.signature remapped dest Chat STREAM must write delta.reasoning_signature, got {chat}"
    );
}

#[test]
fn dest_messages_complete_text_citations_remap_dest_chat_annotations() {
    let body = serde_json::to_vec(&json!({
        "content": [{
            "type": "text",
            "text": "See https://example.com for more.",
            "citations": [{
                "type": "web_search_result_location",
                "url": "https://example.com",
                "title": "Example Domain",
                "encrypted_index": "idx",
                "cited_text": "example.com"
            }]
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Messages, &body, &messages_profile())
        .expect("decode dest Messages complete citations");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::AnnotationAdded { annotation }
                if annotation.get("url").and_then(Value::as_str) == Some("https://example.com")
        )),
        "dest Messages complete text.citations must be AnnotationAdded, got {events:?}"
    );
    let mapped = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat");
    assert_eq!(
        mapped
            .pointer("/choices/0/message/annotations/0/url_citation/url")
            .and_then(Value::as_str),
        Some("https://example.com"),
        "dest Messages complete citations remapped dest Chat must write message.annotations url_citation, got {mapped}"
    );
}

#[test]
fn dest_chat_stream_encode_refusal_delta_is_delta_refusal() {
    let raw = encode_stream_event(
        Wire::ChatCompletions,
        &IrStreamEvent::RefusalDelta {
            text: "nope".into(),
        },
    )
    .expect("encode dest Chat refusal");
    let body: Value = serde_json::from_str(&raw.data).expect("json");
    assert_eq!(
        body.pointer("/choices/0/delta/refusal")
            .and_then(Value::as_str),
        Some("nope"),
        "dest Chat stream encode must write delta.refusal, got {}",
        raw.data
    );
    assert!(
        body.pointer("/choices/0/delta/content").is_none()
            || body
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty),
        "dest Chat stream refusal must not write dest Chat content, got {}",
        raw.data
    );
}

#[test]
fn dest_messages_stream_encode_refusal_delta_is_stop_details() {
    let raw = encode_stream_event(
        Wire::Messages,
        &IrStreamEvent::RefusalDelta {
            text: "nope".into(),
        },
    )
    .expect("encode dest Messages STREAM refusal");
    assert_eq!(
        raw.event.as_deref(),
        Some("message_delta"),
        "dest Messages STREAM encode must use message_delta for stop_details, got {raw:?}"
    );
    let body: Value = serde_json::from_str(&raw.data).expect("json");
    assert_eq!(
        body.pointer("/delta/stop_details/explanation")
            .and_then(Value::as_str),
        Some("nope"),
        "dest Messages STREAM encode must write delta.stop_details.explanation, got {}",
        raw.data
    );
    assert_eq!(
        body.pointer("/delta/stop_details/type")
            .and_then(Value::as_str),
        Some("refusal"),
        "dest Messages STREAM encode must write delta.stop_details.type refusal, got {}",
        raw.data
    );
    assert_ne!(
        body.pointer("/delta/type").and_then(Value::as_str),
        Some("text_delta"),
        "dest Messages STREAM refusal must not fold into text_delta, got {}",
        raw.data
    );
}

#[test]
fn dest_responses_stream_encode_refusal_delta_is_refusal_event() {
    let raw = encode_stream_event(
        Wire::Responses,
        &IrStreamEvent::RefusalDelta {
            text: "nope".into(),
        },
    )
    .expect("encode dest Responses refusal");
    assert_eq!(
        raw.event.as_deref(),
        Some("response.refusal.delta"),
        "dest Responses stream encode must use response.refusal.delta, got {raw:?}"
    );
    let body: Value = serde_json::from_str(&raw.data).expect("json");
    assert_eq!(
        body.get("type").and_then(Value::as_str),
        Some("response.refusal.delta")
    );
    assert_eq!(body.get("delta").and_then(Value::as_str), Some("nope"));
    assert!(
        body.get("output_index").is_some(),
        "dest Responses refusal.delta must follow TextDelta shape with output_index, got {}",
        raw.data
    );
}

#[test]
fn dest_chat_complete_annotations_url_citation_remap_dest_responses_stream() {
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
    let frames = encode_all(Wire::Responses, &events);
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.output_text.annotation.added")
                && frame.data.contains("https://example.com")
        }),
        "dest Chat complete url_citation remapped dest Responses STREAM must emit response.output_text.annotation.added, got {frames:?}"
    );
    let item_done = frames.iter().find(|frame| {
        frame.event.as_deref() == Some("response.output_item.done")
            && frame.data.contains("\"type\":\"output_text\"")
    });
    if let Some(item_done) = item_done {
        assert!(
            item_done.data.contains("annotations")
                && item_done.data.contains("https://example.com"),
            "dest Responses output_item.done output_text must keep annotations, got {}",
            item_done.data
        );
    }
}

fn dest_chat_complete_url_citation_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
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
    .expect("json")
}

#[test]
fn dest_chat_stream_annotations_url_citation_remap_dest_messages_and_gemini() {
    let raw = RawSse {
        event: None,
        data: json!({
            "choices": [{
                "delta": {
                    "content": "see",
                    "annotations": [{
                        "type": "url_citation",
                        "url_citation": {
                            "url": "https://example.com/a",
                            "title": "A",
                            "start_index": 0,
                            "end_index": 3
                        }
                    }]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM annotations");
    let messages = encode_all(Wire::Messages, &events);
    let messages_bodies = sse_json_frames(&messages);
    assert_eq!(
        messages_bodies
            .iter()
            .find_map(|body| { body.pointer("/delta/citation/url").and_then(Value::as_str) }),
        Some("https://example.com/a"),
        "dest Chat STREAM annotations remapped dest Messages STREAM must emit citations_delta url, got {messages:?}"
    );
    let gemini = encode_all(Wire::Gemini, &events);
    let gemini_bodies = sse_json_frames(&gemini);
    assert_eq!(
        gemini_bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/groundingMetadata/groundingChunks/0/web/uri")
                .and_then(Value::as_str)
        }),
        Some("https://example.com/a"),
        "dest Chat STREAM annotations remapped dest Gemini STREAM must emit groundingChunks web uri, got {gemini:?}"
    );
}

#[test]
fn dest_chat_stream_annotation_span_remaps_dest_gemini_grounding_supports() {
    let raw = RawSse {
        event: None,
        data: json!({
            "choices": [{
                "delta": {
                    "content": "see this",
                    "annotations": [{
                        "type": "url_citation",
                        "url_citation": {
                            "url": "https://example.com/s",
                            "title": "S",
                            "start_index": 4,
                            "end_index": 8
                        }
                    }]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM annotation span");
    let frames = encode_all(Wire::Gemini, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/groundingMetadata/groundingSupports/0/segment/startIndex")
                .and_then(Value::as_u64)
        }),
        Some(4),
        "dest Chat STREAM url_citation start_index remapped dest Gemini STREAM must emit groundingSupports.segment.startIndex 4, got {frames:?}"
    );
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/groundingMetadata/groundingSupports/0/segment/endIndex")
                .and_then(Value::as_u64)
        }),
        Some(8),
        "dest Chat STREAM url_citation end_index remapped dest Gemini STREAM must emit groundingSupports.segment.endIndex 8, got {frames:?}"
    );
}

#[test]
fn dest_chat_complete_annotations_url_citation_remap_dest_messages_stream() {
    let events = decode_response(
        Wire::ChatCompletions,
        &dest_chat_complete_url_citation_body(),
        &chat_profile(),
    )
    .expect("decode dest Chat complete annotations");
    let frames = encode_all(Wire::Messages, &events);
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("content_block_delta")
                && frame.data.contains("citations_delta")
                && frame.data.contains("web_search_result_location")
                && frame.data.contains("https://example.com")
        }),
        "dest Chat complete url_citation remapped dest Messages STREAM must emit citations_delta web_search_result_location, got {frames:?}"
    );
}

#[test]
fn dest_chat_complete_annotations_url_citation_remap_dest_gemini_stream() {
    let events = decode_response(
        Wire::ChatCompletions,
        &dest_chat_complete_url_citation_body(),
        &chat_profile(),
    )
    .expect("decode dest Chat complete annotations");
    let frames = encode_all(Wire::Gemini, &events);
    assert!(
        frames.iter().any(|frame| {
            frame.data.contains("groundingMetadata")
                && frame.data.contains("groundingChunks")
                && frame.data.contains("https://example.com")
        }),
        "dest Chat complete url_citation remapped dest Gemini STREAM must emit groundingMetadata.groundingChunks, got {frames:?}"
    );
}

#[test]
fn dest_chat_complete_annotations_url_citation_remap_dest_converse_stream() {
    let events = decode_response(
        Wire::ChatCompletions,
        &dest_chat_complete_url_citation_body(),
        &chat_profile(),
    )
    .expect("decode dest Chat complete annotations");
    let frames = encode_all(Wire::Converse, &events);
    assert!(
        frames.iter().any(|frame| {
            let Ok(body) = serde_json::from_str::<Value>(&frame.data) else {
                return false;
            };
            body.pointer("/contentBlockDelta/delta/citation/location/web/url")
                .and_then(Value::as_str)
                == Some("https://example.com")
        }),
        "dest Chat complete url_citation remapped dest Converse STREAM must emit contentBlockDelta.delta.citation.location.web.url, got {frames:?}"
    );
}

fn dest_chat_complete_audio_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
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
    .expect("json")
}

#[test]
fn dest_chat_complete_audio_transcript_remap_dest_messages_stream() {
    let events = decode_response(
        Wire::ChatCompletions,
        &dest_chat_complete_audio_body(),
        &chat_profile(),
    )
    .expect("decode dest Chat complete audio");
    let frames = encode_all(Wire::Messages, &events);
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("content_block_delta")
                && frame.data.contains("text_delta")
                && frame.data.contains("hello there")
        }),
        "dest Chat audio.transcript remapped dest Messages STREAM must emit text_delta, got {frames:?}"
    );
    assert!(
        frames.iter().all(|frame| {
            frame.event.as_deref() != Some("content_block_delta")
                || !frame.data.contains(r#""text":"""#)
        }),
        "dest Messages STREAM must not emit empty text_delta for dest Chat audio bytes, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_audio_remaps_dest_gemini_and_responses() {
    let raw = RawSse {
        event: None,
        data: json!({
            "choices": [{
                "delta": {
                    "audio": {
                        "id": "audio_1",
                        "data": "SUQz",
                        "transcript": "hello there"
                    }
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM audio");
    let gemini = encode_all(Wire::Gemini, &events);
    assert!(
        gemini.iter().any(|frame| {
            frame.data.contains("inlineData")
                && frame.data.contains("SUQz")
                && frame.data.contains("audio/mpeg")
        }),
        "dest Chat STREAM delta.audio.data remapped dest Gemini STREAM must emit inlineData, got {gemini:?}"
    );
    let messages = encode_all(Wire::Messages, &events);
    assert!(
        messages
            .iter()
            .any(|frame| frame.data.contains("hello there")),
        "dest Chat STREAM delta.audio.transcript remapped dest Messages STREAM must emit text, got {messages:?}"
    );
    let responses = encode_all(Wire::Responses, &events);
    assert!(
        responses.iter().any(|frame| {
            frame.event.as_deref() == Some("response.audio.delta")
                && frame.data.contains(r#""delta":"SUQz""#)
        }),
        "dest Chat STREAM delta.audio.data remapped dest Responses STREAM must emit response.audio.delta, got {responses:?}"
    );
    assert!(
        responses.iter().any(|frame| {
            frame.event.as_deref() == Some("response.audio.transcript.delta")
                && frame.data.contains(r#""delta":"hello there""#)
        }),
        "dest Chat STREAM delta.audio.transcript remapped dest Responses STREAM must emit response.audio.transcript.delta, got {responses:?}"
    );
}

#[test]
fn dest_chat_complete_audio_data_remap_dest_gemini_stream() {
    let events = decode_response(
        Wire::ChatCompletions,
        &dest_chat_complete_audio_body(),
        &chat_profile(),
    )
    .expect("decode dest Chat complete audio");
    let frames = encode_all(Wire::Gemini, &events);
    assert!(
        frames.iter().any(|frame| {
            frame.data.contains("inlineData")
                && frame.data.contains("SUQz")
                && frame.data.contains("audio/mpeg")
        }),
        "dest Chat audio.data remapped dest Gemini STREAM must emit inlineData, got {frames:?}"
    );
}

#[test]
fn dest_chat_complete_message_audio_remap_dest_responses_stream() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "audio": {
                    "id": "audio_1",
                    "data": "SUQz",
                    "expires_at": 1_700_000_000,
                    "transcript": "hello there"
                }
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete audio");
    let frames = encode_all(Wire::Responses, &events);
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.audio.delta")
                && frame.data.contains(r#""delta":"SUQz""#)
        }),
        "dest Chat complete message.audio.data remapped dest Responses STREAM must emit response.audio.delta, got {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.audio.transcript.delta")
                && frame.data.contains(r#""delta":"hello there""#)
        }),
        "dest Chat complete message.audio.transcript remapped dest Responses STREAM must emit response.audio.transcript.delta, got {frames:?}"
    );
}

#[test]
fn dest_responses_complete_output_audio_remaps_dest_chat_message_audio() {
    let body = serde_json::to_vec(&json!({
        "id": "resp_1",
        "object": "response",
        "status": "completed",
        "output": [{
            "type": "output_audio",
            "data": "SUQz",
            "transcript": "hello there"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile())
        .expect("decode dest Responses complete output_audio");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/audio/data")
            .and_then(Value::as_str),
        Some("SUQz"),
        "dest Responses complete output_audio remapped dest Chat complete must write message.audio.data, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/audio/transcript")
            .and_then(Value::as_str),
        Some("hello there"),
        "dest Responses complete output_audio remapped dest Chat complete must write message.audio.transcript, got {chat}"
    );
}

#[test]
fn dest_chat_complete_message_audio_remaps_dest_responses_complete_output_audio() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-audio",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o-audio-preview",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "audio": {
                    "data": "SUQz",
                    "transcript": "hello there"
                }
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete message.audio");
    let mapped = encode_response(Wire::Responses, &events).expect("encode dest Responses complete");
    let audio = mapped
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("output_audio"));
    assert_eq!(
        audio
            .and_then(|item| item.get("data"))
            .and_then(Value::as_str),
        Some("SUQz"),
        "dest Chat complete message.audio remapped dest Responses complete must write output_audio data, got {mapped}"
    );
    assert_eq!(
        audio
            .and_then(|item| item.get("transcript"))
            .and_then(Value::as_str),
        Some("hello there"),
        "dest Chat complete message.audio remapped dest Responses complete must write output_audio transcript, got {mapped}"
    );
    assert!(
        audio.is_some_and(|item| item.get("id").is_none()),
        "dest Chat complete message.audio remapped dest Responses complete must not invent output_audio id, got {mapped}"
    );
    assert!(
        audio.is_some_and(|item| item.get("expires_at").is_none()),
        "dest Chat complete message.audio remapped dest Responses complete must not invent output_audio expires_at, got {mapped}"
    );
}

#[test]
fn dest_chat_complete_custom_tool_call_remap_dest_responses_stream() {
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
    let frames = encode_all(Wire::Responses, &events);
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.custom_tool_call_input.delta")
                && frame.data.contains(r#""delta":"print(1)""#)
        }),
        "dest Chat complete custom tool remapped dest Responses STREAM must emit response.custom_tool_call_input.delta, got {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.output_item.added")
                && frame.data.contains("\"type\":\"custom_tool_call\"")
                && frame.data.contains("\"name\":\"code_exec\"")
        }),
        "dest Responses STREAM must add custom_tool_call output item, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_function_call_remaps_dest_messages_and_gemini() {
    let raw = RawSse {
        event: None,
        data: json!({
            "choices": [{
                "delta": {
                    "function_call": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM function_call");
    let messages = encode_all(Wire::Messages, &events);
    assert!(
        messages
            .iter()
            .any(|frame| frame.data.contains("get_weather")),
        "dest Chat STREAM function_call remapped dest Messages STREAM must emit tool_use name, got {messages:?}"
    );
    assert!(
        messages.iter().any(|frame| frame.data.contains("Paris")),
        "dest Chat STREAM function_call remapped dest Messages STREAM must keep arguments, got {messages:?}"
    );
    let gemini = encode_all(Wire::Gemini, &events);
    let gemini_bodies = sse_json_frames(&gemini);
    assert_eq!(
        gemini_bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/content/parts/0/functionCall/name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        }),
        Some("get_weather"),
        "dest Chat STREAM function_call remapped dest Gemini STREAM must emit functionCall name, got {gemini:?}"
    );
}

#[test]
fn dest_chat_complete_function_call_remaps_dest_messages_and_gemini() {
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "function_call": {
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                }
            },
            "finish_reason": "function_call"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete function_call");
    let messages = encode_all(Wire::Messages, &events);
    let messages_bodies = sse_json_frames(&messages);
    assert_eq!(
        messages_bodies.iter().find_map(|body| {
            body.pointer("/content_block/name")
                .or_else(|| body.pointer("/delta/name"))
                .and_then(Value::as_str)
        }),
        Some("get_weather"),
        "dest Chat complete function_call remapped dest Messages STREAM must emit tool_use name, got {messages:?}"
    );
    assert!(
        messages.iter().any(|frame| frame.data.contains("Paris")),
        "dest Chat complete function_call remapped dest Messages STREAM must keep arguments, got {messages:?}"
    );
    let gemini = encode_all(Wire::Gemini, &events);
    let gemini_bodies = sse_json_frames(&gemini);
    assert_eq!(
        gemini_bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/content/parts/0/functionCall/name")
                .and_then(Value::as_str)
        }),
        Some("get_weather"),
        "dest Chat complete function_call remapped dest Gemini STREAM must emit functionCall name, got {gemini:?}"
    );
}

#[test]
fn dest_responses_stream_decode_annotation_added_is_same_ir_as_chat_complete() {
    let chat_body = serde_json::to_vec(&json!({
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
            }
        }]
    }))
    .expect("json");
    let chat_events = decode_response(Wire::ChatCompletions, &chat_body, &chat_profile())
        .expect("decode dest Chat complete annotations");
    let raw = RawSse {
        event: Some("response.output_text.annotation.added".into()),
        data: json!({
            "type": "response.output_text.annotation.added",
            "output_index": 0,
            "content_index": 0,
            "annotation_index": 0,
            "annotation": {
                "type": "url_citation",
                "start_index": 4,
                "end_index": 23,
                "title": "Example Domain",
                "url": "https://example.com"
            }
        })
        .to_string(),
    };
    let responses_events = decode_stream_events(Wire::Responses, &raw, &responses_profile())
        .expect("decode dest Responses annotation.added");
    let is_citation = |ev: &&IrStreamEvent| match ev {
        IrStreamEvent::TextDelta { .. }
        | IrStreamEvent::FinishReason { .. }
        | IrStreamEvent::Protocol { .. }
        | IrStreamEvent::Unknown { .. } => false,
        other => format!("{other:?}").contains("example.com"),
    };
    let chat_ann = chat_events.iter().find(is_citation);
    let responses_ann = responses_events.iter().find(is_citation);
    assert!(
        chat_ann.is_some(),
        "dest Chat complete url_citation must lift to IR, got {chat_events:?}"
    );
    assert!(
        responses_ann.is_some(),
        "dest Responses annotation.added must lift to IR, not Protocol, got {responses_events:?}"
    );
    assert_eq!(
        chat_ann, responses_ann,
        "dest Chat complete annotations and dest Responses annotation.added must share IR"
    );
}

#[test]
fn dest_responses_stream_decode_audio_is_same_ir_as_chat_complete() {
    let chat_body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "audio": { "data": "SUQz", "transcript": "hello there" }
            }
        }]
    }))
    .expect("json");
    let chat_events = decode_response(Wire::ChatCompletions, &chat_body, &chat_profile())
        .expect("decode dest Chat complete audio");
    let audio = RawSse {
        event: Some("response.audio.delta".into()),
        data: json!({
            "type": "response.audio.delta",
            "delta": "SUQz"
        })
        .to_string(),
    };
    let transcript = RawSse {
        event: Some("response.audio.transcript.delta".into()),
        data: json!({
            "type": "response.audio.transcript.delta",
            "delta": "hello there"
        })
        .to_string(),
    };
    let mut responses_events = decode_stream_events(Wire::Responses, &audio, &responses_profile())
        .expect("decode dest Responses audio.delta");
    responses_events.extend(
        decode_stream_events(Wire::Responses, &transcript, &responses_profile())
            .expect("decode dest Responses audio.transcript.delta"),
    );
    let mapped = encode_response(Wire::ChatCompletions, &responses_events)
        .expect("encode dest Chat complete from dest Responses audio IR");
    assert_eq!(
        mapped
            .pointer("/choices/0/message/audio/data")
            .and_then(Value::as_str),
        Some("SUQz"),
        "dest Responses audio.delta must reach dest Chat message.audio.data, got {mapped}"
    );
    assert_eq!(
        mapped
            .pointer("/choices/0/message/audio/transcript")
            .and_then(Value::as_str),
        Some("hello there"),
        "dest Responses audio.transcript.delta must reach dest Chat message.audio.transcript, got {mapped}"
    );
    assert!(
        chat_events
            .iter()
            .any(|ev| format!("{ev:?}").contains("SUQz")),
        "dest Chat complete message.audio.data must lift to IR, got {chat_events:?}"
    );
    assert!(
        chat_events
            .iter()
            .any(|ev| format!("{ev:?}").contains("hello there")),
        "dest Chat complete message.audio.transcript must lift to IR, got {chat_events:?}"
    );
}

#[test]
fn dest_responses_stream_decode_custom_tool_call_is_same_ir_as_chat_complete() {
    let chat_body = serde_json::to_vec(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_custom",
                    "type": "custom",
                    "custom": { "name": "code_exec", "input": "print(1)" }
                }]
            }
        }]
    }))
    .expect("json");
    let chat_events = decode_response(Wire::ChatCompletions, &chat_body, &chat_profile())
        .expect("decode dest Chat complete custom tool");
    let added = RawSse {
        event: Some("response.output_item.added".into()),
        data: json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "custom_tool_call",
                "id": "call_custom",
                "call_id": "call_custom",
                "name": "code_exec",
                "input": ""
            }
        })
        .to_string(),
    };
    let delta = RawSse {
        event: Some("response.custom_tool_call_input.delta".into()),
        data: json!({
            "type": "response.custom_tool_call_input.delta",
            "output_index": 0,
            "item_id": "call_custom",
            "delta": "print(1)"
        })
        .to_string(),
    };
    let mut responses_events = decode_stream_events(Wire::Responses, &added, &responses_profile())
        .expect("decode dest Responses custom_tool_call added");
    responses_events.extend(
        decode_stream_events(Wire::Responses, &delta, &responses_profile())
            .expect("decode dest Responses custom_tool_call_input.delta"),
    );
    assert!(
        !responses_events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::Protocol { .. })),
        "dest Responses custom_tool_call stream events must lift to IR, got {responses_events:?}"
    );
    let mapped = encode_response(Wire::ChatCompletions, &responses_events)
        .expect("encode dest Chat complete from dest Responses custom tool IR");
    let call = mapped.pointer("/choices/0/message/tool_calls/0");
    assert_eq!(
        call.and_then(|v| v.get("type")).and_then(Value::as_str),
        Some("custom"),
        "dest Responses custom_tool_call must dest-encode dest Chat type=custom, got {mapped}"
    );
    assert_eq!(
        call.and_then(|v| v.pointer("/custom/name"))
            .and_then(Value::as_str),
        Some("code_exec"),
        "dest Responses custom_tool_call name must dest-encode dest Chat custom.name, got {mapped}"
    );
    assert_eq!(
        call.and_then(|v| v.pointer("/custom/input"))
            .and_then(Value::as_str),
        Some("print(1)"),
        "dest Responses custom_tool_call_input.delta must dest-encode dest Chat custom.input, got {mapped}"
    );
    let chat_mapped = encode_response(Wire::ChatCompletions, &chat_events)
        .expect("encode dest Chat complete custom tool round-trip");
    assert_eq!(
        chat_mapped
            .pointer("/choices/0/message/tool_calls/0/type")
            .and_then(Value::as_str),
        Some("custom"),
        "dest Chat complete custom tool round-trip must keep type=custom, got {chat_mapped}"
    );
}

#[test]
fn dest_responses_stream_encode_annotation_added_is_annotation_event() {
    let raw = encode_stream_event(
        Wire::Responses,
        &IrStreamEvent::AnnotationAdded {
            annotation: json!({
                "type": "url_citation",
                "start_index": 4,
                "end_index": 23,
                "title": "Example Domain",
                "url": "https://example.com"
            }),
        },
    )
    .expect("encode dest Responses annotation");
    assert_eq!(
        raw.event.as_deref(),
        Some("response.output_text.annotation.added"),
        "dest Responses stream encode must use response.output_text.annotation.added, got {raw:?}"
    );
    let body: Value = serde_json::from_str(&raw.data).expect("json");
    assert_eq!(
        body.pointer("/annotation/url").and_then(Value::as_str),
        Some("https://example.com")
    );
}

#[test]
fn dest_responses_stream_encode_audio_delta_is_audio_event() {
    let audio = encode_stream_event(
        Wire::Responses,
        &IrStreamEvent::AudioDelta {
            data: "SUQz".into(),
        },
    )
    .expect("encode dest Responses audio");
    assert_eq!(
        audio.event.as_deref(),
        Some("response.audio.delta"),
        "dest Responses stream encode must use response.audio.delta, got {audio:?}"
    );
    let transcript = encode_stream_event(
        Wire::Responses,
        &IrStreamEvent::AudioTranscriptDelta {
            text: "hello there".into(),
        },
    )
    .expect("encode dest Responses audio transcript");
    assert_eq!(
        transcript.event.as_deref(),
        Some("response.audio.transcript.delta"),
        "dest Responses stream encode must use response.audio.transcript.delta, got {transcript:?}"
    );
}

#[test]
fn dest_responses_stream_encode_custom_tool_input_delta() {
    let raw = encode_stream_event(
        Wire::Responses,
        &IrStreamEvent::CustomToolCallInputDelta {
            delta: "print(1)".into(),
            index: 0,
        },
    )
    .expect("encode dest Responses custom tool input");
    assert_eq!(
        raw.event.as_deref(),
        Some("response.custom_tool_call_input.delta"),
        "dest Responses stream encode must use response.custom_tool_call_input.delta, got {raw:?}"
    );
    let body: Value = serde_json::from_str(&raw.data).expect("json");
    assert_eq!(body.get("delta").and_then(Value::as_str), Some("print(1)"));
}

#[test]
fn dest_chat_stream_refusal_remaps_dest_responses_refusal_delta() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"refusal":"nope"}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat refusal")
        .expect("event");
    let mapped = encode_stream_event(Wire::Responses, &ev).expect("encode dest Responses");
    assert_eq!(
        mapped.event.as_deref(),
        Some("response.refusal.delta"),
        "dest Chat stream refusal remapped dest Responses must emit response.refusal.delta, got {mapped:?}"
    );
    assert!(
        mapped.data.contains(r#""delta":"nope""#),
        "dest Responses refusal.delta must carry dest Chat refusal text, got {}",
        mapped.data
    );
}

fn dest_chat_stream_logprobs_body() -> Value {
    json!({
        "choices": [{
            "index": 0,
            "delta": { "content": "Hi" },
            "logprobs": {
                "content": [{
                    "token": "Hi",
                    "logprob": -0.1,
                    "bytes": [72, 105],
                    "top_logprobs": [{
                        "token": "Hi",
                        "logprob": -0.1,
                        "bytes": [72, 105]
                    }]
                }]
            }
        }]
    })
}

fn dest_chat_complete_logprobs_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hi"
            },
            "logprobs": {
                "content": [{
                    "token": "Hi",
                    "logprob": -0.1,
                    "bytes": [72, 105],
                    "top_logprobs": [{
                        "token": "Hi",
                        "logprob": -0.1,
                        "bytes": [72, 105]
                    }]
                }]
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("json")
}

fn dest_gemini_stream_logprobs_body() -> Value {
    json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "Hi" }]
            },
            "logprobsResult": {
                "chosenCandidates": [{ "token": "Hi", "logProbability": -0.1 }],
                "topCandidates": [{
                    "candidates": [{ "token": "Hi", "logProbability": -0.1 }]
                }]
            }
        }]
    })
}

fn sse_json_frames(frames: &[RawSse]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|frame| serde_json::from_str(&frame.data).ok())
        .collect()
}

#[test]
fn dest_chat_stream_logprobs_remaps_dest_gemini_logprobs_result() {
    let raw = RawSse {
        event: None,
        data: dest_chat_stream_logprobs_body().to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM logprobs");
    let frames = encode_all(Wire::Gemini, &events);
    let bodies = sse_json_frames(&frames);
    let token = bodies.iter().find_map(|body| {
        body.pointer("/candidates/0/logprobsResult/chosenCandidates/0/token")
            .and_then(Value::as_str)
    });
    let logp = bodies.iter().find_map(|body| {
        body.pointer("/candidates/0/logprobsResult/chosenCandidates/0/logProbability")
            .and_then(Value::as_f64)
    });
    assert_eq!(
        token,
        Some("Hi"),
        "dest Chat STREAM logprobs remapped dest Gemini must write logprobsResult.chosenCandidates token, got {frames:?}"
    );
    assert_eq!(
        logp,
        Some(-0.1),
        "dest Chat STREAM logprobs remapped dest Gemini must write logprobsResult.chosenCandidates logProbability, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_created_remaps_dest_responses_created_at() {
    let raw = RawSse {
        event: None,
        data: json!({
            "id": "chatcmpl-r58",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM created");
    let frames = encode_all(Wire::Responses, &events);
    let created: Vec<Value> = frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("response.created"))
        .filter_map(|frame| serde_json::from_str(&frame.data).ok())
        .collect();
    assert_eq!(
        created
            .iter()
            .find_map(|body| body.pointer("/response/created_at").and_then(Value::as_i64)),
        Some(1_700_000_000),
        "dest Chat STREAM created remapped dest Responses STREAM must write response.created created_at, got {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.output_text.delta")
                && frame.data.contains("Hi")
        }),
        "dest Chat STREAM content remapped dest Responses STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_responses_stream_created_at_remaps_dest_chat_stream_created() {
    let raw = RawSse {
        event: Some("response.created".into()),
        data: json!({
            "type": "response.created",
            "response": {
                "id": "resp_1",
                "status": "in_progress",
                "created_at": 1700000000
            }
        })
        .to_string(),
    };
    let mut events = decode_stream_events(Wire::Responses, &raw, &responses_profile())
        .expect("decode dest Responses STREAM created_at");
    let delta = RawSse {
        event: Some("response.output_text.delta".into()),
        data: json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "delta": "Hi"
        })
        .to_string(),
    };
    events.extend(
        decode_stream_events(Wire::Responses, &delta, &responses_profile())
            .expect("decode dest Responses STREAM text delta"),
    );
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies
            .iter()
            .find_map(|body| body.get("created").and_then(Value::as_i64)),
        Some(1_700_000_000),
        "dest Responses STREAM response.created created_at remapped dest Chat STREAM must write created, got {frames:?}"
    );
    assert!(
        bodies.iter().any(|body| {
            body.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                == Some("Hi")
        }),
        "dest Responses STREAM text remapped dest Chat STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_metadata_remaps_dest_responses_stream_metadata() {
    let raw = RawSse {
        event: None,
        data: json!({
            "id": "chatcmpl-r58m",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "metadata": { "user": "alice" },
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM metadata");
    let frames = encode_all(Wire::Responses, &events);
    let created: Vec<Value> = frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("response.created"))
        .filter_map(|frame| serde_json::from_str(&frame.data).ok())
        .collect();
    assert_eq!(
        created.iter().find_map(|body| body
            .pointer("/response/metadata/user")
            .and_then(Value::as_str)),
        Some("alice"),
        "dest Chat STREAM metadata remapped dest Responses STREAM must write response.created.response.metadata, got {frames:?}"
    );
    assert_eq!(
        created
            .iter()
            .find_map(|body| body.pointer("/response/created_at").and_then(Value::as_i64)),
        Some(1_700_000_000),
        "dest Chat STREAM created plus metadata remapped dest Responses STREAM must still write created_at, got {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("response.output_text.delta")
                && frame.data.contains("Hi")
        }),
        "dest Chat STREAM content remapped dest Responses STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_responses_stream_metadata_remaps_dest_chat_stream_metadata() {
    let raw = RawSse {
        event: Some("response.created".into()),
        data: json!({
            "type": "response.created",
            "response": {
                "id": "resp_1",
                "status": "in_progress",
                "metadata": { "user": "alice" }
            }
        })
        .to_string(),
    };
    let mut events = decode_stream_events(Wire::Responses, &raw, &responses_profile())
        .expect("decode dest Responses STREAM metadata");
    let delta = RawSse {
        event: Some("response.output_text.delta".into()),
        data: json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "delta": "Hi"
        })
        .to_string(),
    };
    events.extend(
        decode_stream_events(Wire::Responses, &delta, &responses_profile())
            .expect("decode dest Responses STREAM text delta"),
    );
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies
            .iter()
            .find_map(|body| body.pointer("/metadata/user").and_then(Value::as_str)),
        Some("alice"),
        "dest Responses STREAM response.created.response.metadata remapped dest Chat STREAM must write metadata, got {frames:?}"
    );
    assert!(
        bodies.iter().any(|body| {
            body.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                == Some("Hi")
        }),
        "dest Responses STREAM text remapped dest Chat STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_logprobs_remaps_dest_responses_output_text_logprobs() {
    let raw = RawSse {
        event: None,
        data: dest_chat_stream_logprobs_body().to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM logprobs");
    let frames = encode_all(Wire::Responses, &events);
    let deltas: Vec<Value> = frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("response.output_text.delta"))
        .filter_map(|frame| serde_json::from_str(&frame.data).ok())
        .collect();
    let token = deltas
        .iter()
        .find_map(|body| body.pointer("/logprobs/0/token").and_then(Value::as_str));
    let logp = deltas
        .iter()
        .find_map(|body| body.pointer("/logprobs/0/logprob").and_then(Value::as_f64));
    assert_eq!(
        token,
        Some("Hi"),
        "dest Chat STREAM logprobs remapped dest Responses must write output_text.delta logprobs token, got {frames:?}"
    );
    assert_eq!(
        logp,
        Some(-0.1),
        "dest Chat STREAM logprobs remapped dest Responses must write output_text.delta logprobs logprob, got {frames:?}"
    );
    assert!(
        deltas
            .iter()
            .any(|body| body.get("delta").and_then(Value::as_str) == Some("Hi")),
        "dest Chat STREAM content remapped dest Responses must still carry text Hi, got {frames:?}"
    );
    let dones: Vec<Value> = frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("response.output_text.done"))
        .filter_map(|frame| serde_json::from_str(&frame.data).ok())
        .collect();
    assert_eq!(
        dones
            .iter()
            .find_map(|body| body.pointer("/logprobs/0/token").and_then(Value::as_str)),
        Some("Hi"),
        "dest Chat STREAM logprobs remapped dest Responses must write output_text.done logprobs token, got {frames:?}"
    );
    assert_eq!(
        dones
            .iter()
            .find_map(|body| body.get("text").and_then(Value::as_str)),
        Some("Hi"),
        "dest Chat STREAM content remapped dest Responses must write output_text.done text Hi, got {frames:?}"
    );
    let items: Vec<Value> = frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("response.output_item.done"))
        .filter_map(|frame| serde_json::from_str(&frame.data).ok())
        .collect();
    assert_eq!(
        items.iter().find_map(|body| {
            body.pointer("/item/content/0/logprobs/0/token")
                .and_then(Value::as_str)
        }),
        Some("Hi"),
        "dest Chat STREAM logprobs remapped dest Responses must write output_item.done output_text logprobs token, got {frames:?}"
    );
}

#[test]
fn dest_chat_complete_logprobs_remaps_dest_gemini_and_responses() {
    let events = decode_response(
        Wire::ChatCompletions,
        &dest_chat_complete_logprobs_body(),
        &chat_profile(),
    )
    .expect("decode dest Chat complete logprobs");
    let gemini_stream = encode_all(Wire::Gemini, &events);
    let gemini_stream_bodies = sse_json_frames(&gemini_stream);
    assert_eq!(
        gemini_stream_bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/logprobsResult/chosenCandidates/0/token")
                .and_then(Value::as_str)
        }),
        Some("Hi"),
        "dest Chat complete logprobs remapped dest Gemini STREAM must write logprobsResult token, got {gemini_stream:?}"
    );
    assert_eq!(
        gemini_stream_bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/logprobsResult/chosenCandidates/0/logProbability")
                .and_then(Value::as_f64)
        }),
        Some(-0.1),
        "dest Chat complete logprobs remapped dest Gemini STREAM must write logprobsResult logProbability, got {gemini_stream:?}"
    );
    let gemini_complete =
        encode_response(Wire::Gemini, &events).expect("encode dest Gemini complete");
    assert_eq!(
        gemini_complete
            .pointer("/candidates/0/logprobsResult/chosenCandidates/0/token")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete logprobs remapped dest Gemini complete must write logprobsResult token, got {gemini_complete}"
    );
    assert_eq!(
        gemini_complete
            .pointer("/candidates/0/logprobsResult/chosenCandidates/0/logProbability")
            .and_then(Value::as_f64),
        Some(-0.1),
        "dest Chat complete logprobs remapped dest Gemini complete must write logprobsResult logProbability, got {gemini_complete}"
    );
    let responses_stream = encode_all(Wire::Responses, &events);
    let responses_deltas: Vec<Value> = responses_stream
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("response.output_text.delta"))
        .filter_map(|frame| serde_json::from_str(&frame.data).ok())
        .collect();
    assert_eq!(
        responses_deltas
            .iter()
            .find_map(|body| body.pointer("/logprobs/0/token").and_then(Value::as_str)),
        Some("Hi"),
        "dest Chat complete logprobs remapped dest Responses STREAM must write logprobs token, got {responses_stream:?}"
    );
    assert_eq!(
        responses_deltas
            .iter()
            .find_map(|body| body.pointer("/logprobs/0/logprob").and_then(Value::as_f64)),
        Some(-0.1),
        "dest Chat complete logprobs remapped dest Responses STREAM must write logprobs logprob, got {responses_stream:?}"
    );
    let responses_complete =
        encode_response(Wire::Responses, &events).expect("encode dest Responses complete");
    assert_eq!(
        responses_complete
            .pointer("/output/0/content/0/logprobs/0/token")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete logprobs remapped dest Responses complete must write output_text logprobs token, got {responses_complete}"
    );
    assert_eq!(
        responses_complete
            .pointer("/output/0/content/0/logprobs/0/logprob")
            .and_then(Value::as_f64),
        Some(-0.1),
        "dest Chat complete logprobs remapped dest Responses complete must write output_text logprobs logprob, got {responses_complete}"
    );
    assert_eq!(
        responses_complete
            .pointer("/output/0/content/0/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Responses complete must still carry text Hi, got {responses_complete}"
    );
}

#[test]
fn dest_chat_complete_created_remaps_dest_responses_created_at() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r57",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete created");
    let responses =
        encode_response(Wire::Responses, &events).expect("encode dest Responses complete");
    assert_eq!(
        responses.get("created_at").and_then(Value::as_i64),
        Some(1_700_000_000),
        "dest Chat complete created remapped dest Responses complete must write created_at, got {responses}"
    );
    assert_eq!(
        responses
            .pointer("/output/0/content/0/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Responses complete must still carry text Hi, got {responses}"
    );
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.get("created").and_then(Value::as_i64),
        Some(1_700_000_000),
        "dest Chat complete created remapped dest Chat complete must keep created, got {chat}"
    );
}

#[test]
fn dest_chat_complete_remaps_dest_gemini_complete_response_id() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r99",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete");
    let gemini = encode_response(Wire::Gemini, &events).expect("encode dest Gemini complete");
    assert_eq!(
        gemini.get("responseId").and_then(Value::as_str),
        Some("gemini-wiremux"),
        "dest Chat complete remapped dest Gemini complete must write dest Gemini dest identity responseId, got {gemini}"
    );
    assert_ne!(
        gemini.get("responseId").and_then(Value::as_str),
        Some("chatcmpl-r99"),
        "dest Gemini complete responseId must be dest Gemini dest identity, not dest Chat id copy, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Gemini complete must still carry text Hi, got {gemini}"
    );
}

#[test]
fn dest_chat_complete_usage_remaps_dest_gemini_total_token_count() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r101",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120,
            "prompt_tokens_details": { "cached_tokens": 40 },
            "completion_tokens_details": { "reasoning_tokens": 5 }
        }
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete usage");
    let gemini = encode_response(Wire::Gemini, &events).expect("encode dest Gemini complete");
    assert_eq!(
        gemini
            .pointer("/usageMetadata/promptTokenCount")
            .and_then(Value::as_u64),
        Some(100),
        "dest Chat complete usage remapped dest Gemini complete must write promptTokenCount, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/usageMetadata/candidatesTokenCount")
            .and_then(Value::as_u64),
        Some(15),
        "dest Chat complete usage remapped dest Gemini complete must write candidatesTokenCount exclusive of thoughts, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/usageMetadata/thoughtsTokenCount")
            .and_then(Value::as_u64),
        Some(5),
        "dest Chat complete usage remapped dest Gemini complete must write thoughtsTokenCount, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/usageMetadata/totalTokenCount")
            .and_then(Value::as_u64),
        Some(120),
        "dest Chat complete usage remapped dest Gemini complete must write totalTokenCount as prompt plus candidates plus thoughts, got {gemini}"
    );
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        gemini
            .pointer("/usageMetadata/totalTokenCount")
            .and_then(Value::as_u64),
        chat.pointer("/usage/total_tokens").and_then(Value::as_u64),
        "dest Gemini totalTokenCount must equal dest Chat inclusive total_tokens, gemini {gemini} chat {chat}"
    );
    assert_eq!(
        gemini
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Gemini complete must still carry text Hi, got {gemini}"
    );
}

#[test]
fn dest_chat_complete_usage_total_tokens_remaps_dest_converse_total_tokens() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r102",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 30,
            "completion_tokens": 8,
            "total_tokens": 38
        }
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete usage.total_tokens");
    let converse = encode_response(Wire::Converse, &events).expect("encode dest Converse complete");
    assert_eq!(
        converse
            .pointer("/usage/inputTokens")
            .and_then(Value::as_u64),
        Some(30),
        "dest Chat complete usage remapped dest Converse complete must write inputTokens, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/usage/outputTokens")
            .and_then(Value::as_u64),
        Some(8),
        "dest Chat complete usage remapped dest Converse complete must write outputTokens, got {converse}"
    );
    let input = converse
        .pointer("/usage/inputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = converse
        .pointer("/usage/outputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert_eq!(
        converse
            .pointer("/usage/totalTokens")
            .and_then(Value::as_u64),
        Some(input.saturating_add(output)),
        "dest Chat complete usage.total_tokens remapped dest Converse complete must write required totalTokens as inputTokens plus outputTokens, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/usage/totalTokens")
            .and_then(Value::as_u64),
        Some(38),
        "dest Chat complete usage.total_tokens remapped dest Converse complete must write totalTokens 38, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/output/message/content/0/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Converse complete must still carry text Hi, got {converse}"
    );
}

#[test]
fn dest_chat_complete_service_tier_remaps_dest_responses_service_tier() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r63",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "service_tier": "priority",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete service_tier");
    let responses =
        encode_response(Wire::Responses, &events).expect("encode dest Responses complete");
    assert_eq!(
        responses.get("service_tier").and_then(Value::as_str),
        Some("priority"),
        "dest Chat complete service_tier remapped dest Responses complete must write service_tier, got {responses}"
    );
    assert_eq!(
        responses
            .pointer("/output/0/content/0/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Responses complete must still carry text Hi, got {responses}"
    );
}

#[test]
fn dest_chat_stream_service_tier_remaps_dest_responses_stream_service_tier() {
    let raw = RawSse {
        event: None,
        data: json!({
            "id": "chatcmpl-r63s",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "service_tier": "priority",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM service_tier");
    let frames = encode_all(Wire::Responses, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/response/service_tier")
                .and_then(Value::as_str)
        }),
        Some("priority"),
        "dest Chat STREAM service_tier remapped dest Responses STREAM must write response.created.service_tier, got {frames:?}"
    );
}

#[test]
fn dest_responses_stream_service_tier_remaps_dest_chat_stream_service_tier() {
    let raw = RawSse {
        event: Some("response.created".into()),
        data: json!({
            "type": "response.created",
            "response": {
                "id": "resp_1",
                "status": "in_progress",
                "service_tier": "priority"
            }
        })
        .to_string(),
    };
    let mut events = decode_stream_events(Wire::Responses, &raw, &responses_profile())
        .expect("decode dest Responses STREAM service_tier");
    let delta = RawSse {
        event: Some("response.output_text.delta".into()),
        data: json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "delta": "Hi"
        })
        .to_string(),
    };
    events.extend(
        decode_stream_events(Wire::Responses, &delta, &responses_profile())
            .expect("decode dest Responses STREAM text delta"),
    );
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies
            .iter()
            .find_map(|body| body.get("service_tier").and_then(Value::as_str)),
        Some("priority"),
        "dest Responses STREAM response.created service_tier remapped dest Chat STREAM must write service_tier, got {frames:?}"
    );
    assert!(
        bodies.iter().any(|body| {
            body.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                == Some("Hi")
        }),
        "dest Responses STREAM text remapped dest Chat STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_responses_complete_failed_remaps_dest_chat_finish_reason_stop() {
    let body = serde_json::to_vec(&json!({
        "id": "resp_1",
        "object": "response",
        "created_at": 1700000000,
        "status": "failed",
        "error": { "code": "server_error", "message": "upstream failed" },
        "model": "gpt-4o",
        "output": []
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile())
        .expect("decode dest Responses complete failed");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/finish_reason")
            .and_then(Value::as_str),
        Some("stop"),
        "dest Responses complete status failed remapped dest Chat complete must write dest Chat finish_reason stop, got {chat}"
    );
}

#[test]
fn dest_chat_complete_service_tier_remaps_dest_converse_service_tier() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r66",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "service_tier": "priority",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete service_tier");
    let converse = encode_response(Wire::Converse, &events).expect("encode dest Converse complete");
    assert_eq!(
        converse
            .pointer("/serviceTier/type")
            .and_then(Value::as_str),
        Some("priority"),
        "dest Chat complete service_tier remapped dest Converse complete must write serviceTier.type, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/output/message/content/0/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Converse complete must still carry text Hi, got {converse}"
    );
}

#[test]
fn dest_gemini_complete_usage_service_tier_remaps_dest_chat_service_tier() {
    let body = serde_json::to_vec(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "Hi" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 2,
            "serviceTier": "priority"
        }
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &body, &gemini_profile())
        .expect("decode dest Gemini complete usageMetadata.serviceTier");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.get("service_tier").and_then(Value::as_str),
        Some("priority"),
        "dest Gemini complete usageMetadata.serviceTier remapped dest Chat complete must write service_tier, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Gemini complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_chat_complete_service_tier_remaps_dest_gemini_usage_service_tier() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r68",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "service_tier": "priority",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete service_tier");
    let gemini = encode_response(Wire::Gemini, &events).expect("encode dest Gemini complete");
    assert_eq!(
        gemini
            .pointer("/usageMetadata/serviceTier")
            .and_then(Value::as_str),
        Some("priority"),
        "dest Chat complete service_tier remapped dest Gemini complete must write usageMetadata.serviceTier, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Gemini complete must still carry text Hi, got {gemini}"
    );
}

#[test]
fn dest_chat_stream_service_tier_remaps_dest_gemini_usage_service_tier() {
    let raw = RawSse {
        event: None,
        data: json!({
            "id": "chatcmpl-r68s",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "service_tier": "priority",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM service_tier");
    let frames = encode_all(Wire::Gemini, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/usageMetadata/serviceTier")
                .and_then(Value::as_str)
        }),
        Some("priority"),
        "dest Chat STREAM service_tier remapped dest Gemini STREAM must write usageMetadata.serviceTier, got {frames:?}"
    );
}

#[test]
fn dest_gemini_complete_inline_data_audio_remaps_dest_chat_message_audio() {
    let body = serde_json::to_vec(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{
                    "inlineData": { "mimeType": "audio/mpeg", "data": "SUQz" }
                }]
            },
            "finishReason": "STOP"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &body, &gemini_profile())
        .expect("decode dest Gemini complete inlineData audio");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/audio/data")
            .and_then(Value::as_str),
        Some("SUQz"),
        "dest Gemini complete inlineData audio remapped dest Chat complete must write message.audio.data, got {chat}"
    );
    assert!(
        chat.pointer("/choices/0/message/audio/id").is_none(),
        "dest Gemini complete inlineData audio remapped dest Chat complete must not invent audio.id, got {chat}"
    );
    assert!(
        chat.pointer("/choices/0/message/audio/expires_at")
            .is_none(),
        "dest Gemini complete inlineData audio remapped dest Chat complete must not invent audio.expires_at, got {chat}"
    );
}

fn gemini_inline_chunk(mime: &str, data: &str, finish: bool) -> RawSse {
    let mut candidate = json!({
        "content": {
            "role": "model",
            "parts": [{
                "inlineData": { "mimeType": mime, "data": data }
            }]
        }
    });
    if finish {
        candidate["finishReason"] = json!("STOP");
    }
    RawSse {
        event: None,
        data: json!({ "candidates": [candidate] }).to_string(),
    }
}

#[test]
fn gemini_stream_image_inline_data_becomes_image_delta() {
    let raw = gemini_inline_chunk("image/png", "iVBORw0KGgo=", true);
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("image chunk");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/png" && data == "iVBORw0KGgo="
        )),
        "image inlineData must become ImageDelta, got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "image chunk with finishReason must still finish, got {events:?}"
    );

    let singular = decode_stream_event(Wire::Gemini, &raw, &gemini_profile())
        .expect("singular")
        .expect("event");
    assert!(
        matches!(
            singular,
            IrStreamEvent::ImageDelta { ref media_type, ref data }
                if media_type == "image/png" && data == "iVBORw0KGgo="
        ),
        "singular decode must return the image, got {singular:?}"
    );
}

#[test]
fn gemini_stream_audio_inline_data_stays_audio_delta() {
    let raw = gemini_inline_chunk("audio/mpeg", "SUQz", false);
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("audio");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::AudioDelta { data } if data == "SUQz")),
        "audio inlineData must stay AudioDelta, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ImageDelta { .. })),
        "audio inlineData must not become ImageDelta, got {events:?}"
    );
}

#[test]
fn gemini_stream_empty_or_non_image_inline_data_is_not_an_image() {
    let empty = gemini_inline_chunk("image/png", "", true);
    let empty_events =
        decode_stream_events(Wire::Gemini, &empty, &gemini_profile()).expect("empty data");
    assert!(
        !empty_events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ImageDelta { .. })),
        "empty image data must not become ImageDelta, got {empty_events:?}"
    );
    assert!(
        empty_events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { .. })),
        "empty image chunk must still finish, got {empty_events:?}"
    );

    let pdf = gemini_inline_chunk("application/pdf", "JVBERi0=", false);
    let pdf_events = decode_stream_events(Wire::Gemini, &pdf, &gemini_profile()).expect("pdf");
    assert!(
        !pdf_events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { .. } | IrStreamEvent::AudioDelta { .. }
        )),
        "pdf inlineData must not become an image or audio event, got {pdf_events:?}"
    );
}

#[test]
fn gemini_stream_text_and_image_parts_both_emit() {
    let raw = RawSse {
        event: None,
        data: json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        { "text": "here" },
                        { "inlineData": { "mimeType": "image/jpeg", "data": "/9j/" } }
                    ]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("both");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "here")),
        "text part must remain, got {events:?}"
    );
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/jpeg" && data == "/9j/"
        )),
        "image part must become ImageDelta, got {events:?}"
    );
}

#[test]
fn dest_gemini_image_inline_data_remaps_dest_chat_image_url() {
    let raw = gemini_inline_chunk("image/png", "iVBORw0KGgo=", false);
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("decode");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    let url = bodies.iter().find_map(|body| {
        body.pointer("/choices/0/delta/content/0/image_url/url")
            .and_then(Value::as_str)
    });
    assert_eq!(
        url,
        Some("data:image/png;base64,iVBORw0KGgo="),
        "dest Chat stream must write an image_url data URL, got {frames:?}"
    );

    let chat = encode_response(Wire::ChatCompletions, &events).expect("complete");
    assert_eq!(
        chat.pointer("/choices/0/message/content/0/image_url/url")
            .and_then(Value::as_str),
        Some("data:image/png;base64,iVBORw0KGgo="),
        "dest Chat complete must write an image_url data URL, got {chat}"
    );

    let round = decode_response(
        Wire::ChatCompletions,
        &serde_json::to_vec(&chat).expect("bytes"),
        &chat_profile(),
    )
    .expect("decode chat complete");
    assert!(
        round.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/png" && data == "iVBORw0KGgo="
        )),
        "dest Chat complete image_url must decode back to ImageDelta, got {round:?}"
    );
}

#[test]
fn dest_gemini_image_inline_data_keeps_mime_on_dest_gemini() {
    let raw = gemini_inline_chunk("image/webp", "UklGR", false);
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("decode");
    let frame = encode_stream_event(Wire::Gemini, &events[0]).expect("encode");
    let body: Value = serde_json::from_str(&frame.data).expect("json");
    assert_eq!(
        body.pointer("/candidates/0/content/parts/0/inlineData/mimeType")
            .and_then(Value::as_str),
        Some("image/webp"),
        "dest Gemini must keep the image mime, got {body}"
    );
    assert_eq!(
        body.pointer("/candidates/0/content/parts/0/inlineData/data")
            .and_then(Value::as_str),
        Some("UklGR"),
        "dest Gemini must keep the image bytes, got {body}"
    );

    let complete = encode_response(Wire::Gemini, &events).expect("complete");
    assert_eq!(
        complete
            .pointer("/candidates/0/content/parts/0/inlineData/mimeType")
            .and_then(Value::as_str),
        Some("image/webp"),
        "dest Gemini complete must keep the image mime, got {complete}"
    );
}

#[test]
fn dest_gemini_image_inline_data_reaches_messages_responses_and_converse() {
    let events = [IrStreamEvent::ImageDelta {
        media_type: "image/png".into(),
        data: "iVBORw0KGgo=".into(),
    }];
    let messages = encode_response(Wire::Messages, &events).expect("messages");
    assert_eq!(
        messages
            .pointer("/content/0/source/data")
            .and_then(Value::as_str),
        Some("iVBORw0KGgo="),
        "dest Messages complete must keep image bytes, got {messages}"
    );
    assert_eq!(
        messages
            .pointer("/content/0/source/media_type")
            .and_then(Value::as_str),
        Some("image/png"),
        "dest Messages complete must keep the image mime, got {messages}"
    );

    let responses = encode_response(Wire::Responses, &events).expect("responses");
    assert_eq!(
        responses
            .pointer("/output/0/content/0/image_url")
            .and_then(Value::as_str),
        Some("data:image/png;base64,iVBORw0KGgo="),
        "dest Responses complete must write output_image, got {responses}"
    );

    let converse = encode_response(Wire::Converse, &events).expect("converse");
    assert_eq!(
        converse
            .pointer("/output/message/content/0/image/source/bytes")
            .and_then(Value::as_str),
        Some("iVBORw0KGgo="),
        "dest Converse complete must keep image bytes, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/output/message/content/0/image/format")
            .and_then(Value::as_str),
        Some("png"),
        "dest Converse complete must map image/png to png, got {converse}"
    );
}

#[test]
fn dest_complete_image_before_text_keeps_part_order() {
    let events = [
        IrStreamEvent::ImageDelta {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        IrStreamEvent::TextDelta {
            text: "caption".into(),
        },
    ];
    let chat = encode_response(Wire::ChatCompletions, &events).expect("chat");
    assert_eq!(
        chat.pointer("/choices/0/message/content/0/type")
            .and_then(Value::as_str),
        Some("image_url"),
        "image before text must stay first on dest Chat, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content/1/text")
            .and_then(Value::as_str),
        Some("caption"),
        "text after an image must follow it on dest Chat, got {chat}"
    );
    let messages = encode_response(Wire::Messages, &events).expect("messages");
    assert_eq!(
        messages.pointer("/content/0/type").and_then(Value::as_str),
        Some("image"),
        "image before text must stay first on dest Messages, got {messages}"
    );
    assert_eq!(
        messages.pointer("/content/1/text").and_then(Value::as_str),
        Some("caption"),
        "text after an image must follow it on dest Messages, got {messages}"
    );
    let gemini = encode_response(Wire::Gemini, &events).expect("gemini");
    assert!(
        gemini
            .pointer("/candidates/0/content/parts/0/inlineData")
            .is_some(),
        "image before text must stay first on dest Gemini, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/candidates/0/content/parts/1/text")
            .and_then(Value::as_str),
        Some("caption"),
        "text after an image must follow it on dest Gemini, got {gemini}"
    );
    let responses = encode_response(Wire::Responses, &events).expect("responses");
    assert_eq!(
        responses
            .pointer("/output/0/content/0/type")
            .and_then(Value::as_str),
        Some("output_image"),
        "image before text must stay first on dest Responses, got {responses}"
    );
    assert_eq!(
        responses
            .pointer("/output/0/content/1/text")
            .and_then(Value::as_str),
        Some("caption"),
        "text after an image must follow it on dest Responses, got {responses}"
    );
    let round = decode_response(
        Wire::ChatCompletions,
        &serde_json::to_vec(&chat).expect("bytes"),
        &chat_profile(),
    )
    .expect("decode chat");
    let kinds: Vec<&str> = round
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ImageDelta { .. } => Some("image"),
            IrStreamEvent::TextDelta { text } if text == "caption" => Some("text"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        ["image", "text"],
        "dest Chat complete image-then-text must decode in that order, got {round:?}"
    );
}

#[test]
fn dest_converse_rejects_image_mime_outside_the_allow_list() {
    let events = [IrStreamEvent::ImageDelta {
        media_type: "image/bmp".into(),
        data: "Qk0=".into(),
    }];
    let err = encode_response(Wire::Converse, &events).expect_err("converse");
    assert!(
        err.to_string().contains("image/bmp"),
        "complete encode must fail an unsupported Converse image, got {err}"
    );
    let err = encode_stream_event(Wire::Converse, &events[0]).expect_err("bmp");
    assert!(
        err.to_string().contains("image/bmp"),
        "unsupported Converse image must be an error, got {err}"
    );
    let err = StreamEncoder::new(Wire::Converse)
        .push(events[0].clone())
        .expect_err("encoder");
    assert!(
        err.to_string().contains("image/bmp"),
        "stream encoder must fail the unsupported image, got {err}"
    );
}

#[test]
fn responses_content_part_audio_becomes_audio_delta() {
    let raw = RawSse {
        event: Some("response.content_part.added".into()),
        data: json!({
            "type": "response.content_part.added",
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_audio",
                "data": "SUQz",
                "transcript": "hello"
            }
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Responses, &raw, &responses_profile()).expect("part");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::AudioDelta { data } if data == "SUQz")),
        "output_audio data must become AudioDelta, got {events:?}"
    );
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::AudioTranscriptDelta { text } if text == "hello"
        )),
        "output_audio transcript must stay with the bytes, got {events:?}"
    );

    let added = RawSse {
        event: Some("response.output_item.added".into()),
        data: json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_audio",
                    "data": "SUQz"
                }]
            }
        })
        .to_string(),
    };
    let added_events =
        decode_stream_events(Wire::Responses, &added, &responses_profile()).expect("added");
    assert!(
        added_events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::AudioDelta { data } if data == "SUQz")),
        "message output_audio must become AudioDelta, got {added_events:?}"
    );
}

#[test]
fn converse_stream_audio_bytes_round_trip() {
    let raw = RawSse {
        event: None,
        data: json!({
            "contentBlockStart": {
                "start": {
                    "audio": { "format": "mp3", "source": { "bytes": "SUQz" } }
                }
            }
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Converse, &raw, &converse_profile()).expect("decode");
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::AudioDelta { data } if data == "SUQz")),
        "converse stream audio bytes must become AudioDelta, got {events:?}"
    );
    let empty = RawSse {
        event: None,
        data: json!({
            "contentBlockStart": {
                "start": { "audio": { "format": "mp3", "source": { "bytes": "" } } }
            }
        })
        .to_string(),
    };
    let empty_events =
        decode_stream_events(Wire::Converse, &empty, &converse_profile()).expect("empty");
    assert!(
        !empty_events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::AudioDelta { .. })),
        "empty converse audio bytes must stay absent, got {empty_events:?}"
    );

    let encoded = encode_all(
        Wire::Converse,
        &[IrStreamEvent::AudioDelta {
            data: "SUQz".into(),
        }],
    );
    let bodies = sse_json_frames(&encoded);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/contentBlockStart/start/audio/source/bytes")
                .and_then(Value::as_str)
        }),
        Some("SUQz"),
        "dest Converse stream must keep audio bytes, got {bodies:?}"
    );
}

#[test]
fn dest_responses_stream_image_stays_on_one_message() {
    let events = [
        IrStreamEvent::TextDelta {
            text: "before".into(),
        },
        IrStreamEvent::ImageDelta {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        IrStreamEvent::TextDelta {
            text: "after".into(),
        },
    ];
    let bodies = sse_json_frames(&encode_all(Wire::Responses, &events));
    let added = bodies
        .iter()
        .filter(|body| {
            body.get("type").and_then(Value::as_str) == Some("response.output_item.added")
                && body.pointer("/item/type").and_then(Value::as_str) == Some("message")
        })
        .count();
    assert_eq!(
        added, 1,
        "image must not open a second message, got {bodies:?}"
    );
    let done = bodies.iter().find(|body| {
        body.get("type").and_then(Value::as_str) == Some("response.output_item.done")
            && body.pointer("/item/type").and_then(Value::as_str) == Some("message")
    });
    let Some(done) = done else {
        panic!("missing message done, got {bodies:?}");
    };
    assert_eq!(
        done.pointer("/item/content/0/text").and_then(Value::as_str),
        Some("before"),
        "{done}"
    );
    assert_eq!(
        done.pointer("/item/content/1/type").and_then(Value::as_str),
        Some("output_image"),
        "{done}"
    );
    assert_eq!(
        done.pointer("/item/content/2/text").and_then(Value::as_str),
        Some("after"),
        "{done}"
    );
    let done_text: Vec<(u64, &str)> = bodies
        .iter()
        .filter(|body| {
            body.get("type").and_then(Value::as_str) == Some("response.output_text.done")
        })
        .filter_map(|body| {
            Some((
                body.get("content_index").and_then(Value::as_u64)?,
                body.get("text").and_then(Value::as_str)?,
            ))
        })
        .collect();
    assert_eq!(
        done_text,
        vec![(0, "before"), (2, "after")],
        "each text part needs its own done event, got {bodies:?}"
    );
}

#[test]
fn dest_converse_citation_uses_flushed_caption_text() {
    let events = [
        IrStreamEvent::TextDelta {
            text: "caption".into(),
        },
        IrStreamEvent::ImageDelta {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        IrStreamEvent::AnnotationAdded {
            annotation: json!({ "type": "url_citation", "url": "https://example.com" }),
        },
    ];
    let body = encode_response(Wire::Converse, &events).expect("converse");
    assert_eq!(
        body.pointer("/output/message/content/0/citationsContent/content/0/text")
            .and_then(Value::as_str),
        Some("caption"),
        "flushed caption must move into citationsContent, got {body}"
    );
    assert!(
        body.pointer("/output/message/content/1/image").is_some(),
        "image must stay beside the citation, got {body}"
    );
    let plain = body
        .pointer("/output/message/content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("text").and_then(Value::as_str) == Some("caption"))
        });
    assert!(
        !plain,
        "caption must not also be a plain text block, got {body}"
    );

    let split = [
        IrStreamEvent::TextDelta {
            text: "before".into(),
        },
        IrStreamEvent::ImageDelta {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        IrStreamEvent::TextDelta {
            text: "after".into(),
        },
        IrStreamEvent::AnnotationAdded {
            annotation: json!({ "type": "url_citation", "url": "https://example.com" }),
        },
    ];
    let split_body = encode_response(Wire::Converse, &split).expect("split");
    assert_eq!(
        split_body
            .pointer("/output/message/content/0/citationsContent/content/0/text")
            .and_then(Value::as_str),
        Some("before"),
        "the prefix stays in the first citations block, got {split_body}"
    );
    assert!(
        split_body
            .pointer("/output/message/content/1/image")
            .is_some(),
        "the image stays between the text runs, got {split_body}"
    );
    assert_eq!(
        split_body
            .pointer("/output/message/content/2/text")
            .and_then(Value::as_str),
        Some("after"),
        "text after the image stays after it, got {split_body}"
    );
}

#[test]
fn split_text_around_an_image_keeps_sidecars_on_the_first_segment() {
    let events = [
        IrStreamEvent::TextDelta {
            text: "before".into(),
        },
        IrStreamEvent::ImageDelta {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        IrStreamEvent::TextDelta {
            text: "after".into(),
        },
        IrStreamEvent::AnnotationAdded {
            annotation: json!({ "type": "url_citation", "url": "https://example.com" }),
        },
    ];
    let messages = encode_response(Wire::Messages, &events).expect("messages");
    assert!(
        messages
            .pointer("/content/0/citations")
            .and_then(Value::as_array)
            .is_some_and(|c| !c.is_empty()),
        "citations belong on the first text segment, got {messages}"
    );
    assert!(
        messages.pointer("/content/2/citations").is_none(),
        "the text after the image must not take the citations, got {messages}"
    );
    let responses = encode_response(Wire::Responses, &events).expect("responses");
    assert!(
        responses
            .pointer("/output/0/content/0/annotations")
            .and_then(Value::as_array)
            .is_some_and(|c| !c.is_empty()),
        "annotations belong on the first text segment, got {responses}"
    );
    assert!(
        responses
            .pointer("/output/0/content/2/annotations")
            .is_none(),
        "the text after the image must not take the annotations, got {responses}"
    );
}

#[test]
fn dest_gemini_text_and_image_stream_uses_distinct_block_indexes() {
    let events = [
        IrStreamEvent::TextDelta {
            text: "here".into(),
        },
        IrStreamEvent::ImageDelta {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
    ];
    let messages = encode_all(Wire::Messages, &events);
    let bodies = sse_json_frames(&messages);
    let text_index = bodies.iter().find_map(|body| {
        (body.get("type").and_then(Value::as_str) == Some("content_block_delta"))
            .then(|| body.get("index").and_then(Value::as_u64))
            .flatten()
    });
    let image_index = bodies.iter().find_map(|body| {
        (body.pointer("/content_block/type").and_then(Value::as_str) == Some("image"))
            .then(|| body.get("index").and_then(Value::as_u64))
            .flatten()
    });
    assert_eq!(text_index, Some(0), "text block index, got {bodies:?}");
    assert_eq!(image_index, Some(1), "image block index, got {bodies:?}");
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/content_block/source/data")
                .and_then(Value::as_str)
        }),
        Some("iVBORw0KGgo="),
        "dest Messages stream must keep image bytes, got {bodies:?}"
    );
}

#[test]
fn messages_complete_image_block_keeps_preceding_text() {
    let body = serde_json::to_vec(&json!({
        "content": [
            { "type": "text", "text": "before" },
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "iVBORw0KGgo="
                }
            },
            { "type": "text", "text": "after" },
            {
                "type": "tool_use",
                "id": "toolu_1",
                "name": "lookup",
                "input": { "q": "x" }
            }
        ]
    }))
    .expect("json");
    let events = decode_response(Wire::Messages, &body, &messages_profile()).expect("decode");
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::TextDelta { text } if text == "before" => Some("text"),
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/png" && data == "iVBORw0KGgo=" =>
            {
                Some("image")
            }
            IrStreamEvent::TextDelta { text } if text == "after" => Some("text"),
            IrStreamEvent::ToolCallStart { name, .. } if name == "lookup" => Some("tool"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        ["text", "image", "text", "tool"],
        "image must follow the leading text and keep the tool block, got {events:?}"
    );
}

#[test]
fn messages_stream_content_block_start_image_becomes_image_delta() {
    let raw = RawSse {
        event: Some("content_block_start".into()),
        data: json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "iVBORw0KGgo="
                }
            }
        })
        .to_string(),
    };
    let events =
        decode_stream_events(Wire::Messages, &raw, &messages_profile()).expect("decode image");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/png" && data == "iVBORw0KGgo="
        )),
        "content_block_start image must become ImageDelta, got {events:?}"
    );
    let singular = decode_stream_event(Wire::Messages, &raw, &messages_profile())
        .expect("singular")
        .expect("event");
    assert!(
        matches!(
            singular,
            IrStreamEvent::ImageDelta { ref media_type, ref data }
                if media_type == "image/png" && data == "iVBORw0KGgo="
        ),
        "singular decode must return the image, got {singular:?}"
    );
}

#[test]
fn responses_complete_output_image_data_url_becomes_image_delta() {
    let body = serde_json::to_vec(&json!({
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_image",
                "image_url": "data:image/png;base64,iVBORw0KGgo="
            }]
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile()).expect("decode");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/png" && data == "iVBORw0KGgo="
        )),
        "output_image data URL must become ImageDelta, got {events:?}"
    );

    let https = serde_json::to_vec(&json!({
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_image",
                "image_url": "https://example.com/a.png"
            }]
        }]
    }))
    .expect("json");
    let https_events =
        decode_response(Wire::Responses, &https, &responses_profile()).expect("https");
    assert!(
        !https_events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ImageDelta { .. })),
        "https image_url must not become ImageDelta, got {https_events:?}"
    );
}

#[test]
fn responses_stream_output_image_data_url_becomes_image_delta() {
    let raw = RawSse {
        event: Some("response.output_item.added".into()),
        data: json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [
                    { "type": "output_text", "text": "before" },
                    {
                        "type": "output_image",
                        "image_url": "data:image/png;base64,iVBORw0KGgo="
                    }
                ]
            }
        })
        .to_string(),
    };
    let events =
        decode_stream_events(Wire::Responses, &raw, &responses_profile()).expect("decode image");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/png" && data == "iVBORw0KGgo="
        )),
        "message output_image data URL must become ImageDelta, got {events:?}"
    );
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::TextDelta { text } if text == "before" => Some("text"),
            IrStreamEvent::ImageDelta { .. } => Some("image"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        ["text", "image"],
        "text before the image on an added message must stay first, got {events:?}"
    );

    let part = RawSse {
        event: Some("response.content_part.added".into()),
        data: json!({
            "type": "response.content_part.added",
            "output_index": 0,
            "content_index": 1,
            "part": {
                "type": "output_image",
                "image_url": "data:image/webp;base64,UklGR"
            }
        })
        .to_string(),
    };
    let part_events =
        decode_stream_events(Wire::Responses, &part, &responses_profile()).expect("part");
    assert!(
        part_events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/webp" && data == "UklGR"
        )),
        "content_part.added output_image must become ImageDelta, got {part_events:?}"
    );
}

#[test]
fn converse_complete_image_jpeg_bytes_become_image_delta() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [
                    { "text": "before" },
                    { "image": { "format": "jpeg", "source": { "bytes": "/9j/" } } },
                    { "audio": { "format": "mp3", "source": { "bytes": "SUQz" } } },
                    { "toolUse": { "toolUseId": "t1", "name": "lookup", "input": {} } }
                ]
            }
        }
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile()).expect("decode");
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::TextDelta { text } if text == "before" => Some("text"),
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/jpeg" && data == "/9j/" =>
            {
                Some("image")
            }
            IrStreamEvent::AudioDelta { data } if data == "SUQz" => Some("audio"),
            IrStreamEvent::ToolCallStart { name, .. } if name == "lookup" => Some("tool"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        ["text", "image", "audio", "tool"],
        "jpeg bytes must become image/jpeg and keep text, audio, and tool order, got {events:?}"
    );
}

#[test]
fn converse_stream_content_block_start_image_png_becomes_image_delta() {
    let raw = RawSse {
        event: None,
        data: json!({
            "contentBlockStart": {
                "start": {
                    "image": {
                        "format": "png",
                        "source": { "bytes": "iVBORw0KGgo=" }
                    }
                }
            }
        })
        .to_string(),
    };
    let events =
        decode_stream_events(Wire::Converse, &raw, &converse_profile()).expect("decode image");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ImageDelta { media_type, data }
                if media_type == "image/png" && data == "iVBORw0KGgo="
        )),
        "contentBlockStart image must become ImageDelta, got {events:?}"
    );
}

#[test]
fn converse_stream_empty_image_bytes_are_not_image_delta() {
    let raw = RawSse {
        event: None,
        data: json!({
            "contentBlockStart": {
                "start": {
                    "image": {
                        "format": "png",
                        "source": { "bytes": "" }
                    }
                }
            }
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Converse, &raw, &converse_profile()).expect("empty");
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ImageDelta { .. })),
        "empty Converse image bytes must not become ImageDelta, got {events:?}"
    );
    let singular =
        decode_stream_event(Wire::Converse, &raw, &converse_profile()).expect("singular");
    assert!(
        singular.is_none(),
        "empty Converse image bytes must stay a no-op, got {singular:?}"
    );
}

#[test]
fn dest_chat_complete_tool_call_id_remaps_dest_gemini_function_call_id() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r69",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete tool_calls id");
    let gemini = encode_response(Wire::Gemini, &events).expect("encode dest Gemini complete");
    assert_eq!(
        gemini
            .pointer("/candidates/0/content/parts/0/functionCall/id")
            .and_then(Value::as_str),
        Some("call_1"),
        "dest Chat complete tool_calls[].id remapped dest Gemini complete must write functionCall.id, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/candidates/0/content/parts/0/functionCall/name")
            .and_then(Value::as_str),
        Some("get_weather"),
        "dest Chat complete tool_calls remapped dest Gemini complete must still write functionCall.name, got {gemini}"
    );
}

#[test]
fn dest_gemini_complete_prompt_tokens_details_audio_remaps_dest_chat_audio_tokens() {
    let body = serde_json::to_vec(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "Hi" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 50,
            "candidatesTokenCount": 2,
            "promptTokensDetails": [
                { "modality": "TEXT", "tokenCount": 10 },
                { "modality": "AUDIO", "tokenCount": 40 },
                { "modality": "IMAGE", "tokenCount": 8 }
            ]
        }
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &body, &gemini_profile())
        .expect("decode dest Gemini complete promptTokensDetails AUDIO");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/usage/prompt_tokens_details/audio_tokens")
            .and_then(Value::as_u64),
        Some(40),
        "dest Gemini complete promptTokensDetails AUDIO remapped dest Chat complete must write prompt_tokens_details.audio_tokens, got {chat}"
    );
    assert!(
        chat.pointer("/usage/prompt_tokens_details/image_tokens")
            .is_none(),
        "dest Gemini IMAGE promptTokensDetails dest Chat has no image_tokens (official Drop), got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Gemini complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_chat_complete_audio_tokens_remaps_dest_gemini_prompt_tokens_details() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-audio",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o-audio-preview",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 50,
            "completion_tokens": 2,
            "total_tokens": 52,
            "prompt_tokens_details": { "audio_tokens": 40 }
        }
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete prompt_tokens_details.audio_tokens");
    let gemini = encode_response(Wire::Gemini, &events).expect("encode dest Gemini complete");
    assert_eq!(
        gemini
            .pointer("/usageMetadata/promptTokensDetails/0/modality")
            .and_then(Value::as_str),
        Some("AUDIO"),
        "dest Chat complete audio_tokens remapped dest Gemini complete must write promptTokensDetails AUDIO, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/usageMetadata/promptTokensDetails/0/tokenCount")
            .and_then(Value::as_u64),
        Some(40),
        "dest Chat complete audio_tokens remapped dest Gemini complete must write promptTokensDetails tokenCount, got {gemini}"
    );
}

#[test]
fn dest_gemini_complete_candidates_tokens_details_audio_remaps_dest_chat_completion_audio_tokens() {
    let body = serde_json::to_vec(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "Hi" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 50,
            "candidatesTokenCount": 12,
            "promptTokensDetails": [
                { "modality": "AUDIO", "tokenCount": 40 }
            ],
            "candidatesTokensDetails": [
                { "modality": "TEXT", "tokenCount": 2 },
                { "modality": "AUDIO", "tokenCount": 7 },
                { "modality": "IMAGE", "tokenCount": 3 }
            ]
        }
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &body, &gemini_profile())
        .expect("decode dest Gemini complete candidatesTokensDetails AUDIO");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/usage/completion_tokens_details/audio_tokens")
            .and_then(Value::as_u64),
        Some(7),
        "dest Gemini complete candidatesTokensDetails AUDIO remapped dest Chat complete must write completion_tokens_details.audio_tokens, got {chat}"
    );
    assert_eq!(
        chat.pointer("/usage/prompt_tokens_details/audio_tokens")
            .and_then(Value::as_u64),
        Some(40),
        "dest Gemini complete promptTokensDetails AUDIO remapped dest Chat complete must still write prompt_tokens_details.audio_tokens, got {chat}"
    );
    assert!(
        chat.pointer("/usage/completion_tokens_details/image_tokens")
            .is_none(),
        "dest Gemini IMAGE candidatesTokensDetails dest Chat has no image_tokens (official Drop), got {chat}"
    );
    assert!(
        chat.pointer("/usage/completion_tokens_details/text_tokens")
            .is_none(),
        "dest Gemini TEXT candidatesTokensDetails dest Chat Completions has no text_tokens (official Drop), got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Gemini complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_chat_complete_completion_audio_tokens_remaps_dest_gemini_candidates_tokens_details() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-audio",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o-audio-preview",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 50,
            "completion_tokens": 12,
            "total_tokens": 62,
            "completion_tokens_details": { "audio_tokens": 7 }
        }
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete completion_tokens_details.audio_tokens");
    let gemini = encode_response(Wire::Gemini, &events).expect("encode dest Gemini complete");
    assert_eq!(
        gemini
            .pointer("/usageMetadata/candidatesTokensDetails/0/modality")
            .and_then(Value::as_str),
        Some("AUDIO"),
        "dest Chat complete completion audio_tokens remapped dest Gemini complete must write candidatesTokensDetails AUDIO, got {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/usageMetadata/candidatesTokensDetails/0/tokenCount")
            .and_then(Value::as_u64),
        Some(7),
        "dest Chat complete completion audio_tokens remapped dest Gemini complete must write candidatesTokensDetails tokenCount, got {gemini}"
    );
}

#[test]
fn dest_gemini_complete_candidates_audio_and_thoughts_keep_chat_completion_details() {
    let body = serde_json::to_vec(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "Hi" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 12,
            "thoughtsTokenCount": 5,
            "candidatesTokensDetails": [
                { "modality": "AUDIO", "tokenCount": 7 }
            ]
        }
    }))
    .expect("json");
    let events = decode_response(Wire::Gemini, &body, &gemini_profile())
        .expect("decode dest Gemini complete thoughts plus candidates AUDIO");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/usage/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
        Some(5),
        "dest Gemini thoughtsTokenCount remapped dest Chat complete must keep completion_tokens_details.reasoning_tokens, got {chat}"
    );
    assert_eq!(
        chat.pointer("/usage/completion_tokens_details/audio_tokens")
            .and_then(Value::as_u64),
        Some(7),
        "dest Gemini candidatesTokensDetails AUDIO remapped dest Chat complete must keep completion_tokens_details.audio_tokens next to reasoning_tokens, got {chat}"
    );
}

#[test]
fn dest_messages_complete_usage_service_tier_remaps_dest_chat_service_tier() {
    let body = serde_json::to_vec(&json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4",
        "content": [{ "type": "text", "text": "Hi" }],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 2,
            "service_tier": "priority"
        }
    }))
    .expect("json");
    let events = decode_response(Wire::Messages, &body, &messages_profile())
        .expect("decode dest Messages complete usage.service_tier");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.get("service_tier").and_then(Value::as_str),
        Some("priority"),
        "dest Messages complete usage.service_tier remapped dest Chat complete must write service_tier, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Messages complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_chat_complete_service_tier_remaps_dest_messages_usage_service_tier() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r67",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "service_tier": "priority",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi" },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete service_tier");
    let messages = encode_response(Wire::Messages, &events).expect("encode dest Messages complete");
    assert_eq!(
        messages
            .pointer("/usage/service_tier")
            .and_then(Value::as_str),
        Some("priority"),
        "dest Chat complete service_tier remapped dest Messages complete must write usage.service_tier, got {messages}"
    );
    assert_eq!(
        messages.pointer("/content/0/text").and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Messages complete must still carry text Hi, got {messages}"
    );
}

#[test]
fn dest_messages_stream_usage_service_tier_remaps_dest_chat_stream_service_tier() {
    let raw = RawSse {
        event: Some("message_start".into()),
        data: json!({
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-sonnet-4",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 0,
                    "service_tier": "priority"
                }
            }
        })
        .to_string(),
    };
    let mut events = decode_stream_events(Wire::Messages, &raw, &messages_profile())
        .expect("decode dest Messages STREAM message.usage.service_tier");
    let delta = RawSse {
        event: Some("content_block_delta".into()),
        data: json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "Hi" }
        })
        .to_string(),
    };
    events.extend(
        decode_stream_events(Wire::Messages, &delta, &messages_profile())
            .expect("decode dest Messages STREAM text delta"),
    );
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies
            .iter()
            .find_map(|body| body.get("service_tier").and_then(Value::as_str)),
        Some("priority"),
        "dest Messages STREAM message.usage.service_tier remapped dest Chat STREAM must write service_tier, got {frames:?}"
    );
    assert!(
        bodies.iter().any(|body| {
            body.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                == Some("Hi")
        }),
        "dest Messages STREAM text remapped dest Chat STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_service_tier_remaps_dest_messages_stream_usage_service_tier() {
    let raw = RawSse {
        event: None,
        data: json!({
            "id": "chatcmpl-r67s",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "service_tier": "priority",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM service_tier");
    let frames = encode_all(Wire::Messages, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/message/usage/service_tier")
                .or_else(|| body.pointer("/usage/service_tier"))
                .and_then(Value::as_str)
        }),
        Some("priority"),
        "dest Chat STREAM service_tier remapped dest Messages STREAM must write message_start or message_delta usage.service_tier, got {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            matches!(
                frame.event.as_deref(),
                Some("message_start") | Some("message_delta")
            ) && frame.data.contains("\"service_tier\":\"priority\"")
        }),
        "dest Chat STREAM service_tier remapped dest Messages STREAM must not be empty text_delta, got {frames:?}"
    );
    assert!(
        bodies
            .iter()
            .any(|body| { body.pointer("/delta/text").and_then(Value::as_str) == Some("Hi") }),
        "dest Chat STREAM text remapped dest Messages STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_two_chunks_service_tier_remaps_dest_messages_once_on_message_start() {
    let first = RawSse {
        event: None,
        data: json!({
            "id": "chatcmpl-r67s2",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "service_tier": "priority",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" }
            }]
        })
        .to_string(),
    };
    let mut events = decode_stream_events(Wire::ChatCompletions, &first, &chat_profile())
        .expect("decode dest Chat STREAM first service_tier chunk");
    let second = RawSse {
        event: None,
        data: json!({
            "id": "chatcmpl-r67s2",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "service_tier": "priority",
            "choices": [{
                "index": 0,
                "delta": { "content": " there" }
            }]
        })
        .to_string(),
    };
    events.extend(
        decode_stream_events(Wire::ChatCompletions, &second, &chat_profile())
            .expect("decode dest Chat STREAM second service_tier chunk"),
    );
    let frames = encode_all(Wire::Messages, &events);
    let bodies = sse_json_frames(&frames);
    let start_tiers: Vec<String> = frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("message_start"))
        .filter_map(|frame| serde_json::from_str::<Value>(&frame.data).ok())
        .filter_map(|body| {
            body.pointer("/message/usage/service_tier")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        start_tiers.as_slice(),
        ["priority"],
        "dest Chat STREAM two chunks both with service_tier remapped dest Messages STREAM must write usage.service_tier once on message_start, got {frames:?}"
    );
    let empty_delta_tiers = frames
        .iter()
        .filter(|frame| {
            frame.event.as_deref() == Some("message_delta")
                && serde_json::from_str::<Value>(&frame.data)
                    .ok()
                    .is_some_and(|body| {
                        body.get("delta") == Some(&json!({}))
                            && body.pointer("/usage/service_tier").is_some()
                    })
        })
        .count();
    assert_eq!(
        empty_delta_tiers, 0,
        "dest Chat STREAM two chunks remapped dest Messages STREAM must not emit empty message_delta usage.service_tier between content_block_deltas, got {frames:?}"
    );
    let texts: Vec<&str> = bodies
        .iter()
        .filter_map(|body| body.pointer("/delta/text").and_then(Value::as_str))
        .collect();
    assert!(
        texts.contains(&"Hi") && texts.contains(&" there"),
        "dest Chat STREAM two chunks remapped dest Messages STREAM must still carry both text deltas, got {frames:?}"
    );
}

#[test]
fn dest_messages_complete_stop_details_explanation_remaps_dest_chat_refusal() {
    let body = serde_json::to_vec(&json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4",
        "content": [],
        "stop_reason": "refusal",
        "stop_details": {
            "type": "refusal",
            "explanation": "nope"
        }
    }))
    .expect("json");
    let events = decode_response(Wire::Messages, &body, &messages_profile())
        .expect("decode dest Messages complete stop_details.explanation");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/refusal")
            .and_then(Value::as_str),
        Some("nope"),
        "dest Messages complete stop_details.explanation remapped dest Chat complete must write message.refusal, got {chat}"
    );
}

#[test]
fn dest_chat_complete_refusal_remaps_dest_messages_stop_details_explanation() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r68",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "refusal": "nope"
            },
            "finish_reason": "content_filter"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete message.refusal");
    let messages = encode_response(Wire::Messages, &events).expect("encode dest Messages complete");
    assert_eq!(
        messages
            .pointer("/stop_details/explanation")
            .and_then(Value::as_str),
        Some("nope"),
        "dest Chat complete message.refusal remapped dest Messages complete must write stop_details.explanation, got {messages}"
    );
    assert_eq!(
        messages
            .pointer("/stop_details/type")
            .and_then(Value::as_str),
        Some("refusal"),
        "dest Chat complete message.refusal remapped dest Messages complete must write stop_details.type refusal, got {messages}"
    );
}

#[test]
fn dest_messages_stream_stop_details_explanation_remaps_dest_chat_refusal() {
    let raw = RawSse {
        event: Some("message_delta".into()),
        data: json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "refusal",
                "stop_sequence": null,
                "stop_details": {
                    "type": "refusal",
                    "explanation": "nope"
                }
            }
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Messages, &raw, &messages_profile())
        .expect("decode dest Messages STREAM stop_details.explanation");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/delta/refusal")
                .and_then(Value::as_str)
        }),
        Some("nope"),
        "dest Messages STREAM stop_details.explanation remapped dest Chat STREAM must write delta.refusal, got {frames:?}"
    );
    assert!(
        bodies.iter().all(|body| {
            body.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                != Some("nope")
        }),
        "dest Messages STREAM stop_details.explanation remapped dest Chat STREAM must not fold refusal into content, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_refusal_remaps_dest_messages_stop_details_explanation() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"refusal":"nope"}}]}"#.into(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM delta.refusal");
    let frames = encode_all(Wire::Messages, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/delta/stop_details/explanation")
                .and_then(Value::as_str)
        }),
        Some("nope"),
        "dest Chat STREAM delta.refusal remapped dest Messages STREAM must write delta.stop_details.explanation, got {frames:?}"
    );
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/delta/stop_details/type")
                .and_then(Value::as_str)
        }),
        Some("refusal"),
        "dest Chat STREAM delta.refusal remapped dest Messages STREAM must write delta.stop_details.type refusal, got {frames:?}"
    );
    assert!(
        bodies
            .iter()
            .all(|body| { body.pointer("/delta/text").and_then(Value::as_str) != Some("nope") }),
        "dest Chat STREAM delta.refusal remapped dest Messages STREAM must not fold refusal into text_delta, got {frames:?}"
    );
}

#[test]
fn dest_converse_complete_cache_read_remaps_dest_chat_cached_tokens() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [{ "text": "Hi" }]
            }
        },
        "stopReason": "end_turn",
        "usage": {
            "inputTokens": 10,
            "outputTokens": 3,
            "cacheReadInputTokens": 7
        }
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile())
        .expect("decode dest Converse complete cacheReadInputTokens");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/usage/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        Some(7),
        "dest Converse complete cacheReadInputTokens remapped dest Chat complete must write prompt_tokens_details.cached_tokens, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Converse complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_converse_complete_citations_content_text_remaps_dest_chat_content() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [{
                    "citationsContent": {
                        "content": [{ "text": "See https://example.com for more." }],
                        "citations": [{
                            "title": "Example Domain",
                            "source": "https://example.com",
                            "location": { "web": { "url": "https://example.com" } }
                        }]
                    }
                }]
            }
        },
        "stopReason": "end_turn"
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile())
        .expect("decode dest Converse complete citationsContent.content");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("See https://example.com for more."),
        "dest Converse complete citationsContent.content text remapped dest Chat complete must write message.content, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/annotations/0/url_citation/url")
            .and_then(Value::as_str),
        Some("https://example.com"),
        "dest Converse complete citationsContent citations remapped dest Chat complete must still write url_citation, got {chat}"
    );
}

#[test]
fn dest_converse_complete_citations_content_does_not_duplicate_sibling_text() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [{
                    "text": "See https://example.com for more.",
                    "citationsContent": {
                        "content": [{ "text": "See https://example.com for more." }],
                        "citations": [{
                            "location": { "web": { "url": "https://example.com" } }
                        }]
                    }
                }]
            }
        },
        "stopReason": "end_turn"
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile())
        .expect("decode dest Converse complete sibling text plus citationsContent");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("See https://example.com for more."),
        "dest Converse complete sibling text equal to citationsContent.content must not duplicate dest Chat message.content, got {chat}"
    );
}

#[test]
fn dest_converse_complete_reasoning_content_remaps_dest_chat_reasoning_content() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "reasoningContent": {
                            "reasoningText": {
                                "text": "plan",
                                "signature": "sig-1"
                            }
                        }
                    },
                    { "text": "Hi" }
                ]
            }
        },
        "stopReason": "end_turn"
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile())
        .expect("decode dest Converse complete reasoningContent");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/reasoning_content")
            .and_then(Value::as_str),
        Some("plan"),
        "dest Converse complete reasoningContent.reasoningText.text remapped dest Chat complete must write reasoning_content, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/reasoning_signature")
            .and_then(Value::as_str),
        Some("sig-1"),
        "dest Converse complete reasoningContent.reasoningText.signature remapped dest Chat complete must write reasoning_signature, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Converse complete sibling text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_chat_complete_reasoning_content_remaps_dest_converse_reasoning_content() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r103",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hi",
                "reasoning_content": "plan",
                "reasoning_signature": "sig-1"
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete reasoning_content");
    let converse = encode_response(Wire::Converse, &events).expect("encode dest Converse complete");
    assert_eq!(
        converse
            .pointer("/output/message/content/0/reasoningContent/reasoningText/text")
            .and_then(Value::as_str),
        Some("plan"),
        "dest Chat complete reasoning_content remapped dest Converse complete must write reasoningText.text, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/output/message/content/0/reasoningContent/reasoningText/signature")
            .and_then(Value::as_str),
        Some("sig-1"),
        "dest Chat complete reasoning_signature remapped dest Converse complete must write reasoningText.signature, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/output/message/content/1/text")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Chat complete content remapped dest Converse complete must still carry text Hi, got {converse}"
    );
}

#[test]
fn dest_chat_complete_reasoning_and_tool_calls_remaps_dest_converse_reasoning_before_tool_use() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r104",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "reasoning_content": "plan",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete reasoning_content plus tool_calls");
    let converse = encode_response(Wire::Converse, &events).expect("encode dest Converse complete");
    assert_eq!(
        converse
            .pointer("/output/message/content/0/reasoningContent/reasoningText/text")
            .and_then(Value::as_str),
        Some("plan"),
        "dest Chat complete reasoning_content plus tool_calls remapped dest Converse complete must write reasoningContent before toolUse, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/output/message/content/1/toolUse/name")
            .and_then(Value::as_str),
        Some("get_weather"),
        "dest Chat complete tool_calls remapped dest Converse complete must write toolUse after reasoningContent, got {converse}"
    );
}

#[test]
fn dest_converse_complete_audio_bytes_remaps_dest_chat_message_audio() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [{
                    "audio": {
                        "format": "mp3",
                        "source": { "bytes": "SUQz" }
                    }
                }]
            }
        },
        "stopReason": "end_turn"
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile())
        .expect("decode dest Converse complete audio.source.bytes");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/message/audio/data")
            .and_then(Value::as_str),
        Some("SUQz"),
        "dest Converse complete audio.source.bytes remapped dest Chat complete must write message.audio.data, got {chat}"
    );
    assert!(
        chat.pointer("/choices/0/message/audio/id").is_none(),
        "dest Converse complete audio remapped dest Chat complete must not invent audio.id, got {chat}"
    );
    assert!(
        chat.pointer("/choices/0/message/audio/expires_at")
            .is_none(),
        "dest Converse complete audio remapped dest Chat complete must not invent audio.expires_at, got {chat}"
    );
}

#[test]
fn dest_chat_complete_message_audio_remaps_dest_converse_audio_bytes() {
    let body = serde_json::to_vec(&json!({
        "id": "chatcmpl-r104",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "audio": { "data": "SUQz" }
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::ChatCompletions, &body, &chat_profile())
        .expect("decode dest Chat complete message.audio.data");
    let converse = encode_response(Wire::Converse, &events).expect("encode dest Converse complete");
    assert_eq!(
        converse
            .pointer("/output/message/content/0/audio/source/bytes")
            .and_then(Value::as_str),
        Some("SUQz"),
        "dest Chat complete message.audio.data remapped dest Converse complete must write audio.source.bytes, got {converse}"
    );
    assert_eq!(
        converse
            .pointer("/output/message/content/0/audio/format")
            .and_then(Value::as_str),
        Some("mp3"),
        "dest Chat complete message.audio.data remapped dest Converse complete must write audio.format mp3, got {converse}"
    );
    assert!(
        converse
            .pointer("/output/message/content/0/audio/source/s3Location")
            .is_none(),
        "dest Chat complete message.audio remapped dest Converse complete must not invent audio.source.s3Location, got {converse}"
    );
}

#[test]
fn dest_responses_complete_cancelled_remaps_dest_chat_finish_reason_stop() {
    let body = serde_json::to_vec(&json!({
        "id": "resp_1",
        "object": "response",
        "created_at": 1700000000,
        "status": "cancelled",
        "model": "gpt-4o",
        "output": []
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile())
        .expect("decode dest Responses complete cancelled");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/finish_reason")
            .and_then(Value::as_str),
        Some("stop"),
        "dest Responses complete status cancelled remapped dest Chat complete must write dest Chat finish_reason stop, got {chat}"
    );
}

#[test]
fn dest_converse_complete_end_turn_remaps_dest_chat_finish_reason_stop() {
    let body = serde_json::to_vec(&json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [{ "text": "Hi" }]
            }
        },
        "stopReason": "end_turn"
    }))
    .expect("json");
    let events = decode_response(Wire::Converse, &body, &converse_profile())
        .expect("decode dest Converse complete end_turn");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/finish_reason")
            .and_then(Value::as_str),
        Some("stop"),
        "dest Converse complete stopReason end_turn remapped dest Chat complete must write dest Chat finish_reason stop, got {chat}"
    );
}

#[test]
fn dest_messages_complete_stop_sequence_remaps_dest_chat_finish_reason_stop() {
    let body = serde_json::to_vec(&json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4",
        "content": [{ "type": "text", "text": "Hi" }],
        "stop_reason": "stop_sequence"
    }))
    .expect("json");
    let events = decode_response(Wire::Messages, &body, &messages_profile())
        .expect("decode dest Messages complete stop_sequence");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/finish_reason")
            .and_then(Value::as_str),
        Some("stop"),
        "dest Messages complete stop_reason stop_sequence remapped dest Chat complete must write dest Chat finish_reason stop, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Messages complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_responses_complete_output_text_logprobs_remaps_dest_chat_complete() {
    let body = serde_json::to_vec(&json!({
        "id": "resp_1",
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": "Hi",
                "logprobs": [{
                    "token": "Hi",
                    "logprob": -0.1,
                    "bytes": [72, 105],
                    "top_logprobs": [{
                        "token": "Hi",
                        "logprob": -0.1,
                        "bytes": [72, 105]
                    }]
                }]
            }]
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile())
        .expect("decode dest Responses complete output_text logprobs");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/choices/0/logprobs/content/0/token")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Responses complete output_text logprobs remapped dest Chat complete must write logprobs.content token, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/logprobs/content/0/logprob")
            .and_then(Value::as_f64),
        Some(-0.1),
        "dest Responses complete output_text logprobs remapped dest Chat complete must write logprobs.content logprob, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Responses complete output_text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_responses_complete_metadata_remaps_dest_chat_complete_metadata() {
    let body = serde_json::to_vec(&json!({
        "id": "resp_1",
        "object": "response",
        "created_at": 1700000000,
        "status": "completed",
        "model": "gpt-4o",
        "metadata": { "user": "alice" },
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "Hi" }]
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile())
        .expect("decode dest Responses complete metadata");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/metadata/user").and_then(Value::as_str),
        Some("alice"),
        "dest Responses complete metadata remapped dest Chat complete must write metadata.user, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Responses complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
}

#[test]
fn dest_responses_complete_moderation_remaps_dest_chat_complete_moderation() {
    let body = serde_json::to_vec(&json!({
        "id": "resp_1",
        "object": "response",
        "status": "completed",
        "moderation": {
            "input": {
                "type": "moderation_result",
                "model": "omni-moderation-latest",
                "flagged": false,
                "categories": { "hate": false },
                "category_scores": { "hate": 0.0 },
                "category_applied_input_types": { "hate": ["text"] }
            },
            "output": {
                "type": "moderation_result",
                "model": "omni-moderation-latest",
                "flagged": false,
                "categories": { "hate": false },
                "category_scores": { "hate": 0.0 },
                "category_applied_input_types": { "hate": ["text"] }
            }
        },
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "Hi" }]
        }]
    }))
    .expect("json");
    let events = decode_response(Wire::Responses, &body, &responses_profile())
        .expect("decode dest Responses complete moderation");
    let chat = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat complete");
    assert_eq!(
        chat.pointer("/moderation/input/type")
            .and_then(Value::as_str),
        Some("moderation_results"),
        "dest Responses complete moderation.input remapped dest Chat complete must write moderation.input.type moderation_results, got {chat}"
    );
    assert_eq!(
        chat.pointer("/moderation/input/results/0/flagged")
            .and_then(Value::as_bool),
        Some(false),
        "dest Responses complete moderation.input remapped dest Chat complete must write results[0].flagged false, got {chat}"
    );
    assert_eq!(
        chat.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("Hi"),
        "dest Responses complete text remapped dest Chat complete must still carry text Hi, got {chat}"
    );
    let chat_bytes = serde_json::to_vec(&chat).expect("chat json");
    let from_chat = decode_response(Wire::ChatCompletions, &chat_bytes, &chat_profile())
        .expect("decode dest Chat complete moderation");
    let responses =
        encode_response(Wire::Responses, &from_chat).expect("encode dest Responses complete");
    assert_eq!(
        responses
            .pointer("/moderation/input/type")
            .and_then(Value::as_str),
        Some("moderation_result"),
        "dest Chat complete moderation.input remapped dest Responses complete must write moderation_result, got {responses}"
    );
    assert_eq!(
        responses
            .pointer("/moderation/input/flagged")
            .and_then(Value::as_bool),
        Some(false),
        "dest Chat complete moderation.input remapped dest Responses complete must write flagged false, got {responses}"
    );
}

#[test]
fn dest_responses_stream_moderation_remaps_dest_chat_stream_moderation() {
    let raw = RawSse {
        event: Some("response.completed".into()),
        data: json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1",
                "status": "completed",
                "moderation": {
                    "input": {
                        "type": "moderation_result",
                        "model": "omni-moderation-latest",
                        "flagged": false,
                        "categories": { "hate": false },
                        "category_scores": { "hate": 0.0 },
                        "category_applied_input_types": { "hate": ["text"] }
                    },
                    "output": {
                        "type": "moderation_result",
                        "model": "omni-moderation-latest",
                        "flagged": false,
                        "categories": { "hate": false },
                        "category_scores": { "hate": 0.0 },
                        "category_applied_input_types": { "hate": ["text"] }
                    }
                }
            }
        })
        .to_string(),
    };
    let mut events = decode_stream_events(Wire::Responses, &raw, &responses_profile())
        .expect("decode dest Responses STREAM moderation");
    let delta = RawSse {
        event: Some("response.output_text.delta".into()),
        data: json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "delta": "Hi"
        })
        .to_string(),
    };
    events.extend(
        decode_stream_events(Wire::Responses, &delta, &responses_profile())
            .expect("decode dest Responses STREAM text delta"),
    );
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/moderation/input/type")
                .and_then(Value::as_str)
        }),
        Some("moderation_results"),
        "dest Responses STREAM response.completed moderation remapped dest Chat STREAM must write moderation.input.type moderation_results, got {frames:?}"
    );
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/moderation/input/results/0/flagged")
                .and_then(Value::as_bool)
        }),
        Some(false),
        "dest Responses STREAM response.completed moderation remapped dest Chat STREAM must write results[0].flagged false, got {frames:?}"
    );
    assert!(
        bodies.iter().any(|body| {
            body.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                == Some("Hi")
        }),
        "dest Responses STREAM text remapped dest Chat STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_moderation_remaps_dest_responses_stream_moderation() {
    let raw = RawSse {
        event: None,
        data: json!({
            "moderation": {
                "input": {
                    "type": "moderation_results",
                    "model": "omni-moderation-latest",
                    "results": [{
                        "flagged": false,
                        "categories": { "hate": false },
                        "category_scores": { "hate": 0.0 },
                        "category_applied_input_types": { "hate": ["text"] }
                    }]
                },
                "output": {
                    "type": "moderation_results",
                    "model": "omni-moderation-latest",
                    "results": [{
                        "flagged": false,
                        "categories": { "hate": false },
                        "category_scores": { "hate": 0.0 },
                        "category_applied_input_types": { "hate": ["text"] }
                    }]
                }
            },
            "choices": []
        })
        .to_string(),
    };
    let mut events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM moderation");
    let delta = RawSse {
        event: None,
        data: json!({
            "choices": [{ "index": 0, "delta": { "content": "Hi" } }]
        })
        .to_string(),
    };
    events.extend(
        decode_stream_events(Wire::ChatCompletions, &delta, &chat_profile())
            .expect("decode dest Chat STREAM text delta"),
    );
    let frames = encode_all(Wire::Responses, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/response/moderation/input/type")
                .and_then(Value::as_str)
        }),
        Some("moderation_result"),
        "dest Chat STREAM moderation remapped dest Responses STREAM must write response.moderation.input.type moderation_result, got {frames:?}"
    );
    assert!(
        bodies
            .iter()
            .any(|body| { body.pointer("/delta").and_then(Value::as_str) == Some("Hi") }),
        "dest Chat STREAM text remapped dest Responses STREAM must still carry text Hi, got {frames:?}"
    );
}

#[test]
fn dest_chat_stream_logprobs_refusal_remaps_dest_gemini_logprobs_result() {
    let raw = RawSse {
        event: None,
        data: json!({
            "choices": [{
                "index": 0,
                "delta": { "refusal": "nope" },
                "logprobs": {
                    "refusal": [{
                        "token": "nope",
                        "logprob": -0.2,
                        "bytes": [110, 111, 112, 101],
                        "top_logprobs": [{
                            "token": "nope",
                            "logprob": -0.2,
                            "bytes": [110, 111, 112, 101]
                        }]
                    }]
                }
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode dest Chat STREAM logprobs.refusal");
    let frames = encode_all(Wire::Gemini, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/logprobsResult/chosenCandidates/0/token")
                .and_then(Value::as_str)
        }),
        Some("nope"),
        "dest Chat STREAM logprobs.refusal remapped dest Gemini must write logprobsResult token, got {frames:?}"
    );
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/candidates/0/logprobsResult/chosenCandidates/0/logProbability")
                .and_then(Value::as_f64)
        }),
        Some(-0.2),
        "dest Chat STREAM logprobs.refusal remapped dest Gemini must write logprobsResult logProbability, got {frames:?}"
    );
}

#[test]
fn dest_gemini_stream_logprobs_result_remaps_dest_chat() {
    let raw = RawSse {
        event: None,
        data: dest_gemini_stream_logprobs_body().to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini STREAM logprobsResult");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/logprobs/content/0/token")
                .and_then(Value::as_str)
        }),
        Some("Hi"),
        "dest Gemini STREAM logprobsResult remapped dest Chat must write choices.logprobs.content token, got {frames:?}"
    );
    assert_eq!(
        bodies.iter().find_map(|body| {
            body.pointer("/choices/0/logprobs/content/0/logprob")
                .and_then(Value::as_f64)
        }),
        Some(-0.1),
        "dest Gemini STREAM logprobsResult remapped dest Chat must write choices.logprobs.content logprob, got {frames:?}"
    );
}

#[test]
fn dest_gemini_stream_token_count_remaps_dest_chat_usage() {
    let raw = RawSse {
        event: None,
        data: json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Hi"}]},
                "tokenCount": 7,
                "finishReason": "STOP"
            }]
        })
        .to_string(),
    };
    let events = decode_stream_events(Wire::Gemini, &raw, &gemini_profile())
        .expect("decode dest Gemini STREAM tokenCount");
    let frames = encode_all(Wire::ChatCompletions, &events);
    let bodies = sse_json_frames(&frames);
    assert_eq!(
        bodies.iter().find_map(|body| body
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)),
        Some(7),
        "dest Gemini STREAM tokenCount remapped dest Chat must write usage.completion_tokens, got {frames:?}"
    );
    assert!(
        bodies.iter().any(|body| {
            body.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                == Some("Hi")
        }),
        "dest Gemini STREAM text remapped dest Chat must still carry Hi, got {frames:?}"
    );
}

#[test]
fn chat_eos_finish_reason_is_stop() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{},"finish_reason":"eos"}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::FinishReason { ref reason } if reason == "stop"),
        "eos must be stop, got {ev:?}"
    );
}

#[test]
fn chat_prompt_cache_hit_tokens_alias_is_read() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":1,"prompt_cache_hit_tokens":20}}"#
            .into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode prompt_cache_hit_tokens")
        .expect("usage");
    match ev {
        IrStreamEvent::Usage {
            prompt_tokens,
            cache_read_tokens,
            ..
        } => {
            assert_eq!(cache_read_tokens, 20);
            assert_eq!(prompt_tokens, 30);
        }
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn unknown_event_hard_error_and_passthrough() {
    let raw = RawSse {
        event: Some("vendor.foo".into()),
        data: r#"{"type":"vendor.foo","ok":true}"#.into(),
    };
    let hard = decode_stream_event(Wire::Messages, &raw, &messages_profile())
        .expect_err("default stream_unknown_policy is hard-error");
    match hard {
        MapError::HardError { path, detail } => {
            assert_eq!(path, "vendor.foo");
            assert!(detail.contains("vendor.foo"), "detail={detail}");
            assert!(
                detail.contains("stream_unknown_policy"),
                "must name stream_unknown_policy, got {detail}"
            );
            assert!(
                detail.contains("hard-error") && detail.contains("passthrough"),
                "must list stream_unknown_policy values, got {detail}"
            );
            assert!(
                detail.contains("stream_events"),
                "must mention adding the name to stream_events, got {detail}"
            );
        }
        other => panic!("expected HardError, got {other}"),
    }

    let pass = profile(
        r#"
schema_version = 1
id = "test-pass"
wire = "messages"
stream_unknown_policy = "passthrough"
"#,
    );
    let ev = decode_stream_event(Wire::Messages, &raw, &pass)
        .expect("passthrough")
        .expect("unknown forwarded");
    match ev {
        IrStreamEvent::Unknown { ref event, ref raw } => {
            assert_eq!(event, "vendor.foo");
            assert_eq!(raw["ok"], true);
        }
        other => panic!("expected Unknown, got {other:?}"),
    }

    let encoded = encode_stream_event(Wire::Messages, &ev).expect("re-encode unknown");
    assert_eq!(encoded.event.as_deref(), Some("vendor.foo"));
    let body: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(body["ok"], true);
}

#[test]
fn tool_bearing_unknown_is_never_dropped() {
    let raw = RawSse {
        event: Some("vendor.tool_blast".into()),
        data: r#"{"type":"vendor.tool_blast","tool_use":{"name":"get_weather"}}"#.into(),
    };
    let hard = profile(
        r#"
schema_version = 1
id = "test-hard"
wire = "messages"
stream_unknown_policy = "hard-error"
"#,
    );
    assert!(
        decode_stream_event(Wire::Messages, &raw, &hard).is_err(),
        "hard-error must fail closed on tool-bearing unknown"
    );

    let pass = profile(
        r#"
schema_version = 1
id = "test-pass-tool"
wire = "messages"
stream_unknown_policy = "passthrough"
"#,
    );
    let ev = decode_stream_event(Wire::Messages, &raw, &pass)
        .expect("passthrough tool-bearing")
        .expect("must not drop");
    assert!(
        matches!(ev, IrStreamEvent::Unknown { ref event, .. } if event == "vendor.tool_blast"),
        "passthrough must forward the frame, got {ev:?}"
    );
}

#[test]
fn responses_added_with_arguments_fans_out() {
    let raw = RawSse {
        event: Some("response.output_item.added".into()),
        data: r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{\"q\":\"x\"}"}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Responses, &raw, &responses_profile())
        .expect("decode")
        .expect("start");
    assert!(
        matches!(ev, IrStreamEvent::ToolCallStart { ref id, ref name, .. } if id == "call_1" && name == "lookup"),
        "1:1 keeps start, got {ev:?}"
    );
    let all = decode_stream_events(Wire::Responses, &raw, &responses_profile()).expect("fan-out");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"q":"x"}"#
        )),
        "fan-out must emit arguments from output_item.added, got {all:?}"
    );
}

#[test]
fn responses_function_call_stream_maps() {
    let text = r#"
event: response.output_item.added
data: {"type":"response.output_item.added","output_index":0,"item":{"id":"fc_redacted","type":"function_call","name":"get_weather","arguments":"","call_id":"call_redacted"}}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"location\":\"SF\"}"}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_redacted","name":"get_weather"}}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_redacted","status":"completed","usage":{"input_tokens":20,"output_tokens":10}}}
"#;
    let events = decode_all(Wire::Responses, text, &responses_profile()).expect("decode Responses");
    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::ToolCallStart { id, name, .. } if id == "call_redacted" && name == "get_weather")
        ),
        "function_call start missing: {events:?}"
    );
    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"location":"SF"}"#)
        ),
        "arg delta missing: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ToolCallEnd)),
        "output_item.done missing ToolCallEnd: {events:?}"
    );
    match events
        .iter()
        .find(|ev| matches!(ev, IrStreamEvent::Usage { .. }))
    {
        Some(IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            ..
        }) => {
            assert_eq!(*prompt_tokens, 20);
            assert_eq!(*completion_tokens, 10);
            assert_eq!(*cache_read_tokens, 0);
            assert_eq!(*cache_write_tokens, 0);
        }
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn encode_round_trip_text_and_tool_start() {
    let text = IrStreamEvent::TextDelta {
        text: "hello".into(),
    };
    for wire in [
        Wire::ChatCompletions,
        Wire::Messages,
        Wire::Responses,
        Wire::Gemini,
    ] {
        let profile = match wire {
            Wire::ChatCompletions => chat_profile(),
            Wire::Messages => messages_profile(),
            Wire::Responses => responses_profile(),
            Wire::Gemini => gemini_profile(),
            Wire::Converse => {
                parse_profile_str("schema_version = 1\nid = \"bedrock\"\nwire = \"converse\"\n")
                    .expect("converse profile")
            }
            _ => continue,
        };
        let raw = encode_stream_event(wire, &text).expect("encode text");
        let back = decode_stream_event(wire, &raw, &profile)
            .expect("decode text")
            .expect("text event");
        assert_eq!(back, text, "text round-trip {wire:?}");
    }

    let start = IrStreamEvent::ToolCallStart {
        id: "call_1".into(),
        name: "lookup".into(),
        thought_signature: None,
        index: 0,
    };
    for wire in [Wire::ChatCompletions, Wire::Messages, Wire::Responses] {
        let profile = match wire {
            Wire::ChatCompletions => chat_profile(),
            Wire::Messages => messages_profile(),
            Wire::Responses => responses_profile(),
            Wire::Gemini => gemini_profile(),
            Wire::Converse => {
                parse_profile_str("schema_version = 1\nid = \"bedrock\"\nwire = \"converse\"\n")
                    .expect("converse profile")
            }
            _ => continue,
        };
        let raw = encode_stream_event(wire, &start).expect("encode start");
        let back = decode_stream_event(wire, &raw, &profile)
            .expect("decode start")
            .expect("start event");
        assert_eq!(back, start, "tool start round-trip {wire:?}");
    }
}

#[test]
fn message_delta_stop_reason_wins_over_usage() {
    let both = RawSse {
        event: Some("message_delta".into()),
        data: r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":67}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Messages, &both, &messages_profile())
        .expect("decode combined message_delta")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::FinishReason { ref reason } if reason == "tool_calls"),
        "stop_reason must not be dropped for usage, got {ev:?}"
    );
    let all = decode_stream_events(Wire::Messages, &both, &messages_profile()).expect("events");
    assert!(
        all.iter().any(
            |ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "tool_calls")
        ),
        "finish must stay, got {all:?}"
    );
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::Usage { .. })),
        "usage on the same message_delta must not be dropped, got {all:?}"
    );

    let usage_only = RawSse {
        event: Some("message_delta".into()),
        data: r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":3}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Messages, &usage_only, &messages_profile())
        .expect("decode usage-only message_delta")
        .expect("event");
    assert!(
        matches!(
            ev,
            IrStreamEvent::Usage {
                completion_tokens: 3,
                ..
            }
        ),
        "usage-only message_delta should stay Usage, got {ev:?}"
    );
}

#[test]
fn responses_reasoning_delta_is_not_output_text() {
    let ev = IrStreamEvent::ReasoningDelta {
        text: "hidden thought".into(),
    };
    let raw = encode_stream_event(Wire::Responses, &ev).expect("encode reasoning");
    assert_ne!(
        raw.event.as_deref(),
        Some("response.output_text.delta"),
        "thinking must not become assistant output_text"
    );
    let body: Value = serde_json::from_str(&raw.data).expect("json");
    assert_ne!(
        body.get("type").and_then(Value::as_str),
        Some("response.output_text.delta")
    );
    assert_eq!(body["item"]["type"], "reasoning");
    assert_eq!(body["item"]["text"], "hidden thought");

    let back = decode_stream_event(Wire::Responses, &raw, &responses_profile())
        .expect("decode reasoning item")
        .expect("event");
    assert_eq!(back, ev, "reasoning must not rematch as TextDelta");
}

#[test]
fn chat_tool_start_with_args_is_not_one_event() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":"}}]}}]}"#.into(),
    };
    let err = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect_err("start plus arguments is not one event");
    match err {
        MapError::Invalid(detail) => {
            assert!(
                detail.contains("decode_stream_events"),
                "singular limit must name decode_stream_events, detail={detail}"
            );
        }
        other => panic!("expected Invalid, got {other}"),
    }

    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("fan-out start+args");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallStart { id, name, .. } if id == "call_1" && name == "lookup"
        )),
        "fan-out must emit ToolCallStart, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"q":"#
        )),
        "fan-out must emit ArgDelta, got {all:?}"
    );
}

#[test]
fn chat_object_arguments_become_compact_json() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather","arguments":{"city":"Paris","n":1}}}]}}]}"#.into(),
    };
    let err = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect_err("object arguments are not one event");
    match err {
        MapError::Invalid(detail) => {
            assert!(
                detail.contains("decode_stream_events"),
                "singular decode must not look like an argument-free start, detail={detail}"
            );
        }
        other => panic!("expected Invalid, got {other}"),
    }
    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile()).expect("fan-out");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, index: 0 }
                if delta == r#"{"city":"Paris","n":1}"#
        )),
        "object arguments must be compact JSON, got {all:?}"
    );

    let array = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":["a",1]}}]}}]}"#.into(),
    };
    let events = decode_stream_event(Wire::ChatCompletions, &array, &chat_profile())
        .expect("array-only delta")
        .expect("event");
    assert!(
        matches!(
            events,
            IrStreamEvent::ToolCallArgDelta { ref delta, index: 0 } if delta == r#"["a",1]"#
        ),
        "array arguments must be compact JSON, got {events:?}"
    );

    for (label, arguments) in [("number", "1"), ("boolean", "true"), ("null", "null")] {
        let raw = RawSse {
            event: None,
            data: format!(
                r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"call_n","type":"function","function":{{"name":"n","arguments":{arguments}}}}}]}}}}]}}"#
            ),
        };
        let err = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile()).unwrap_err();
        match err {
            MapError::Invalid(detail) => {
                assert!(
                    detail.contains("arguments must be a string, object, or array"),
                    "{label} arguments must be Invalid, detail={detail}"
                );
            }
            other => panic!("{label} arguments: expected Invalid, got {other}"),
        }
    }

    let body = r#"{"choices":[{"message":{"tool_calls":[{"id":"call_a","type":"function","function":{"name":"get_weather","arguments":{"city":"Paris"}}}]},"finish_reason":"tool_calls"}]}"#;
    let events =
        decode_response(Wire::ChatCompletions, body.as_bytes(), &chat_profile()).expect("complete");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"city":"Paris"}"#
        )),
        "complete object arguments must be compact JSON, got {events:?}"
    );

    let responses = r#"{"output":[{"type":"function_call","call_id":"call_r","name":"lookup","arguments":{"q":"x"}}]}"#;
    let events = decode_response(Wire::Responses, responses.as_bytes(), &responses_profile())
        .expect("responses");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"q":"x"}"#
        )),
        "Responses object arguments must be compact JSON, got {events:?}"
    );

    let added = RawSse {
        event: Some("response.output_item.added".into()),
        data: r#"{"output_index":0,"item":{"type":"function_call","call_id":"call_r","name":"lookup","arguments":{"q":"x"}}}"#.into(),
    };
    let err = decode_stream_event(Wire::Responses, &added, &responses_profile())
        .expect_err("Responses object arguments are not one event");
    assert!(
        matches!(err, MapError::Invalid(ref detail) if detail.contains("decode_stream_events")),
        "singular Responses decode must not drop the object, got {err}"
    );
    let events =
        decode_stream_events(Wire::Responses, &added, &responses_profile()).expect("added item");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"q":"x"}"#
        )),
        "Responses stream object arguments must be compact JSON, got {events:?}"
    );
}

#[test]
fn chat_decode_stream_event_refuses_bundled_parallel_arguments() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}},{"index":1,"id":"call_b","type":"function","function":{"name":"get_time","arguments":"{}"}}]}}]}"#.into(),
    };
    let err = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect_err("call_a must not be a start whose arguments were never delivered");
    match err {
        MapError::Invalid(detail) => {
            assert!(
                detail.contains("decode_stream_events"),
                "singular limit must name decode_stream_events, detail={detail}"
            );
            assert!(
                !detail.contains("call_a"),
                "the error must not look like a delivered start, detail={detail}"
            );
        }
        other => panic!("expected Invalid, not an argument-free ToolCallStart, got {other}"),
    }

    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile()).expect("events");
    let ids: Vec<&str> = all
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["call_a", "call_b"], "both calls stay, got {all:?}");
    let deltas: Vec<(&str, u32)> = all
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta, index } => Some((delta.as_str(), *index)),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        [(r#"{"city":"Paris"}"#, 0), ("{}", 1)],
        "both argument strings stay, got {all:?}"
    );

    let more = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":" more"}}]}}]}"#.into(),
    };
    let delta = decode_stream_event(Wire::ChatCompletions, &more, &chat_profile())
        .expect("decode continuation")
        .expect("arg delta");
    assert!(
        matches!(
            delta,
            IrStreamEvent::ToolCallArgDelta { ref delta, index: 0 } if delta == " more"
        ),
        "arguments-only chunk stays ToolCallArgDelta at index 0, got {delta:?}"
    );
}

#[test]
fn chat_parallel_tool_calls_emit_both_starts() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":1}"}},{"index":1,"id":"call_2","type":"function","function":{"name":"search","arguments":"{\"q\":2}"}}]}}]}"#.into(),
    };
    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode parallel tools");
    let starts: Vec<(&str, &str, u32)> = all
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart {
                id, name, index, ..
            } => Some((id.as_str(), name.as_str(), *index)),
            _ => None,
        })
        .collect();
    assert!(
        starts
            .iter()
            .any(|(id, name, index)| *id == "call_1" && *name == "lookup" && *index == 0),
        "first parallel tool_call must survive as ToolCallStart index 0, got {all:?}"
    );
    assert!(
        starts
            .iter()
            .any(|(id, name, index)| *id == "call_2" && *name == "search" && *index == 1),
        "second parallel tool_call must survive as ToolCallStart index 1, got {all:?}"
    );
    assert!(
        !all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::Protocol { .. })),
        "parallel tool_calls must not collapse to Protocol, got {all:?}"
    );
}

#[test]
fn chat_interleaved_tool_indexes_assemble() {
    let frames = [
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_2","type":"function","function":{"name":"search"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":1}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"q\":2}"}}]}}]}"#,
    ];
    let mut asm = ToolCallAssembler::new();
    let mut out = Vec::new();
    for data in frames {
        let raw = RawSse {
            event: None,
            data: data.into(),
        };
        for ev in decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile()).expect("dec") {
            out.extend(asm.push(ev));
        }
    }
    out.extend(asm.flush());
    let starts: Vec<(&str, &str, u32)> = out
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart {
                id, name, index, ..
            } => Some((id.as_str(), name.as_str(), *index)),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![("call_1", "lookup", 0), ("call_2", "search", 1)],
        "interleaved starts must keep distinct indexes, got {out:?}"
    );
    let args: Vec<(&str, u32)> = out
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta, index } => Some((delta.as_str(), *index)),
            _ => None,
        })
        .collect();
    assert_eq!(
        args,
        vec![(r#"{"q":1}"#, 0), (r#"{"q":2}"#, 1)],
        "arg deltas must stay on their index, got {out:?}"
    );
}

#[test]
fn chat_id_then_name_assembles_one_start() {
    let id_only = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":""}}]}}]}"#.into(),
    };
    let name_only = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"type":"function","function":{"name":"lookup"}}]}}]}"#.into(),
    };
    let mut asm = ToolCallAssembler::new();
    let first = decode_stream_events(Wire::ChatCompletions, &id_only, &chat_profile()).expect("id");
    let mut out = Vec::new();
    for ev in first {
        out.extend(asm.push(ev));
    }
    assert!(
        out.is_empty(),
        "id-only start must wait for the name, got {out:?}"
    );
    let second =
        decode_stream_events(Wire::ChatCompletions, &name_only, &chat_profile()).expect("name");
    for ev in second {
        out.extend(asm.push(ev));
    }
    assert_eq!(out.len(), 1, "must merge to one start, got {out:?}");
    assert!(
        matches!(
            &out[0],
            IrStreamEvent::ToolCallStart { id, name, .. } if id == "call_1" && name == "lookup"
        ),
        "merged start, got {out:?}"
    );
}

#[test]
fn chat_stream_custom_tool_call_decodes() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_custom","type":"custom","custom":{"name":"code_exec","input":"print(1)"}}]}}]}"#.into(),
    };
    let events = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode custom tool chunk");
    assert!(
        matches!(
            events.first(),
            Some(IrStreamEvent::CustomToolCallStart { id, name, index })
                if id == "call_custom" && name == "code_exec" && *index == 0
        ),
        "custom start, got {events:?}"
    );
    assert!(
        matches!(
            events.get(1),
            Some(IrStreamEvent::CustomToolCallInputDelta { delta, index: 0 })
                if delta == "print(1)"
        ),
        "custom input delta, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::Protocol { .. })),
        "type=custom must not stay Protocol, got {events:?}"
    );

    let more = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"type":"custom","custom":{"input":"\nprint(2)"}}]}}]}"#.into(),
    };
    let tail = decode_stream_events(Wire::ChatCompletions, &more, &chat_profile())
        .expect("input continuation");
    assert_eq!(
        tail,
        vec![IrStreamEvent::CustomToolCallInputDelta {
            delta: "\nprint(2)".into(),
            index: 0,
        }],
        "later custom.input is another delta, got {tail:?}"
    );

    let bare = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"custom":{"input":"more"}}]}}]}"#
            .into(),
    };
    let bare_events = decode_stream_events(Wire::ChatCompletions, &bare, &chat_profile())
        .expect("custom.input without type");
    assert_eq!(
        bare_events,
        vec![IrStreamEvent::CustomToolCallInputDelta {
            delta: "more".into(),
            index: 1,
        }],
        "custom object without type is still an input delta, got {bare_events:?}"
    );

    let mixed = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"other","other":{"name":"n"}},{"index":1,"id":"call_custom","type":"custom","custom":{"name":"code_exec","input":"print(1)"}}]}}]}"#.into(),
    };
    let mixed_events = decode_stream_events(Wire::ChatCompletions, &mixed, &chat_profile())
        .expect("unknown then custom");
    let custom_starts = mixed_events
        .iter()
        .filter(|ev| {
            matches!(
                ev,
                IrStreamEvent::CustomToolCallStart { id, .. } if id == "call_custom"
            )
        })
        .count();
    assert_eq!(
        custom_starts, 1,
        "custom call after an unknown type must be emitted once, got {mixed_events:?}"
    );
    assert!(
        mixed_events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::Protocol { .. })),
        "leading unknown type must stay, got {mixed_events:?}"
    );

    let unknown = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"other","other":{"name":"n"}}]}}]}"#.into(),
    };
    let unknown_events = decode_stream_events(Wire::ChatCompletions, &unknown, &chat_profile())
        .expect("unknown type");
    assert!(
        matches!(
            unknown_events.as_slice(),
            [IrStreamEvent::Protocol { item_type, .. }] if item_type == "chunk"
        ),
        "unknown tool type stays Protocol, got {unknown_events:?}"
    );

    let bad_input = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_custom","type":"custom","custom":{"name":"code_exec","input":1}}]}}]}"#.into(),
    };
    let err = decode_stream_events(Wire::ChatCompletions, &bad_input, &chat_profile())
        .expect_err("number custom.input");
    assert!(
        matches!(err, MapError::Invalid(ref detail) if detail.contains("input must be a string, object, or array")),
        "number custom.input must be Invalid, got {err}"
    );
}

#[test]
fn chat_non_function_tool_type_is_not_relabeled() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"custom","custom":{"name":"lookup","input":"{}"}}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode custom tool")
        .expect("must not drop tool-bearing frame");
    match ev {
        IrStreamEvent::Protocol { ref payload, .. } => {
            assert_eq!(
                payload["choices"][0]["delta"]["tool_calls"][0]["type"],
                "custom"
            );
        }
        other => panic!("non-function type must stay Protocol, got {other:?}"),
    }
}

#[test]
fn chat_same_delta_content_and_reasoning_fans_out() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"content":"Hello","reasoning":"I should greet"}}]}"#.into(),
    };
    let first = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("1:1")
        .expect("event");
    assert!(
        matches!(first, IrStreamEvent::TextDelta { ref text } if text == "Hello"),
        "1:1 stays first-signal TextDelta, got {first:?}"
    );
    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile()).expect("fan-out");
    assert!(
        all.iter().any(
            |ev| matches!(ev, IrStreamEvent::ReasoningDelta { text } if text == "I should greet")
        ),
        "same-delta reasoning must not be dropped, got {all:?}"
    );
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "Hello")),
        "same-delta content must stay, got {all:?}"
    );
}

#[test]
fn chat_array_delta_content_flattens_to_text() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"content":[{"type":"text","text":"Hi"}]}}]}"#.into(),
    };
    let first = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("1:1")
        .expect("event");
    assert!(
        matches!(first, IrStreamEvent::TextDelta { ref text } if text == "Hi"),
        "1:1 decode must flatten array content, got {first:?}"
    );
    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile()).expect("array");
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "Hi")),
        "array delta.content text parts must become TextDelta, got {all:?}"
    );
}

#[test]
fn chat_same_chunk_finish_and_usage_fans_out() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#.into(),
    };
    let first = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("1:1")
        .expect("event");
    assert!(
        matches!(first, IrStreamEvent::FinishReason { ref reason } if reason == "stop"),
        "1:1 stays FinishReason, got {first:?}"
    );
    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile()).expect("fan-out");
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "finish must stay, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                ..
            }
        )),
        "same-chunk usage must not be dropped, got {all:?}"
    );
}

#[test]
fn chat_tool_delta_same_chunk_finish_and_usage() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"x\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":4,"completion_tokens":6}}"#.into(),
    };
    let all = decode_stream_events(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode tool+finish+usage");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallStart { id, name, .. } if id == "call_1" && name == "lookup"
        )),
        "must emit Start, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::ToolCallArgDelta { delta, .. } if delta == r#"{"q":"x"}"#
        )),
        "must emit ArgDelta, got {all:?}"
    );
    assert!(
        all.iter().any(
            |ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "tool_calls")
        ),
        "same-chunk finish_reason must stay, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Usage {
                prompt_tokens: 4,
                completion_tokens: 6,
                ..
            }
        )),
        "same-chunk usage must stay, got {all:?}"
    );
}

#[test]
fn responses_completed_fans_protocol_finish_and_usage() {
    let raw = RawSse {
        event: Some("response.completed".into()),
        data: r#"{"type":"response.completed","response":{"status":"completed","output":[{"type":"reasoning","encrypted_content":"enc-xyz"}],"usage":{"input_tokens":4,"output_tokens":6}}}"#.into(),
    };
    let first = decode_stream_event(Wire::Responses, &raw, &responses_profile())
        .expect("1:1")
        .expect("event");
    assert!(
        matches!(first, IrStreamEvent::Usage { .. }),
        "1:1 stays Usage, got {first:?}"
    );
    let all = decode_stream_events(Wire::Responses, &raw, &responses_profile()).expect("fan-out");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Protocol { payload, .. }
                if payload.get("encrypted_content").and_then(Value::as_str) == Some("enc-xyz")
        )),
        "encrypted reasoning in output must become Protocol, got {all:?}"
    );
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "completed status must become FinishReason stop, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Usage {
                prompt_tokens: 4,
                completion_tokens: 6,
                ..
            }
        )),
        "completed usage must stay, got {all:?}"
    );
}

#[test]
fn responses_incomplete_fans_finish_usage_and_protocol() {
    let raw = RawSse {
        event: Some("response.incomplete".into()),
        data: r#"{"type":"response.incomplete","response":{"status":"incomplete","output":[{"type":"reasoning","encrypted_content":"enc-cut"}],"usage":{"input_tokens":1,"output_tokens":2}}}"#.into(),
    };
    let all = decode_stream_events(Wire::Responses, &raw, &responses_profile()).expect("fan-out");
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "length")),
        "incomplete status must become FinishReason length, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                ..
            }
        )),
        "incomplete usage must not be dropped, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Protocol { payload, .. }
                if payload.get("encrypted_content").and_then(Value::as_str) == Some("enc-cut")
        )),
        "incomplete encrypted reasoning must become Protocol, got {all:?}"
    );
}

#[test]
fn responses_completed_failed_status_is_not_stop() {
    let raw = RawSse {
        event: Some("response.completed".into()),
        data: r#"{"type":"response.completed","response":{"status":"failed","usage":{"input_tokens":1,"output_tokens":0}}}"#.into(),
    };
    let all = decode_stream_events(Wire::Responses, &raw, &responses_profile()).expect("fan-out");
    assert!(
        !all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "failed status must not map to stop, got {all:?}"
    );
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "failed")),
        "failed status must stay failed, got {all:?}"
    );
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Usage {
                prompt_tokens: 1,
                ..
            }
        )),
        "usage on a failed completed body must stay, got {all:?}"
    );
}

#[test]
fn messages_max_tokens_encodes_chat_length() {
    let raw = RawSse {
        event: Some("message_delta".into()),
        data:
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens","stop_sequence":null}}"#
                .into(),
    };
    let ev = decode_stream_event(Wire::Messages, &raw, &messages_profile())
        .expect("decode Messages")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::FinishReason { ref reason } if reason == "max_tokens"),
        "Messages decode IR stays max_tokens, got {ev:?}"
    );
    let encoded = encode_stream_event(Wire::ChatCompletions, &ev).expect("encode Chat");
    let json: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(
        json.pointer("/choices/0/finish_reason")
            .and_then(Value::as_str),
        Some("length"),
        "Chat encode must remap max_tokens to length, got {json}"
    );
}

#[test]
fn chat_length_encodes_messages_max_tokens() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode Chat")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::FinishReason { ref reason } if reason == "length"),
        "Chat decode IR stays length, got {ev:?}"
    );
    let encoded = encode_stream_event(Wire::Messages, &ev).expect("encode Messages");
    let json: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(
        json.pointer("/delta/stop_reason").and_then(Value::as_str),
        Some("max_tokens"),
        "Messages encode must remap length to max_tokens, got {json}"
    );
}

#[test]
#[allow(non_snake_case)]
fn chat_length_encodes_gemini_MAX_TOKENS() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode Chat")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::FinishReason { ref reason } if reason == "length"),
        "Chat decode IR stays length, got {ev:?}"
    );
    let encoded = encode_stream_event(Wire::Gemini, &ev).expect("encode Gemini");
    let json: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(
        json.pointer("/candidates/0/finishReason")
            .and_then(Value::as_str),
        Some("MAX_TOKENS"),
        "Gemini encode must remap length to MAX_TOKENS, not STOP, got {json}"
    );
}

#[test]
#[allow(non_snake_case)]
fn gemini_thought_part_keeps_thoughtSignature() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"role":"model","parts":[{"thought":true,"text":"think","thoughtSignature":"sig-1"}]},"finishReason":"STOP"}]}"#.into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("fan-out");
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::ReasoningDelta { text } if text == "think")),
        "thought text must stay, got {all:?}"
    );
    assert!(
        all.iter().any(
            |ev| matches!(ev, IrStreamEvent::ReasoningSignature { signature } if signature == "sig-1")
        ),
        "thoughtSignature must not drop beside thought text, got {all:?}"
    );
}

#[test]
fn responses_stop_encodes_completed_status() {
    let raw = encode_stream_event(
        Wire::Responses,
        &IrStreamEvent::FinishReason {
            reason: "stop".into(),
        },
    )
    .expect("encode stop");
    let json: Value = serde_json::from_str(&raw.data).expect("json");
    assert_eq!(
        raw.event.as_deref(),
        Some("response.completed"),
        "IR stop must stay response.completed, got {raw:?}"
    );
    assert_eq!(
        json.pointer("/response/status").and_then(Value::as_str),
        Some("completed"),
        "IR stop must encode status completed, not stop, got {json}"
    );
    assert_ne!(
        json.pointer("/response/status").and_then(Value::as_str),
        Some("stop"),
        "status must not leak the IR string stop, got {json}"
    );
}

#[test]
fn responses_length_encodes_incomplete() {
    let raw = encode_stream_event(
        Wire::Responses,
        &IrStreamEvent::FinishReason {
            reason: "length".into(),
        },
    )
    .expect("encode length");
    let json: Value = serde_json::from_str(&raw.data).expect("json");
    assert_eq!(
        raw.event.as_deref(),
        Some("response.incomplete"),
        "IR length must be response.incomplete, got {raw:?}"
    );
    assert_eq!(
        json.pointer("/response/status").and_then(Value::as_str),
        Some("incomplete"),
        "IR length must encode status incomplete, got {json}"
    );
    assert_eq!(
        json.pointer("/response/incomplete_details/reason")
            .and_then(Value::as_str),
        Some("max_output_tokens"),
        "IR length must encode incomplete_details.reason=max_output_tokens, got {json}"
    );
}

#[test]
fn responses_max_tokens_encodes_incomplete() {
    let raw = encode_stream_event(
        Wire::Responses,
        &IrStreamEvent::FinishReason {
            reason: "max_tokens".into(),
        },
    )
    .expect("encode max_tokens");
    let json: Value = serde_json::from_str(&raw.data).expect("json");
    assert_eq!(
        raw.event.as_deref(),
        Some("response.incomplete"),
        "IR max_tokens must be response.incomplete like length, got {raw:?}"
    );
    assert_eq!(
        json.pointer("/response/status").and_then(Value::as_str),
        Some("incomplete"),
        "IR max_tokens must encode status incomplete, got {json}"
    );
    assert_eq!(
        json.pointer("/response/incomplete_details/reason")
            .and_then(Value::as_str),
        Some("max_output_tokens"),
        "IR max_tokens must encode incomplete_details.reason=max_output_tokens, got {json}"
    );
}

#[test]
fn responses_incomplete_round_trips_status() {
    let raw = RawSse {
        event: Some("response.incomplete".into()),
        data: r#"{"type":"response.incomplete","response":{"status":"incomplete"}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Responses, &raw, &responses_profile())
        .expect("decode incomplete")
        .expect("event");
    let encoded = encode_stream_event(Wire::Responses, &ev).expect("encode Responses");
    let json: Value = serde_json::from_str(&encoded.data).expect("json");
    assert_eq!(
        json.pointer("/response/status").and_then(Value::as_str),
        Some("incomplete"),
        "decode then encode must keep status incomplete, got event={ev:?} json={json}"
    );
}

#[test]
fn dest_responses_stream_incomplete_content_filter_stays_ir() {
    let raw = RawSse {
        event: Some("response.incomplete".into()),
        data: r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"content_filter"}}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Responses, &raw, &responses_profile())
        .expect("decode dest Responses incomplete")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::FinishReason { ref reason } if reason == "content_filter"),
        "dest Responses incomplete_details.reason=content_filter must stay IR content_filter, got {ev:?}"
    );
    let all = decode_stream_events(Wire::Responses, &raw, &responses_profile()).expect("events");
    assert!(
        all.iter().any(
            |ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "content_filter")
        ),
        "dest Responses stream decode must keep IR content_filter, got {all:?}"
    );
}

#[test]
fn dest_responses_usage_lifts_cache_write_tokens() {
    let raw = RawSse {
        event: Some("response.completed".into()),
        data: r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":105,"output_tokens":15,"input_tokens_details":{"cached_tokens":25,"cache_write_tokens":9},"output_tokens_details":{"reasoning_tokens":3}}}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Responses, &raw, &responses_profile())
        .expect("decode dest Responses usage")
        .expect("event");
    match ev {
        IrStreamEvent::Usage {
            cache_write_tokens, ..
        } => {
            assert_eq!(
                cache_write_tokens, 9,
                "dest Responses input_tokens_details.cache_write_tokens must lift, got {ev:?}"
            );
        }
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn converse_eventstream_bytes_decode_to_text_delta() {
    let converse_profile =
        parse_profile_str("schema_version = 1\nid = \"amazon-bedrock\"\nwire = \"converse\"\n")
            .expect("converse profile");
    let payload = br#"{"contentBlockDelta":{"delta":{"text":"pong"}}}"#;
    let bytes = wiremux::stream::encode_eventstream_message("contentBlockDelta", payload);
    let mut reader = wiremux::stream::EventStreamReader::new();
    let (frames, err) = reader.feed(&bytes).expect("feed");
    assert!(err.is_none());
    assert_eq!(frames.len(), 1);
    let ev = decode_stream_event(Wire::Converse, &frames[0], &converse_profile)
        .expect("decode")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::TextDelta { ref text } if text == "pong"),
        "{ev:?}"
    );
}

#[test]
fn converse_eventstream_unwrapped_payload_decodes_to_text_delta() {
    let converse_profile =
        parse_profile_str("schema_version = 1\nid = \"amazon-bedrock\"\nwire = \"converse\"\n")
            .expect("converse profile");
    let payload = br#"{"delta":{"text":"hi"},"contentBlockIndex":0}"#;
    let bytes = wiremux::stream::encode_eventstream_message("contentBlockDelta", payload);
    let mut reader = wiremux::stream::EventStreamReader::new();
    let (frames, err) = reader.feed(&bytes).expect("feed");
    assert!(err.is_none());
    assert_eq!(frames.len(), 1);
    let ev = decode_stream_event(Wire::Converse, &frames[0], &converse_profile)
        .expect("decode")
        .expect("event");
    assert!(
        matches!(ev, IrStreamEvent::TextDelta { ref text } if text == "hi"),
        "{ev:?}"
    );
}

#[test]
fn gemini_stop_with_function_call_is_tool_calls() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"weather","args":{"city":"Paris"}}}]},"finishReason":"STOP"}]}"#.into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("events");
    assert!(
        all.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::FinishReason { reason } if reason == "tool_calls"
        )),
        "STOP + functionCall must be tool_calls, got {all:?}"
    );
}

#[test]
fn gemini_stop_text_only_is_stop() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#
            .into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("events");
    assert!(
        all.iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "text-only STOP must stay stop, got {all:?}"
    );
}

#[test]
fn gemini_parallel_same_name_calls_get_distinct_ids() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"fc_1","name":"weather","args":{"city":"Paris"}}},{"functionCall":{"id":"fc_2","name":"weather","args":{"city":"Rome"}}}]}}]}"#.into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("events");
    let ids: Vec<&str> = all
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["fc_1", "fc_2"], "{all:?}");
}

#[test]
fn gemini_generated_ids_are_distinct_when_vendor_omits_id() {
    let raw = RawSse {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"weather","args":{"city":"Paris"}}},{"functionCall":{"name":"weather","args":{"city":"Rome"}}}]}}]}"#.into(),
    };
    let all = decode_stream_events(Wire::Gemini, &raw, &gemini_profile()).expect("events");
    let ids: Vec<&str> = all
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallStart { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2, "{all:?}");
    assert_ne!(ids[0], ids[1], "{ids:?}");
}

fn encode_all(wire: Wire, events: &[IrStreamEvent]) -> Vec<RawSse> {
    let mut enc = StreamEncoder::new(wire);
    let mut out = Vec::new();
    for ev in events {
        out.extend(enc.push(ev.clone()).expect("push"));
    }
    out.extend(enc.finish().expect("finish"));
    out
}

fn frame_events(frames: &[RawSse]) -> Vec<String> {
    frames
        .iter()
        .map(|f| {
            f.event.clone().unwrap_or_else(|| {
                serde_json::from_str::<Value>(&f.data)
                    .ok()
                    .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_else(|| "chunk".into())
            })
        })
        .collect()
}

#[test]
fn stream_encoder_chat_to_messages_emits_grammar() {
    let frames = encode_all(
        Wire::Messages,
        &[
            IrStreamEvent::TextDelta { text: "Hi".into() },
            IrStreamEvent::ToolCallStart {
                id: "call_1".into(),
                name: "lookup".into(),
                thought_signature: None,
                index: 0,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: r#"{"q":"x"}"#.into(),
                index: 0,
            },
            IrStreamEvent::FinishReason {
                reason: "tool_calls".into(),
            },
            IrStreamEvent::Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                audio_tokens: 0,
                completion_audio_tokens: 0,
            },
            IrStreamEvent::Done,
        ],
    );
    let names = frame_events(&frames);
    assert_eq!(
        names.first().map(String::as_str),
        Some("message_start"),
        "Messages must open with message_start, got {names:?}"
    );
    let starts: Vec<u64> = frames
        .iter()
        .filter(|f| f.event.as_deref() == Some("content_block_start"))
        .filter_map(|f| {
            serde_json::from_str::<Value>(&f.data)
                .ok()?
                .get("index")
                .and_then(Value::as_u64)
        })
        .collect();
    assert!(
        starts.contains(&0) && starts.contains(&1),
        "text and tool_use must use distinct indexes, got {starts:?} from {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|f| f.event.as_deref() == Some("content_block_stop")),
        "must close blocks with content_block_stop, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "message_delta"),
        "must emit message_delta, got {names:?}"
    );
    assert_eq!(
        names.last().map(String::as_str),
        Some("message_stop"),
        "must end with message_stop, got {names:?}"
    );
}

#[test]
fn stream_encoder_chat_to_responses_one_created_one_completed() {
    let frames = encode_all(
        Wire::Responses,
        &[
            IrStreamEvent::TextDelta { text: "Hi".into() },
            IrStreamEvent::ToolCallStart {
                id: "call_1".into(),
                name: "lookup".into(),
                thought_signature: None,
                index: 0,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: r#"{"q":"x"}"#.into(),
                index: 0,
            },
            IrStreamEvent::FinishReason {
                reason: "tool_calls".into(),
            },
            IrStreamEvent::Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                audio_tokens: 0,
                completion_audio_tokens: 0,
            },
            IrStreamEvent::Done,
        ],
    );
    let names = frame_events(&frames);
    let created = names.iter().filter(|n| *n == "response.created").count();
    let completed = names.iter().filter(|n| *n == "response.completed").count();
    assert_eq!(created, 1, "exactly one response.created, got {names:?}");
    assert_eq!(
        completed, 1,
        "exactly one response.completed, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "response.output_item.added"),
        "must add output items, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "response.output_item.done"),
        "must close output items, got {names:?}"
    );
    let completed_frame = frames
        .iter()
        .find(|f| f.event.as_deref() == Some("response.completed"))
        .expect("completed");
    let json: Value = serde_json::from_str(&completed_frame.data).expect("json");
    assert!(
        json.pointer("/response/usage").is_some(),
        "completed must carry usage, got {json}"
    );
}

#[test]
fn stream_encoder_parallel_tools_to_chat_use_distinct_indexes() {
    let frames = encode_all(
        Wire::ChatCompletions,
        &[
            IrStreamEvent::ToolCallStart {
                id: "c1".into(),
                name: "weather".into(),
                thought_signature: None,
                index: 0,
            },
            IrStreamEvent::ToolCallStart {
                id: "c2".into(),
                name: "weather".into(),
                thought_signature: None,
                index: 0,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: r#"{"city":"Paris"}"#.into(),
                index: 0,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: r#"{"city":"Rome"}"#.into(),
                index: 0,
            },
        ],
    );
    let indexes: Vec<u64> = frames
        .iter()
        .filter_map(|f| {
            let v: Value = serde_json::from_str(&f.data).ok()?;
            v.pointer("/choices/0/delta/tool_calls/0/index")
                .and_then(Value::as_u64)
        })
        .collect();
    assert!(
        indexes.contains(&0) && indexes.contains(&1),
        "parallel Chat tool calls must use distinct indexes, got {indexes:?} from {frames:?}"
    );
}

#[test]
fn converse_stream_tool_uses_content_block_index() {
    let start = RawSse {
        event: None,
        data: r#"{"contentBlockStart":{"contentBlockIndex":1,"start":{"toolUse":{"toolUseId":"t1","name":"lookup"}}}}"#.into(),
    };
    let delta = RawSse {
        event: None,
        data: r#"{"contentBlockDelta":{"contentBlockIndex":1,"delta":{"toolUse":{"input":"{\"q\":1}"}}}}"#.into(),
    };
    let start_ev = decode_stream_event(Wire::Converse, &start, &converse_profile())
        .expect("decode contentBlockStart")
        .expect("tool start");
    assert!(
        matches!(start_ev, IrStreamEvent::ToolCallStart { index: 1, .. }),
        "contentBlockStart contentBlockIndex 1 must be ToolCallStart index 1, got {start_ev:?}"
    );
    let delta_ev = decode_stream_event(Wire::Converse, &delta, &converse_profile())
        .expect("decode contentBlockDelta")
        .expect("tool delta");
    assert!(
        matches!(delta_ev, IrStreamEvent::ToolCallArgDelta { index: 1, .. }),
        "contentBlockDelta contentBlockIndex 1 must be ToolCallArgDelta index 1, got {delta_ev:?}"
    );
}

#[test]
fn converse_stream_missing_content_block_index_is_zero() {
    let start = RawSse {
        event: None,
        data: r#"{"contentBlockStart":{"start":{"toolUse":{"toolUseId":"t","name":"f"}}}}"#.into(),
    };
    let ev = decode_stream_event(Wire::Converse, &start, &converse_profile())
        .expect("decode")
        .expect("tool start");
    assert!(
        matches!(ev, IrStreamEvent::ToolCallStart { index: 0, .. }),
        "missing contentBlockIndex stays 0, got {ev:?}"
    );
}

#[test]
fn converse_stream_content_block_index_above_cap_is_error() {
    let start = RawSse {
        event: None,
        data: r#"{"contentBlockStart":{"contentBlockIndex":129,"start":{"toolUse":{"toolUseId":"t","name":"f"}}}}"#.into(),
    };
    let err = decode_stream_event(Wire::Converse, &start, &converse_profile())
        .expect_err("contentBlockIndex 129 must fail");
    assert!(
        matches!(err, MapError::Invalid(ref msg) if msg.contains("129") && msg.contains("128")),
        "hostile contentBlockIndex must be an error, not index 0, got {err}"
    );
}

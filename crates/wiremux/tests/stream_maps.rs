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
        matches!(ev, IrStreamEvent::Protocol { ref item_type, .. } if item_type == "chunk"),
        "1:1 nonempty args stay Protocol, got {ev:?}"
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
fn chat_tool_start_with_args_keeps_bytes() {
    let raw = RawSse {
        event: None,
        data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":"}}]}}]}"#.into(),
    };
    let ev = decode_stream_event(Wire::ChatCompletions, &raw, &chat_profile())
        .expect("decode start+args")
        .expect("must not drop tool-bearing frame");
    match ev {
        IrStreamEvent::Protocol {
            ref item_type,
            ref payload,
        } => {
            assert_eq!(item_type, "chunk");
            assert_eq!(
                payload["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
                r#"{"q":"#
            );
        }
        other => panic!("expected Protocol keeping arg bytes, got {other:?}"),
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

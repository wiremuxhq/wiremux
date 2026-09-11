//! Stream-map goldens. Fixtures are redacted SSE, not live captures.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use wiremux::{
    IrStreamEvent, MapError, RawSse, ResolvedProfile, Wire, decode_stream_event,
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
    RawSse::parse_all(text)
        .into_iter()
        .filter_map(|raw| decode_stream_event(wire, &raw, profile).transpose())
        .collect()
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
        IrStreamEvent::ToolCallStart { id, name } => Some((id.as_str(), name.as_str())),
        _ => None,
    });
    assert_eq!(start, Some(("toolu_redacted", "get_weather")));

    let args: String = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(args, r#"{"location":"San Francisco"}"#);

    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "tool_use")),
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
        IrStreamEvent::ToolCallStart { id, name } => Some((id.as_str(), name.as_str())),
        _ => None,
    });
    assert_eq!(start, Some(("call_redacted", "get_weather")));

    let args: String = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta } => Some(delta.as_str()),
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
    // 100-40 cache, 20-7 reasoning. Matches Bline normalize_oai_usage.
    assert_eq!(usage_tuple(&chat), (60, 13, 40, 0, 7));

    let messages = decode_all(
        Wire::Messages,
        &golden("anthropic_usage_with_cache.sse"),
        &messages_profile(),
    )
    .expect("decode messages cache usage");
    // Anthropic input_tokens is already exclusive of cache. output includes
    // thinking: 12-3. Matches Bline anthropic conversions.
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
        resp_json.pointer("/response/usage/output_tokens_details/reasoning_tokens"),
        Some(&Value::from(3))
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
            |ev| matches!(ev, IrStreamEvent::ToolCallStart { id, name } if id == "call_redacted" && name == "get_weather")
        ),
        "function_call start missing: {events:?}"
    );
    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::ToolCallArgDelta { delta } if delta == r#"{"location":"SF"}"#)
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
    };
    for wire in [Wire::ChatCompletions, Wire::Messages, Wire::Responses] {
        let profile = match wire {
            Wire::ChatCompletions => chat_profile(),
            Wire::Messages => messages_profile(),
            Wire::Responses => responses_profile(),
            Wire::Gemini => gemini_profile(),
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
        matches!(ev, IrStreamEvent::FinishReason { ref reason } if reason == "tool_use"),
        "stop_reason must not be dropped for usage, got {ev:?}"
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

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
    for wire in [Wire::ChatCompletions, Wire::Messages, Wire::Responses] {
        let profile = match wire {
            Wire::ChatCompletions => chat_profile(),
            Wire::Messages => messages_profile(),
            Wire::Responses => responses_profile(),
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
        };
        let raw = encode_stream_event(wire, &start).expect("encode start");
        let back = decode_stream_event(wire, &raw, &profile)
            .expect("decode start")
            .expect("start event");
        assert_eq!(back, start, "tool start round-trip {wire:?}");
    }
}

//! Whole-stream grammar checks for the proxy remap pipeline.
//!
//! Each committed golden (redacted, not live) is decoded in its vendor
//! dialect, assembled, then encoded for every client dialect.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use wiremux::{
    EventStreamReader, IrStreamEvent, RawSse, ResolvedProfile, StreamEncoder, ToolCallAssembler,
    Wire, decode_stream_events, encode_eventstream_message, parse_profile_str,
};

fn golden(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens/streams")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn profile(wire: &str) -> ResolvedProfile {
    parse_profile_str(&format!(
        "schema_version = 1\nid = \"grammar-{wire}\"\nwire = \"{wire}\"\n"
    ))
    .expect("test profile parses")
}

fn profile_for(wire: Wire) -> ResolvedProfile {
    match wire {
        Wire::ChatCompletions => profile("chat-completions"),
        Wire::Messages => profile("messages"),
        Wire::Responses => profile("responses"),
        Wire::Gemini => profile("gemini"),
        Wire::Converse => profile("converse"),
        other => panic!("no grammar fixture for {other:?}"),
    }
}

fn has_slot(client: Wire, ev: &IrStreamEvent) -> bool {
    match ev {
        IrStreamEvent::Protocol { .. } => false,
        IrStreamEvent::Unknown { .. } => matches!(client, Wire::Messages | Wire::Responses),
        _ => true,
    }
}

fn remap(upstream: Wire, client: Wire, frames: &[RawSse]) -> Vec<RawSse> {
    let profile = profile_for(upstream);
    let mut assembler = ToolCallAssembler::new();
    let mut encoder = StreamEncoder::new(client);
    let mut out = Vec::new();
    for raw in frames {
        let events = decode_stream_events(upstream, raw, &profile).unwrap_or_default();
        for ev in events.into_iter().flat_map(|ev| assembler.push(ev)) {
            if !has_slot(client, &ev) {
                continue;
            }
            out.extend(encoder.push(ev).expect("encode"));
        }
    }
    for ev in assembler.flush() {
        if !has_slot(client, &ev) {
            continue;
        }
        out.extend(encoder.push(ev).expect("encode flush"));
    }
    out.extend(encoder.finish().expect("finish"));
    out
}

fn frame_names(frames: &[RawSse]) -> Vec<String> {
    frames
        .iter()
        .map(|f| {
            f.event.clone().unwrap_or_else(|| {
                serde_json::from_str::<serde_json::Value>(&f.data)
                    .ok()
                    .and_then(|v| v.get("object").and_then(|o| o.as_str()).map(str::to_string))
                    .unwrap_or_else(|| "chunk".into())
            })
        })
        .collect()
}

fn check_messages(frames: &[RawSse]) {
    let names = frame_names(frames);
    assert_eq!(
        names.first().map(String::as_str),
        Some("message_start"),
        "Messages must open with message_start, got {names:?}"
    );
    let starts = names
        .iter()
        .filter(|n| n.as_str() == "content_block_start")
        .count();
    let stops = names
        .iter()
        .filter(|n| n.as_str() == "content_block_stop")
        .count();
    assert_eq!(starts, stops, "unbalanced content blocks: {names:?}");
    assert_eq!(
        names
            .iter()
            .filter(|n| n.as_str() == "message_stop")
            .count(),
        1,
        "exactly one message_stop, got {names:?}"
    );
    let mut open: HashSet<u32> = HashSet::new();
    for frame in frames {
        let Some(name) = frame.event.as_deref() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&frame.data) else {
            continue;
        };
        let idx = v.get("index").and_then(|i| i.as_u64()).map(|i| i as u32);
        match name {
            "content_block_start" => {
                let idx = idx.expect("start index");
                assert!(open.insert(idx), "reused content_block index {idx}");
            }
            "content_block_delta" => {
                let idx = idx.expect("delta index");
                assert!(open.contains(&idx), "delta before start at {idx}");
            }
            "content_block_stop" => {
                let idx = idx.expect("stop index");
                assert!(open.remove(&idx), "stop without start at {idx}");
            }
            _ => {}
        }
    }
}

fn check_responses(frames: &[RawSse]) {
    let names = frame_names(frames);
    assert_eq!(
        names
            .iter()
            .filter(|n| n.as_str() == "response.created")
            .count(),
        1,
        "exactly one response.created, got {names:?}"
    );
    assert_eq!(
        names
            .iter()
            .filter(|n| n.as_str() == "response.completed")
            .count(),
        1,
        "exactly one response.completed, got {names:?}"
    );
}

fn check_chat(frames: &[RawSse]) {
    let mut indexes = HashSet::new();
    for frame in frames {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&frame.data) else {
            continue;
        };
        let Some(calls) = v
            .pointer("/choices/0/delta/tool_calls")
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for call in calls {
            if let Some(idx) = call.get("index").and_then(|i| i.as_u64()) {
                indexes.insert(idx);
            }
        }
    }
    if indexes.len() > 1 {
        assert!(
            indexes.contains(&0) && indexes.contains(&1),
            "parallel Chat tools must use distinct indexes, got {indexes:?}"
        );
    }
}

fn check_gemini(frames: &[RawSse]) {
    for frame in frames {
        if frame.data.trim() == "[DONE]" {
            continue;
        }
        serde_json::from_str::<serde_json::Value>(&frame.data)
            .unwrap_or_else(|err| panic!("Gemini frame must be JSON: {err} {}", frame.data));
    }
}

fn check_grammar(client: Wire, frames: &[RawSse]) {
    match client {
        Wire::Messages => check_messages(frames),
        Wire::Responses => check_responses(frames),
        Wire::ChatCompletions => check_chat(frames),
        Wire::Gemini => check_gemini(frames),
        Wire::Converse | _ => {}
    }
}

const GOLDENS: &[(&str, Wire)] = &[
    ("chat_tool_call_deltas.sse", Wire::ChatCompletions),
    ("chat_usage_no_cache.sse", Wire::ChatCompletions),
    ("chat_usage_with_cache.sse", Wire::ChatCompletions),
    ("anthropic_tool_use_thinking.sse", Wire::Messages),
    ("anthropic_usage_with_cache.sse", Wire::Messages),
    ("responses_usage_with_cache.sse", Wire::Responses),
    ("gemini_text_and_tool.sse", Wire::Gemini),
];

const CLIENTS: &[Wire] = &[
    Wire::ChatCompletions,
    Wire::Messages,
    Wire::Responses,
    Wire::Gemini,
];

#[test]
fn grammar_matrix_upstream_goldens_to_each_client() {
    for (name, upstream) in GOLDENS {
        let frames = RawSse::parse_all(&golden(name));
        assert!(!frames.is_empty(), "{name} parsed no frames");
        for client in CLIENTS {
            let mapped = remap(*upstream, *client, &frames);
            check_grammar(*client, &mapped);
        }
    }
}

#[test]
fn redacted_capture_exists_per_vendor_dialect() {
    for (name, _) in GOLDENS {
        let raw = golden(name);
        assert!(!raw.to_ascii_lowercase().contains("sk-"), "{name}");
        assert!(!raw.contains("sk-ant-"), "{name}");
    }
    let converse = encode_eventstream_message(
        "contentBlockDelta",
        br#"{"delta":{"text":"Hi"},"contentBlockIndex":0}"#,
    );
    let mut reader = EventStreamReader::new();
    let (frames, err) = reader.feed(&converse).expect("eventstream");
    assert!(err.is_none(), "{err:?}");
    assert_eq!(frames.len(), 1, "{frames:?}");
    let mapped = remap(Wire::Converse, Wire::Messages, &frames);
    check_messages(&mapped);
}

#[test]
fn messages_grammar_has_start_blocks_and_one_stop() {
    let frames = remap(
        Wire::ChatCompletions,
        Wire::Messages,
        &RawSse::parse_all(&golden("chat_tool_call_deltas.sse")),
    );
    check_messages(&frames);
}

#[test]
fn responses_grammar_one_created_one_completed() {
    let frames = remap(
        Wire::Responses,
        Wire::Responses,
        &RawSse::parse_all(&golden("responses_usage_with_cache.sse")),
    );
    check_responses(&frames);
}

#[test]
fn chat_grammar_distinct_tool_indexes() {
    let frames = remap(
        Wire::ChatCompletions,
        Wire::ChatCompletions,
        &RawSse::parse_all(&golden("chat_tool_call_deltas.sse")),
    );
    check_chat(&frames);
}

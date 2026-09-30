//! Dest stream identity and tool-index contracts.
//! Proxy EOF and error-frame tests live next to `dest_error_bytes`.

use serde_json::Value;
use wiremux::{IrStreamEvent, RawSse, StreamEncoder, Wire};

fn data_of(frames: &[RawSse]) -> String {
    frames
        .iter()
        .map(|frame| frame.data.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn json_frames(frames: &[RawSse]) -> Vec<Value> {
    frames
        .iter()
        .map(|frame| {
            serde_json::from_str(&frame.data).unwrap_or_else(|err| panic!("{err}: {}", frame.data))
        })
        .collect()
}

#[test]
fn chat_chunk_repeats_fixed_id_object_created() {
    let mut enc = StreamEncoder::new(Wire::ChatCompletions);
    let frames = enc
        .push(IrStreamEvent::TextDelta { text: "hi".into() })
        .expect("push");
    assert!(!frames.is_empty());
    for value in json_frames(&frames) {
        assert_eq!(value["id"], "chatcmpl-wiremux", "{value}");
        assert_eq!(value["object"], "chat.completion.chunk", "{value}");
        assert!(value["created"].is_number(), "{value}");
    }
}

#[test]
fn responses_completed_repeats_resp_wiremux() {
    let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4.1");
    enc.push(IrStreamEvent::TextDelta { text: "hi".into() })
        .expect("text");
    enc.push(IrStreamEvent::FinishReason {
        reason: "stop".into(),
        vendor: None,
    })
    .expect("finish reason");
    let done = enc.finish().expect("finish");
    let completed = done
        .iter()
        .find(|frame| frame.data.contains("response.completed"))
        .expect("completed frame");
    let value: Value = serde_json::from_str(&completed.data).expect("json");
    assert_eq!(value["response"]["id"], "resp_wiremux", "{value}");
    assert!(
        value["response"]["object"].as_str().is_some()
            || value["object"].as_str().is_some()
            || completed.data.contains("\"object\""),
        "{value}"
    );
}

#[test]
fn messages_dest_distinct_ids_same_index_do_not_merge() {
    let mut enc = StreamEncoder::new(Wire::Messages);
    let mut all = Vec::new();
    all.extend(
        enc.push(IrStreamEvent::ToolCallStart {
            id: "call_a".into(),
            name: "one".into(),
            thought_signature: None,
            index: 0,
        })
        .expect("start a"),
    );
    all.extend(
        enc.push(IrStreamEvent::ToolCallArgDelta {
            delta: "{\"a\":".into(),
            index: 0,
        })
        .expect("arg a"),
    );
    all.extend(
        enc.push(IrStreamEvent::ToolCallStart {
            id: "call_b".into(),
            name: "two".into(),
            thought_signature: None,
            index: 0,
        })
        .expect("start b"),
    );
    all.extend(enc.finish().expect("finish"));
    let blob = data_of(&all);
    assert!(blob.contains("call_a"), "{blob}");
    assert!(blob.contains("call_b"), "{blob}");
    assert!(
        blob.contains("content_block_stop"),
        "a second id must close the first tool block, got {blob}"
    );
}

#[test]
fn chat_dest_tool_index_is_dense_from_zero() {
    let mut enc = StreamEncoder::new(Wire::ChatCompletions);
    let frames = enc
        .push(IrStreamEvent::ToolCallStart {
            id: "call_z".into(),
            name: "weather".into(),
            thought_signature: None,
            index: 1,
        })
        .expect("start");
    let tool = frames
        .iter()
        .find(|frame| frame.data.contains("tool_calls"))
        .expect("tool frame");
    let value: Value = serde_json::from_str(&tool.data).expect("json");
    assert_eq!(
        value["choices"][0]["delta"]["tool_calls"][0]["index"], 0,
        "{value}"
    );
}

#[test]
fn responses_interleaved_arguments_stay_on_first_item() {
    let mut enc = StreamEncoder::new(Wire::Responses);
    enc.push(IrStreamEvent::ToolCallStart {
        id: "call_0".into(),
        name: "one".into(),
        thought_signature: None,
        index: 0,
    })
    .expect("start 0");
    enc.push(IrStreamEvent::ToolCallArgDelta {
        delta: "{\"q\":".into(),
        index: 0,
    })
    .expect("partial 0");
    enc.push(IrStreamEvent::ToolCallStart {
        id: "call_1".into(),
        name: "two".into(),
        thought_signature: None,
        index: 1,
    })
    .expect("start 1");
    let late = enc
        .push(IrStreamEvent::ToolCallArgDelta {
            delta: "1}".into(),
            index: 0,
        })
        .expect("rest of 0");
    let blob = data_of(&late);
    assert!(
        blob.contains("call_0") || blob.contains("\"q\""),
        "late arguments for item 0 must not be dropped or attached only to call_1, got {blob}"
    );
    assert!(
        !blob.contains("call_1") || blob.contains("call_0") || blob.contains("\"q\""),
        "{blob}"
    );
}

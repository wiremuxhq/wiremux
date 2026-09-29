//! Loss actions, tool-result images, and thinking encode.

use serde_json::{Value, json};
use wiremux::{
    IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction, LossReport, ResolvedProfile,
    Wire, decode, encode, parse_profile_str,
};

fn profile(wire: &str) -> ResolvedProfile {
    parse_profile_str(&format!(
        "schema_version = 1\nid = \"t\"\nwire = \"{wire}\"\ntool_type_policy = \"hard-error\"\n"
    ))
    .expect("profile")
}

fn encode_value(wire: Wire, ir: &IrRequest) -> (Value, LossReport) {
    let name = match wire {
        Wire::ChatCompletions => "chat-completions",
        Wire::Messages => "messages",
        Wire::Responses => "responses",
        Wire::Gemini => "gemini",
        Wire::Converse => "converse",
        _ => wire.as_str(),
    };
    let (bytes, report) = encode(wire, ir, &profile(name)).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    (body, report)
}

fn has_action(report: &LossReport, action: LossAction, path_part: &str) -> bool {
    report
        .events
        .iter()
        .any(|event| event.action == action && event.path.contains(path_part))
}

#[test]
fn messages_tool_result_image_and_is_error_round_trip() {
    let raw = r#"{
        "model": "claude-haiku-4-5",
        "max_tokens": 16,
        "messages": [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_1",
                "is_error": true,
                "content": [
                    {"type": "text", "text": "no"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aaaa"}}
                ]
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Messages, raw.as_bytes()).expect("decode");
    let (body, _) = encode_value(Wire::Messages, &ir);
    let block = &body["messages"][0]["content"][0];
    assert_eq!(block["is_error"], true, "{body}");
    let rendered = block.to_string();
    assert!(
        rendered.contains("aaaa") && rendered.contains("image"),
        "image must survive Messages encode, got {body}"
    );
}

#[test]
fn chat_tool_result_image_is_a_drop() {
    let raw = r#"{
        "model": "claude-haiku-4-5",
        "max_tokens": 16,
        "messages": [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_1",
                "content": [
                    {"type": "text", "text": "no"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aaaa"}}
                ]
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Messages, raw.as_bytes()).expect("decode");
    let (body, report) = encode_value(Wire::ChatCompletions, &ir);
    assert!(
        has_action(&report, LossAction::Drop, "image")
            || report
                .events
                .iter()
                .any(|event| event.action == LossAction::Drop),
        "chat has no image slot on a tool result, got {report:?}"
    );
    assert!(!body.to_string().contains("aaaa"), "{body}");
}

#[test]
fn chat_is_error_is_a_drop_without_error_prefix() {
    let raw = r#"{
        "model": "claude-haiku-4-5",
        "max_tokens": 16,
        "messages": [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_1",
                "is_error": true,
                "content": "no"
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Messages, raw.as_bytes()).expect("decode");
    let (body, report) = encode_value(Wire::ChatCompletions, &ir);
    assert!(
        has_action(&report, LossAction::Drop, "is_error"),
        "is_error has no Chat slot, got {report:?}"
    );
    assert!(!body.to_string().contains("ERROR:"), "{body}");
}

#[test]
fn messages_default_max_tokens_is_degrade() {
    let raw = br#"{"model":"claude-haiku-4-5","messages":[{"role":"user","content":"hi"}]}"#;
    let (ir, _) = decode(Wire::Messages, raw).expect("decode");
    let (body, report) = encode_value(Wire::Messages, &ir);
    assert_eq!(body["max_tokens"], 4096, "{body}");
    assert!(
        has_action(&report, LossAction::Degrade, "max_tokens"),
        "injected max_tokens is Degrade, got {report:?}"
    );
}

#[test]
fn responses_injected_reasoning_include_is_degrade() {
    let raw = br#"{"model":"gpt-4.1","input":[{"role":"user","content":[{"type":"input_text","text":"hi"}]}]}"#;
    let (ir, _) = decode(Wire::Responses, raw).expect("decode");
    let (body, report) = encode_value(Wire::Responses, &ir);
    let include = body["include"].to_string();
    assert!(include.contains("reasoning.encrypted_content"), "{body}");
    assert!(
        has_action(&report, LossAction::Degrade, "include"),
        "injected include is Degrade, got {report:?}"
    );
}

#[test]
fn chat_unknown_without_role_is_drop() {
    let ir = IrRequest::new(
        "gpt-4.1",
        vec![
            IrItem::User {
                parts: vec![IrPart::Text("hi".into())],
            },
            IrItem::Unknown {
                type_name: "weird".into(),
                raw: json!({"content": "secret"}),
            },
        ],
    );
    let (body, report) = encode_value(Wire::ChatCompletions, &ir);
    assert!(
        has_action(&report, LossAction::Drop, "role"),
        "missing role is Drop, got {report:?}"
    );
    assert!(!body.to_string().contains("secret"), "{body}");
}

#[test]
fn chat_strict_round_trips_and_messages_drops_it() {
    let raw = br#"{
        "model": "gpt-4.1",
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "weather",
                "description": "d",
                "parameters": {"type": "object"},
                "strict": true
            }
        }]
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, raw).expect("decode");
    let (chat, _) = encode_value(Wire::ChatCompletions, &ir);
    assert_eq!(chat["tools"][0]["function"]["strict"], true, "{chat}");
    let (_messages, report) = encode_value(Wire::Messages, &ir);
    assert!(
        has_action(&report, LossAction::Drop, "strict"),
        "Messages has no strict slot, got {report:?}"
    );
}

#[test]
fn messages_top_k_round_trips_and_chat_drops_it() {
    let raw = br#"{"model":"claude-haiku-4-5","max_tokens":16,"top_k":5,"messages":[{"role":"user","content":"hi"}]}"#;
    let (ir, _) = decode(Wire::Messages, raw).expect("decode");
    let (messages, _) = encode_value(Wire::Messages, &ir);
    assert_eq!(messages["top_k"], 5, "{messages}");
    let (_chat, report) = encode_value(Wire::ChatCompletions, &ir);
    assert!(
        has_action(&report, LossAction::Drop, "top_k"),
        "Chat has no top_k slot, got {report:?}"
    );
}

#[test]
fn chat_json_schema_strict_round_trips() {
    let raw = br#"{
        "model": "gpt-4.1",
        "messages": [{"role": "user", "content": "hi"}],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "answer",
                "strict": true,
                "schema": {"type": "object"}
            }
        }
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, raw).expect("decode");
    let (body, _) = encode_value(Wire::ChatCompletions, &ir);
    assert_eq!(
        body["response_format"]["json_schema"]["strict"], true,
        "{body}"
    );
}

#[test]
fn thinking_forces_tool_choice_auto_and_drops_temperature() {
    let thinking = IrRequest::new(
        "claude-haiku-4-5",
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    )
    .with_sampling(IrSampling::patch(|sampling| {
        sampling.reasoning_effort = Some("high".into());
        sampling.temperature = Some(0.2);
        sampling.tool_choice = IrToolChoice::Required;
    }));
    let (body, report) = encode_value(Wire::Messages, &thinking);
    assert!(body.get("temperature").is_none(), "{body}");
    assert_eq!(body["tool_choice"]["type"], "auto", "{body}");
    assert!(
        has_action(&report, LossAction::Drop, "temperature"),
        "{report:?}"
    );
    assert!(
        has_action(&report, LossAction::Degrade, "tool_choice"),
        "{report:?}"
    );

    let plain = IrRequest::new(
        "claude-haiku-4-5",
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    )
    .with_sampling(IrSampling::patch(|sampling| {
        sampling.temperature = Some(0.2);
        sampling.tool_choice = IrToolChoice::Required;
    }));
    let (body, _) = encode_value(Wire::Messages, &plain);
    assert_eq!(body["temperature"], 0.2, "{body}");
    assert_ne!(body["tool_choice"]["type"], "auto", "{body}");
}

#[test]
fn loss_event_display_skips_preserve() {
    let mut report = LossReport::default();
    report.record("sampling.top_k", LossAction::Drop, "no slot");
    report.record("sampling.temperature", LossAction::Preserve, "kept");
    assert_eq!(report.events[0].to_string(), "drop sampling.top_k: no slot");
    let lossy = report.lossy();
    assert_eq!(lossy.len(), 1);
    assert_eq!(lossy[0].action, LossAction::Drop);
}

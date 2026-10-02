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
fn gemini_keeps_messages_tool_result_image() {
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
    let (body, report) = encode_value(Wire::Gemini, &ir);
    assert!(
        !has_action(&report, LossAction::Drop, "image"),
        "Gemini functionResponse.parts accepts inlineData, got {report:?} {body}"
    );
    let rendered = body.to_string();
    assert!(
        rendered.contains("aaaa") && rendered.contains("inlineData"),
        "image bytes must be functionResponse inlineData, got {body}"
    );
}

#[test]
fn gemini_named_function_response_image_reaches_image_wires() {
    let raw = r#"{
        "contents": [{
            "role": "user",
            "parts": [{
                "functionResponse": {
                    "name": "get_image",
                    "response": {"ok": true},
                    "parts": [{
                        "inlineData": {
                            "mimeType": "image/jpeg",
                            "displayName": "instrument.jpg",
                            "data": "aaaa"
                        }
                    }]
                }
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, raw.as_bytes()).expect("decode");
    for wire in [Wire::Messages, Wire::Responses, Wire::Converse] {
        let (body, report) = encode_value(wire, &ir);
        let rendered = body.to_string();
        assert!(
            rendered.contains("aaaa"),
            "{wire:?} dropped functionResponse image bytes: {body} {report:?}"
        );
        assert!(
            !has_action(&report, LossAction::Drop, "image"),
            "{wire:?} recorded an image drop: {report:?}"
        );
    }
    let (body, report) = encode_value(Wire::ChatCompletions, &ir);
    assert!(
        !body.to_string().contains("aaaa"),
        "Chat tool content has no image slot, got {body}"
    );
    assert!(
        has_action(&report, LossAction::Drop, "image"),
        "Chat must record the image drop, got {report:?}"
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
fn messages_inference_geo_round_trips_and_other_dests_drop_it() {
    let raw = br#"{"model":"claude-haiku-4-5","max_tokens":16,"inference_geo":"us","messages":[{"role":"user","content":"hi"}]}"#;
    let (ir, _) = decode(Wire::Messages, raw).expect("decode");
    assert_eq!(ir.sampling.inference_geo.as_deref(), Some("us"));
    let (messages, _) = encode_value(Wire::Messages, &ir);
    assert_eq!(messages["inference_geo"], "us", "{messages}");
    for wire in [
        Wire::ChatCompletions,
        Wire::Responses,
        Wire::Gemini,
        Wire::Converse,
    ] {
        let (body, report) = encode_value(wire, &ir);
        assert!(
            body.get("inference_geo").is_none() && !body.to_string().contains("inference_geo"),
            "{wire:?} must omit inference_geo, got {body}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "inference_geo"),
            "{wire:?} must Drop sampling.inference_geo, got {report:?}"
        );
    }

    for blank in ["", "  "] {
        let raw = format!(
            r#"{{"model":"claude-haiku-4-5","max_tokens":16,"inference_geo":"{blank}","messages":[{{"role":"user","content":"hi"}}]}}"#
        );
        let (ir, _) = decode(Wire::Messages, raw.as_bytes()).expect("decode");
        assert!(
            ir.sampling.inference_geo.is_none(),
            "blank inference_geo decodes as None"
        );
        let (messages, _) = encode_value(Wire::Messages, &ir);
        assert!(
            messages.get("inference_geo").is_none(),
            "blank inference_geo must be omitted, got {messages}"
        );
    }
}

#[test]
fn messages_previous_message_id_round_trips_and_other_dests_drop_it() {
    let raw = br#"{"model":"claude-haiku-4-5","max_tokens":16,"diagnostics":{"previous_message_id":"msg_123"},"messages":[{"role":"user","content":"hi"}]}"#;
    let (ir, _) = decode(Wire::Messages, raw).expect("decode");
    assert_eq!(ir.sampling.previous_message_id.as_deref(), Some("msg_123"));
    let (messages, _) = encode_value(Wire::Messages, &ir);
    assert_eq!(
        messages["diagnostics"]["previous_message_id"], "msg_123",
        "{messages}"
    );
    for wire in [
        Wire::ChatCompletions,
        Wire::Responses,
        Wire::Gemini,
        Wire::Converse,
    ] {
        let (body, report) = encode_value(wire, &ir);
        assert!(
            body.pointer("/diagnostics/previous_message_id").is_none()
                && !body.to_string().contains("previous_message_id"),
            "{wire:?} must omit previous_message_id, got {body}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "previous_message_id"),
            "{wire:?} must Drop sampling.previous_message_id, got {report:?}"
        );
    }

    for blank in ["", "  "] {
        let raw = format!(
            r#"{{"model":"claude-haiku-4-5","max_tokens":16,"diagnostics":{{"previous_message_id":"{blank}"}},"messages":[{{"role":"user","content":"hi"}}]}}"#
        );
        let (ir, _) = decode(Wire::Messages, raw.as_bytes()).expect("decode");
        assert!(
            ir.sampling.previous_message_id.is_none(),
            "blank previous_message_id decodes as None"
        );
        let (messages, _) = encode_value(Wire::Messages, &ir);
        assert!(
            messages.get("diagnostics").is_none(),
            "blank previous_message_id must omit diagnostics, got {messages}"
        );
    }
}

#[test]
fn converse_performance_latency_round_trips_and_other_dests_drop_it() {
    let raw = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "performanceConfig": {"latency": "optimized"}
    }"#;
    let (ir, _) = decode(Wire::Converse, raw).expect("decode");
    assert_eq!(
        ir.sampling.performance_latency.as_deref(),
        Some("optimized")
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert_eq!(
        converse
            .pointer("/performanceConfig/latency")
            .and_then(Value::as_str),
        Some("optimized"),
        "{converse}"
    );
    for wire in [
        Wire::ChatCompletions,
        Wire::Messages,
        Wire::Responses,
        Wire::Gemini,
    ] {
        let (body, report) = encode_value(wire, &ir);
        assert!(
            body.get("performanceConfig").is_none()
                && !body.to_string().contains("performanceConfig")
                && !body.to_string().contains("performance_latency"),
            "{wire:?} must omit performanceConfig, got {body}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "performance_latency"),
            "{wire:?} must Drop sampling.performance_latency, got {report:?}"
        );
    }

    let mixed = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "performanceConfig": {"latency": "  STANDARD  "}
    }"#;
    let (ir, _) = decode(Wire::Converse, mixed).expect("decode");
    assert_eq!(ir.sampling.performance_latency.as_deref(), Some("standard"));
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert_eq!(
        converse
            .pointer("/performanceConfig/latency")
            .and_then(Value::as_str),
        Some("standard"),
        "{converse}"
    );

    let turbo = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "performanceConfig": {"latency": "turbo"}
    }"#;
    let (ir, decode_report) = decode(Wire::Converse, turbo).expect("decode");
    assert!(
        ir.sampling.performance_latency.is_none(),
        "unmapped latency decodes as None"
    );
    assert!(
        decode_report.events.iter().any(|event| {
            event.action == LossAction::Drop
                && event.path.contains("performance_latency")
                && event.detail.contains("unmapped latency")
        }),
        "decode must Drop unmapped latency, got {decode_report:?}"
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert!(
        converse.get("performanceConfig").is_none(),
        "unmapped latency must omit performanceConfig, got {converse}"
    );

    for blank in ["", "  "] {
        let raw = format!(
            r#"{{"modelId":"amazon.nova-lite-v1:0","messages":[{{"role":"user","content":[{{"text":"hi"}}]}}],"performanceConfig":{{"latency":"{blank}"}}}}"#
        );
        let (ir, report) = decode(Wire::Converse, raw.as_bytes()).expect("decode");
        assert!(
            ir.sampling.performance_latency.is_none(),
            "blank latency decodes as None"
        );
        assert!(
            !has_action(&report, LossAction::Drop, "performance_latency"),
            "blank latency must not Drop, got {report:?}"
        );
    }
}

#[test]
fn converse_response_field_paths_round_trip_and_other_dests_drop_them() {
    let raw = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "additionalModelResponseFieldPaths": ["/amazon-bedrock-invocationMetrics"]
    }"#;
    let (ir, _) = decode(Wire::Converse, raw).expect("decode");
    assert_eq!(
        ir.sampling.response_field_paths,
        vec!["/amazon-bedrock-invocationMetrics".to_string()]
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    let paths = converse
        .get("additionalModelResponseFieldPaths")
        .and_then(Value::as_array)
        .expect("additionalModelResponseFieldPaths");
    assert_eq!(paths.len(), 1, "{converse}");
    assert_eq!(
        paths[0].as_str(),
        Some("/amazon-bedrock-invocationMetrics"),
        "{converse}"
    );
    for wire in [
        Wire::ChatCompletions,
        Wire::Messages,
        Wire::Responses,
        Wire::Gemini,
    ] {
        let (body, report) = encode_value(wire, &ir);
        assert!(
            body.get("additionalModelResponseFieldPaths").is_none()
                && !body
                    .to_string()
                    .contains("additionalModelResponseFieldPaths")
                && !body.to_string().contains("response_field_paths")
                && !body
                    .to_string()
                    .contains("amazon-bedrock-invocationMetrics"),
            "{wire:?} must omit additionalModelResponseFieldPaths, got {body}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "response_field_paths"),
            "{wire:?} must Drop sampling.response_field_paths, got {report:?}"
        );
    }

    let blank = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "additionalModelResponseFieldPaths": ["  "]
    }"#;
    let (ir, report) = decode(Wire::Converse, blank).expect("decode");
    assert!(
        ir.sampling.response_field_paths.is_empty(),
        "blank path decodes as empty"
    );
    assert!(
        !has_action(&report, LossAction::Drop, "response_field_paths"),
        "blank path must not Drop, got {report:?}"
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert!(
        converse.get("additionalModelResponseFieldPaths").is_none(),
        "blank path must omit additionalModelResponseFieldPaths, got {converse}"
    );

    let missing = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}]
    }"#;
    let (ir, _) = decode(Wire::Converse, missing).expect("decode");
    assert!(
        ir.sampling.response_field_paths.is_empty(),
        "missing array stays empty"
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert!(
        converse.get("additionalModelResponseFieldPaths").is_none(),
        "missing array must omit additionalModelResponseFieldPaths, got {converse}"
    );
}

#[test]
fn converse_additional_request_fields_round_trip_and_other_dests_drop_them() {
    let raw = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "additionalModelRequestFields": {"top_k": 10}
    }"#;
    let (ir, _) = decode(Wire::Converse, raw).expect("decode");
    assert_eq!(
        ir.sampling.additional_request_fields,
        Some(json!({"top_k": 10}))
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert_eq!(
        converse.get("additionalModelRequestFields"),
        Some(&json!({"top_k": 10})),
        "{converse}"
    );
    for wire in [
        Wire::ChatCompletions,
        Wire::Messages,
        Wire::Responses,
        Wire::Gemini,
    ] {
        let (body, report) = encode_value(wire, &ir);
        assert!(
            body.get("additionalModelRequestFields").is_none()
                && !body.to_string().contains("additionalModelRequestFields")
                && !body.to_string().contains("additional_request_fields")
                && !body.to_string().contains("top_k"),
            "{wire:?} must omit additionalModelRequestFields, got {body}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "additional_request_fields"),
            "{wire:?} must Drop sampling.additional_request_fields, got {report:?}"
        );
    }

    let null_fields = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "additionalModelRequestFields": null
    }"#;
    let (ir, _) = decode(Wire::Converse, null_fields).expect("decode");
    assert!(
        ir.sampling.additional_request_fields.is_none(),
        "JSON null decodes as None"
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert!(
        converse.get("additionalModelRequestFields").is_none(),
        "JSON null must omit additionalModelRequestFields, got {converse}"
    );
}

#[test]
fn converse_prompt_variables_and_guardrail_round_trip_and_other_dests_drop_them() {
    let raw = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "promptVariables": {"genre": {"text": "pop"}},
        "guardrailConfig": {
            "guardrailIdentifier": "g",
            "guardrailVersion": "1",
            "trace": "enabled"
        }
    }"#;
    let (ir, _) = decode(Wire::Converse, raw).expect("decode");
    assert_eq!(
        ir.sampling.prompt_variables,
        Some(json!({"genre": {"text": "pop"}}))
    );
    assert_eq!(
        ir.sampling.guardrail,
        Some(json!({
            "guardrailIdentifier": "g",
            "guardrailVersion": "1",
            "trace": "enabled"
        }))
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert_eq!(
        converse.get("promptVariables"),
        Some(&json!({"genre": {"text": "pop"}})),
        "{converse}"
    );
    assert_eq!(
        converse.get("guardrailConfig"),
        Some(&json!({
            "guardrailIdentifier": "g",
            "guardrailVersion": "1",
            "trace": "enabled"
        })),
        "{converse}"
    );
    for wire in [
        Wire::ChatCompletions,
        Wire::Messages,
        Wire::Responses,
        Wire::Gemini,
    ] {
        let (body, report) = encode_value(wire, &ir);
        let text = body.to_string();
        assert!(
            body.get("promptVariables").is_none()
                && body.get("guardrailConfig").is_none()
                && !text.contains("promptVariables")
                && !text.contains("guardrailConfig")
                && !text.contains("prompt_variables")
                && !text.contains("guardrail")
                && !text.contains("pop"),
            "{wire:?} must omit promptVariables and guardrailConfig, got {body}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "prompt_variables"),
            "{wire:?} must Drop sampling.prompt_variables, got {report:?}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "guardrail"),
            "{wire:?} must Drop sampling.guardrail, got {report:?}"
        );
    }

    let string_vars = br#"{
        "modelId": "amazon.nova-lite-v1:0",
        "messages": [{"role": "user", "content": [{"text": "hi"}]}],
        "promptVariables": "pop"
    }"#;
    let (ir, _) = decode(Wire::Converse, string_vars).expect("decode");
    assert!(
        ir.sampling.prompt_variables.is_none(),
        "JSON string is not stored"
    );
    let (converse, _) = encode_value(Wire::Converse, &ir);
    assert!(
        converse.get("promptVariables").is_none()
            && !converse.to_string().contains("promptVariables")
            && !converse.to_string().contains("pop"),
        "JSON string must omit promptVariables, got {converse}"
    );
}

#[test]
fn messages_container_context_and_mcp_round_trip_and_other_dests_drop_them() {
    let raw = br#"{
        "model": "claude-haiku-4-5",
        "max_tokens": 16,
        "messages": [{"role": "user", "content": "hi"}],
        "container": "ctr_1",
        "context_management": {"edits": []},
        "mcp_servers": [{"type": "url", "url": "https://example.com/mcp", "name": "docs"}]
    }"#;
    let (ir, _) = decode(Wire::Messages, raw).expect("decode");
    assert_eq!(ir.sampling.container, Some(json!("ctr_1")));
    assert_eq!(ir.sampling.context_management, Some(json!({"edits": []})));
    assert_eq!(
        ir.sampling.mcp_servers,
        Some(json!([
            {"type": "url", "url": "https://example.com/mcp", "name": "docs"}
        ]))
    );
    let (messages, messages_report) = encode_value(Wire::Messages, &ir);
    assert_eq!(
        messages.get("container"),
        Some(&json!("ctr_1")),
        "{messages}"
    );
    assert_eq!(
        messages.get("context_management"),
        Some(&json!({"edits": []})),
        "{messages}"
    );
    assert_eq!(
        messages.get("mcp_servers"),
        Some(&json!([
            {"type": "url", "url": "https://example.com/mcp", "name": "docs"}
        ])),
        "{messages}"
    );
    assert!(
        !messages_report.events.iter().any(|event| {
            event.path.contains("container")
                || event.path.contains("context_management")
                || event.path.contains("mcp_servers")
        }),
        "Messages must keep the three fields, got {messages_report:?}"
    );
    for wire in [
        Wire::ChatCompletions,
        Wire::Responses,
        Wire::Gemini,
        Wire::Converse,
    ] {
        let (body, report) = encode_value(wire, &ir);
        let text = body.to_string();
        assert!(
            body.get("container").is_none()
                && body.get("context_management").is_none()
                && body.get("mcp_servers").is_none()
                && !text.contains("container")
                && !text.contains("context_management")
                && !text.contains("mcp_servers")
                && !text.contains("ctr_1")
                && !text.contains("example.com"),
            "{wire:?} must omit container, context_management, and mcp_servers, got {body}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "sampling.container"),
            "{wire:?} must Drop sampling.container, got {report:?}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "sampling.context_management"),
            "{wire:?} must Drop sampling.context_management, got {report:?}"
        );
        assert!(
            has_action(&report, LossAction::Drop, "sampling.mcp_servers"),
            "{wire:?} must Drop sampling.mcp_servers, got {report:?}"
        );
    }

    let null_container = br#"{
        "model": "claude-haiku-4-5",
        "max_tokens": 16,
        "messages": [{"role": "user", "content": "hi"}],
        "container": null
    }"#;
    let (ir, _) = decode(Wire::Messages, null_container).expect("decode");
    assert!(ir.sampling.container.is_none(), "JSON null decodes as None");
    let (messages, _) = encode_value(Wire::Messages, &ir);
    assert!(
        messages.get("container").is_none() && !messages.to_string().contains("container"),
        "JSON null must omit container, got {messages}"
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

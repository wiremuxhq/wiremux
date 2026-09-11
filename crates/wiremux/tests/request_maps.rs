//! Request-map goldens. Fixtures are redacted JSON, not live captures.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use wiremux::{
    IrItem, IrTool, LossAction, MapError, ResolvedProfile, Wire, decode, encode, parse_profile_str,
};

fn golden(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens/requests")
        .join(name);
    fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn profile(toml: &str) -> ResolvedProfile {
    parse_profile_str(toml).expect("test profile parses")
}

fn hard_error_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-hard-error"
wire = "responses"
tool_type_policy = "hard-error"
"#,
    )
}

fn flatten_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-flatten"
wire = "responses"
tool_type_policy = "flatten-namespace"
"#,
    )
}

fn openrouter_forbid_store() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-openrouter-store"
wire = "responses"
tool_type_policy = "flatten-namespace"

[fingerprint]
forbidden_body_fields = ["store"]
forbidden_field_policy = "hard-error"
"#,
    )
}

fn messages_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-messages"
wire = "messages"
tool_type_policy = "hard-error"
"#,
    )
}

fn chat_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "test-chat"
wire = "chat-completions"
tool_type_policy = "hard-error"
"#,
    )
}

fn namespace_tool(tools: &[IrTool]) -> &IrTool {
    tools
        .iter()
        .find(|tool| matches!(tool, IrTool::Namespace { .. }))
        .expect("namespace tool must not be silent-stripped")
}

#[test]
fn codex_namespace_is_not_silent_stripped() {
    let bytes = golden("codex_namespace.json");
    let (ir, _loss) = decode(Wire::Responses, &bytes).expect("decode Responses");

    let IrTool::Namespace { name, raw } = namespace_tool(&ir.tools) else {
        panic!("expected IrTool::Namespace, got {:?}", ir.tools);
    };
    assert_eq!(name, "mcp__fs");
    assert_eq!(raw["type"], "namespace");
    assert_eq!(raw["tools"][0]["name"], "list_dir");
    assert_eq!(raw["tools"][0]["description"], "List a directory");
    assert!(
        raw["tools"][0]["parameters"]["properties"]
            .get("path")
            .is_some()
    );

    let err = encode(Wire::Messages, &ir, &hard_error_profile())
        .expect_err("default hard-error must fail closed on type=namespace, not strip tools");
    match err {
        MapError::HardError { path, detail } => {
            assert!(path.contains("tools"), "path={path}");
            assert!(
                detail.contains("namespace"),
                "hard-error should name namespace, got {detail}"
            );
        }
        other => panic!("expected HardError, got {other}"),
    }

    let err = encode(Wire::ChatCompletions, &ir, &hard_error_profile())
        .expect_err("Chat hard-error must not silent-strip namespace");
    assert!(matches!(err, MapError::HardError { .. }), "{err}");

    let (preserved, loss) =
        encode(Wire::Responses, &ir, &hard_error_profile()).expect("Responses keeps namespace");
    let body: Value = serde_json::from_slice(&preserved).expect("json");
    assert_eq!(body["tools"][0]["type"], "namespace");
    assert_eq!(body["tools"][0]["name"], "mcp__fs");
    assert_eq!(body["tools"][0]["tools"][0]["name"], "list_dir");
    assert!(
        loss.events
            .iter()
            .any(|event| event.action == LossAction::Preserve && event.path.contains("tools")),
        "expected Preserve on namespace tools, got {:?}",
        loss.events
    );

    let (flat, loss) =
        encode(Wire::ChatCompletions, &ir, &flatten_profile()).expect("flatten-namespace");
    let body: Value = serde_json::from_slice(&flat).expect("json");
    let tool = &body["tools"][0];
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["function"]["name"], "mcp__fs.list_dir");
    assert_eq!(tool["function"]["description"], "List a directory");
    assert!(
        tool["function"]["parameters"]["properties"]
            .get("path")
            .is_some(),
        "flatten must keep parameters, not a name-only stub: {tool}"
    );
    assert!(
        loss.events
            .iter()
            .any(|event| event.action == LossAction::Degrade && event.detail.contains("flatten")),
        "expected flatten Degrade, got {:?}",
        loss.events
    );

    let (flat_resp, _) =
        encode(Wire::Responses, &ir, &flatten_profile()).expect("flatten on Responses");
    let body: Value = serde_json::from_slice(&flat_resp).expect("json");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "mcp__fs.list_dir");
    assert_ne!(
        body["tools"][0]["type"], "namespace",
        "openrouter-style flatten-namespace must not emit type=namespace"
    );
}

#[test]
fn store_true_forbidden_is_hard_error() {
    let bytes = golden("store_true.json");
    let (ir, _loss) = decode(Wire::Responses, &bytes).expect("decode");
    assert_eq!(ir.sampling.store, Some(true));

    let err = encode(Wire::Responses, &ir, &openrouter_forbid_store())
        .expect_err("forbidden store must hard-error");
    match err {
        MapError::HardError { path, detail } => {
            assert!(path.contains("store"), "path={path}");
            assert!(detail.contains("store"), "detail={detail}");
        }
        other => panic!("expected HardError, got {other}"),
    }

    let (ok, loss) = encode(Wire::Responses, &ir, &hard_error_profile()).expect("store allowed");
    let body: Value = serde_json::from_slice(&ok).expect("json");
    assert_eq!(body["store"], true);
    assert!(
        loss.events
            .iter()
            .any(|event| event.path.contains("store") && event.action == LossAction::Preserve),
        "expected Preserve store, got {:?}",
        loss.events
    );
}

#[test]
fn developer_degrades_to_system_on_messages() {
    let bytes = golden("developer_chat.json");
    let (ir, _loss) = decode(Wire::ChatCompletions, &bytes).expect("decode Chat");
    assert!(
        ir.items.iter().any(
            |item| matches!(item, IrItem::Developer { text } if text == "Follow house style.")
        ),
        "developer item missing: {:?}",
        ir.items
    );

    let (chat_bytes, chat_loss) =
        encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("Chat preserves developer");
    let chat: Value = serde_json::from_slice(&chat_bytes).expect("json");
    assert_eq!(chat["messages"][0]["role"], "developer");
    assert_eq!(chat["messages"][0]["content"], "Follow house style.");
    assert!(
        chat_loss
            .events
            .iter()
            .any(|event| event.action == LossAction::Preserve && event.detail.contains("developer")),
        "expected Preserve developer, got {:?}",
        chat_loss.events
    );

    let (msg_bytes, loss) =
        encode(Wire::Messages, &ir, &messages_profile()).expect("Messages encode");
    let body: Value = serde_json::from_slice(&msg_bytes).expect("json");
    assert!(
        loss.events.iter().any(|event| {
            event.action == LossAction::Degrade && event.detail.contains("developer to system")
        }),
        "expected developer → system Degrade, got {:?}",
        loss.events
    );

    let system = &body["system"];
    let system_text = if let Some(s) = system.as_str() {
        s.to_string()
    } else {
        system
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        system_text.contains("Follow house style."),
        "system should carry developer text, got {system}"
    );
    if let Some(messages) = body["messages"].as_array() {
        assert!(
            messages
                .iter()
                .all(|msg| msg.get("role").and_then(Value::as_str) != Some("developer")),
            "Messages must not emit role=developer: {messages:?}"
        );
    }
}

#[test]
fn responses_messages_responses_round_trip_keeps_tool_names() {
    let bytes = golden("roundtrip_responses.json");
    let flatten = flatten_profile();

    let (ir1, _l1) = decode(Wire::Responses, &bytes).expect("decode Responses");
    let IrTool::Namespace { name, .. } = namespace_tool(&ir1.tools) else {
        panic!("round-trip source must keep namespace");
    };
    assert_eq!(name, "crm");

    let (msg_bytes, msg_loss) = encode(Wire::Messages, &ir1, &flatten).expect("encode Messages");
    assert!(
        msg_loss.events.iter().any(|event| {
            event.action == LossAction::Degrade && event.path.contains("previous_response_id")
        }),
        "previous_response_id should degrade off Responses, got {:?}",
        msg_loss.events
    );
    assert!(
        msg_loss
            .events
            .iter()
            .any(|event| event.action == LossAction::Degrade && event.detail.contains("flatten")),
        "namespace should flatten toward Messages, got {:?}",
        msg_loss.events
    );

    let messages: Value = serde_json::from_slice(&msg_bytes).expect("json");
    let msg_tools = messages["tools"]
        .as_array()
        .expect("Messages tools")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        msg_tools.contains(&"crm.lookup"),
        "flattened dotted name missing: {msg_tools:?}"
    );
    assert!(messages.get("previous_response_id").is_none());

    let (ir2, _l2) = decode(Wire::Messages, &msg_bytes).expect("decode Messages");
    let function_names: Vec<_> = ir2
        .tools
        .iter()
        .filter_map(|tool| match tool {
            IrTool::Function { name, .. } => Some(name.as_str()),
            IrTool::Namespace { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        function_names.iter().any(|name| name.contains("lookup")),
        "tool name lookup lost after Messages decode: {function_names:?}"
    );

    let (resp_bytes, resp_loss) =
        encode(Wire::Responses, &ir2, &flatten).expect("encode Responses");
    let resp: Value = serde_json::from_slice(&resp_bytes).expect("json");
    let tools = resp["tools"].as_array().expect("Responses tools");
    let names = collect_tool_names(tools);
    assert!(
        names.iter().any(|name| name == "crm.lookup"),
        "round-trip must keep CRM tool names as dotted functions, got {names:?}"
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.get("type").and_then(Value::as_str) == Some("function")),
        "flatten-namespace encode to Responses must emit function tools, got {tools:?}"
    );
    assert!(
        !resp_loss.events.is_empty() || !msg_loss.events.is_empty(),
        "round-trip must report loss"
    );
}

#[test]
fn unknown_type_with_name_is_not_relabeled_function() {
    let bytes = br#"{
        "model": "gpt-5",
        "input": "hi",
        "tools": [{
            "type": "weird",
            "name": "do_thing",
            "description": "A thing",
            "parameters": {"type": "object", "properties": {}}
        }]
    }"#;
    let (ir, _) = decode(Wire::Responses, bytes).expect("decode");
    assert!(
        matches!(&ir.tools[0], IrTool::Unknown { type_name, .. } if type_name == "weird"),
        "type=weird with name must stay Unknown, got {:?}",
        ir.tools
    );
    encode(Wire::Responses, &ir, &hard_error_profile())
        .expect_err("hard-error must fail closed on unknown type");
    encode(Wire::Responses, &ir, &flatten_profile())
        .expect_err("flatten-namespace must not coerce unknown type to function");

    let passthrough = profile(
        r#"
schema_version = 1
id = "test-passthrough"
wire = "responses"
tool_type_policy = "passthrough"
"#,
    );
    let (out, _) = encode(Wire::Responses, &ir, &passthrough).expect("passthrough");
    let body: Value = serde_json::from_slice(&out).expect("json");
    assert_eq!(body["tools"][0]["type"], "weird");
    assert_eq!(body["tools"][0]["name"], "do_thing");
}

fn collect_tool_names(tools: &[Value]) -> Vec<String> {
    let mut names = Vec::new();
    for tool in tools {
        if let Some(name) = tool.get("name").and_then(Value::as_str) {
            names.push(name.to_string());
        }
        if let Some(name) = tool
            .get("function")
            .and_then(|func| func.get("name"))
            .and_then(Value::as_str)
        {
            names.push(name.to_string());
        }
        if let Some(inner) = tool.get("tools").and_then(Value::as_array) {
            for child in inner {
                if let Some(name) = child.get("name").and_then(Value::as_str) {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

#[test]
fn stream_true_survives_chat_messages_responses() {
    let chat_req = br#"{
        "model": "grok-4",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, chat_req).expect("decode chat stream");
    assert_eq!(
        ir.sampling.stream,
        Some(true),
        "Grok always-SSE must not drop stream:true"
    );

    for (wire, profile) in [
        (Wire::ChatCompletions, chat_profile()),
        (Wire::Messages, messages_profile()),
        (Wire::Responses, flatten_profile()),
    ] {
        let (bytes, _) = encode(wire, &ir, &profile).expect("encode keeps stream");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body.get("stream"),
            Some(&Value::Bool(true)),
            "{wire:?} must send stream:true, got {body}"
        );
    }
}

#[test]
fn stream_absent_is_not_invented() {
    let chat_req = br#"{
        "model": "grok-4",
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, chat_req).expect("decode");
    assert_eq!(ir.sampling.stream, None);
    let (bytes, _) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("stream").is_none(),
        "must not invent stream when the source omitted it: {body}"
    );
}

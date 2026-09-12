//! Request-map goldens. Fixtures are redacted JSON, not live captures.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use wiremux::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrTool, IrToolChoice, LossAction, LossReport,
    MapError, ResolvedProfile, Wire, decode, encode, parse_profile_str,
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
fn gemini_request_round_trip_text_and_function() {
    let req = br#"{
        "model": "gemini-2.5-flash",
        "systemInstruction": { "parts": [{ "text": "Be brief." }] },
        "contents": [
            { "role": "user", "parts": [{ "text": "hi" }] },
            { "role": "model", "parts": [{ "functionCall": { "name": "lookup", "args": { "q": "x" } } }] },
            { "role": "user", "parts": [{ "functionResponse": { "name": "lookup", "response": { "ok": true } } }] }
        ],
        "tools": [{
            "functionDeclarations": [{
                "name": "lookup",
                "description": "Look up",
                "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
            }]
        }],
        "generationConfig": { "temperature": 0.2, "maxOutputTokens": 64 }
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode gemini");
    assert_eq!(ir.model, "gemini-2.5-flash");
    assert!(
        ir.items
            .iter()
            .any(|item| matches!(item, IrItem::System { text } if text == "Be brief.")),
        "systemInstruction: {:?}",
        ir.items
    );
    assert!(
        ir.items
            .iter()
            .any(|item| matches!(item, IrItem::FunctionCall { name, .. } if name == "lookup")),
        "functionCall: {:?}",
        ir.items
    );
    assert_eq!(ir.sampling.temperature, Some(0.2));
    assert_eq!(ir.sampling.max_tokens, Some(64));

    let profile = profile(
        r#"
schema_version = 1
id = "test-gemini"
wire = "gemini"
"#,
    );
    let (bytes, _) = encode(Wire::Gemini, &ir, &profile).expect("encode gemini");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/systemInstruction/parts/0/text")
            .and_then(Value::as_str),
        Some("Be brief.")
    );
    assert_eq!(
        body.pointer("/tools/0/functionDeclarations/0/name")
            .and_then(Value::as_str),
        Some("lookup")
    );
    assert_eq!(
        body.pointer("/contents/1/parts/0/functionCall/name")
            .and_then(Value::as_str),
        Some("lookup"),
        "encode must keep functionCall.name, got {body}"
    );
    assert_eq!(
        body.pointer("/contents/1/parts/0/functionCall/args/q")
            .and_then(Value::as_str),
        Some("x"),
        "encode must keep functionCall.args.q, got {body}"
    );
    assert_eq!(
        body.pointer("/contents/2/parts/0/functionResponse/name")
            .and_then(Value::as_str),
        Some("lookup"),
        "encode must keep functionResponse, got {body}"
    );
    assert_eq!(
        body.pointer("/contents/2/parts/0/functionResponse/response/ok"),
        Some(&Value::Bool(true)),
        "encode must keep functionResponse.response, got {body}"
    );
    let temp = body
        .pointer("/generationConfig/temperature")
        .and_then(Value::as_f64)
        .expect("temperature");
    assert!((temp - 0.2).abs() < 1e-6, "temperature={temp}");
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

#[test]
fn function_call_thought_signature_round_trips_on_next_request() {
    let req = br#"{
        "contents": [{
            "role": "model",
            "parts": [{
                "functionCall": { "name": "lookup", "args": { "q": "x" } },
                "thoughtSignature": "sig_thought_abc"
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let parts = body
        .pointer("/contents/0/parts")
        .and_then(Value::as_array)
        .expect("parts");
    let signed = parts.iter().find(|p| {
        p.pointer("/functionCall/name").and_then(Value::as_str) == Some("lookup")
            && p.get("thoughtSignature").and_then(Value::as_str) == Some("sig_thought_abc")
    });
    assert!(
        signed.is_some(),
        "function-call thoughtSignature must be sent back on the next request, got {body}"
    );
}

#[test]
fn thought_part_signature_is_not_stolen_by_later_function_call() {
    let req = br#"{
        "contents": [{
            "role": "model",
            "parts": [
                { "text": "plan", "thought": true, "thoughtSignature": "sig_thought_part" },
                { "functionCall": { "name": "lookup", "args": {} }, "thoughtSignature": "sig_function_call" }
            ]
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let parts: Vec<&Value> = body
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("parts").and_then(Value::as_array))
        .flatten()
        .collect();
    let thought_sig = parts.iter().find_map(|p| {
        (p.get("thought").and_then(Value::as_bool) == Some(true))
            .then(|| p.get("thoughtSignature").and_then(Value::as_str))
            .flatten()
    });
    let call_sig = parts.iter().find_map(|p| {
        p.get("functionCall")
            .and_then(|_| p.get("thoughtSignature").and_then(Value::as_str))
    });
    assert_eq!(thought_sig, Some("sig_thought_part"));
    assert_eq!(
        call_sig,
        Some("sig_function_call"),
        "function call must keep its own thoughtSignature, got {body}"
    );
}

#[test]
fn parallel_function_calls_only_first_signed() {
    let req = br#"{
        "contents": [{
            "role": "model",
            "parts": [
                { "functionCall": { "name": "one", "args": {} }, "thoughtSignature": "sig_first_only" },
                { "functionCall": { "name": "two", "args": {} } }
            ]
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let parts: Vec<&Value> = body
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("parts").and_then(Value::as_array))
        .flatten()
        .collect();
    let one = parts
        .iter()
        .find(|p| p.pointer("/functionCall/name").and_then(Value::as_str) == Some("one"))
        .expect("one");
    let two = parts
        .iter()
        .find(|p| p.pointer("/functionCall/name").and_then(Value::as_str) == Some("two"))
        .expect("two");
    assert_eq!(
        one.get("thoughtSignature").and_then(Value::as_str),
        Some("sig_first_only"),
        "first parallel call must keep its thoughtSignature, got {body}"
    );
    assert!(
        two.get("thoughtSignature").is_none(),
        "unsigned parallel call must not inherit a leftover thoughtSignature, got {body}"
    );
}

#[test]
fn gemini_thinking_config_round_trips() {
    let req = br#"{
        "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
        "thinkingConfig": { "includeThoughts": true, "thinkingBudget": 24576 }
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let tc = body
        .pointer("/generationConfig/thinkingConfig")
        .expect("thinkingConfig should be nested under generationConfig");
    assert_eq!(tc["includeThoughts"], true);
    assert_eq!(tc["thinkingBudget"], 24576);
    assert!(
        body.get("thinkingConfig").is_none(),
        "must not emit top-level thinkingConfig: {body}"
    );
}

#[test]
fn gemini_thinking_config_absent_is_not_invented() {
    let req = br#"{
        "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("thinkingConfig").is_none(),
        "must not invent thinkingConfig: {body}"
    );
    assert!(
        body.pointer("/generationConfig/thinkingConfig").is_none(),
        "must not invent nested thinkingConfig: {body}"
    );
}

#[test]
fn gemini_consecutive_function_responses_share_user_turn() {
    let req = br#"{
        "contents": [
            { "role": "user", "parts": [{ "text": "hi" }] },
            { "role": "model", "parts": [
                { "functionCall": { "name": "one", "args": {} }, "thoughtSignature": "s1" },
                { "functionCall": { "name": "two", "args": {} } }
            ]},
            { "role": "user", "parts": [
                { "functionResponse": { "name": "one", "response": { "ok": 1 } } },
                { "functionResponse": { "name": "two", "response": { "ok": 2 } } }
            ]}
        ]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .expect("contents");
    let model_turns = contents
        .iter()
        .filter(|c| c.get("role").and_then(Value::as_str) == Some("model"))
        .count();
    let user_tool = contents.iter().find(|c| {
        c.get("role").and_then(Value::as_str) == Some("user")
            && c.pointer("/parts/0/functionResponse").is_some()
    });
    assert_eq!(
        model_turns, 1,
        "text + calls must share one model content, got {body}"
    );
    let parts = user_tool
        .and_then(|c| c.get("parts").and_then(Value::as_array))
        .expect("merged user tool parts");
    assert_eq!(
        parts.len(),
        2,
        "consecutive functionResponse must share one user turn, got {body}"
    );
}

#[test]
fn gemini_function_response_uses_function_name_not_call_id() {
    let ir = wiremux::IrRequest {
        model: "gemini-2.5-flash".into(),
        items: vec![
            IrItem::FunctionCall {
                call_id: "call_abc".into(),
                name: "lookup".into(),
                arguments: "{}".into(),
                thought_signature: None,
            },
            IrItem::FunctionOutput {
                call_id: "call_abc".into(),
                output: "plain text".into(),
            },
        ],
        tools: vec![],
        sampling: wiremux::IrSampling::default(),
    };
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/contents/1/parts/0/functionResponse/name")
            .and_then(Value::as_str),
        Some("lookup"),
        "functionResponse.name must be the function name, got {body}"
    );
    assert_eq!(
        body.pointer("/contents/1/parts/0/functionResponse/response/result")
            .and_then(Value::as_str),
        Some("plain text"),
        "non-JSON output must be {{result: text}}, got {body}"
    );
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

#[test]
fn replay_thinking_and_signature_in_assistant_json() {
    let req = br#"{
        "model": "claude-opus-4-6",
        "messages": [{
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "I should greet them", "signature": "sig_abc" },
                { "type": "text", "text": "Hello" }
            ]
        }]
    }"#;
    let (ir, _) = decode(Wire::Messages, req).expect("decode");
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let content = body
        .pointer("/messages/0/content")
        .and_then(Value::as_array)
        .expect("content");
    assert!(
        content.iter().any(|block| {
            block.get("type").and_then(Value::as_str) == Some("thinking")
                && block.get("thinking").and_then(Value::as_str) == Some("I should greet them")
                && block.get("signature").and_then(Value::as_str) == Some("sig_abc")
        }),
        "replay JSON must include thinking + signature, got {body}"
    );
    assert!(
        !loss_dropped(&report, "part.thinking"),
        "signed thinking must not be recorded as dropped, got {report:?}"
    );
}

#[test]
fn unsigned_thinking_is_not_replayed_on_messages() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![IrItem::Assistant {
            parts: vec![
                IrPart::Thinking {
                    text: "scratch".into(),
                    signature: None,
                },
                IrPart::Text("Hello".into()),
            ],
        }],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let content = body
        .pointer("/messages/0/content")
        .and_then(Value::as_array)
        .expect("content");
    assert!(
        content
            .iter()
            .all(|block| block.get("type").and_then(Value::as_str) != Some("thinking")),
        "unsigned thinking must not be replayed, got {body}"
    );
    assert!(
        content
            .iter()
            .any(|block| block.get("text").and_then(Value::as_str) == Some("Hello")),
        "visible text must stay, got {body}"
    );
    assert!(
        loss_dropped(&report, "part.thinking"),
        "unsigned thinking drop missing, got {report:?}"
    );
}

#[test]
fn replay_redacted_thinking_in_assistant_json() {
    let req = br#"{
        "model": "claude-opus-4-6",
        "messages": [{
            "role": "assistant",
            "content": [
                { "type": "redacted_thinking", "data": "enc_abc" },
                { "type": "text", "text": "Hello" }
            ]
        }]
    }"#;
    let (ir, _) = decode(Wire::Messages, req).expect("decode");
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let content = body
        .pointer("/messages/0/content")
        .and_then(Value::as_array)
        .expect("content");
    assert!(
        content.iter().any(|block| {
            block.get("type").and_then(Value::as_str) == Some("redacted_thinking")
                && block.get("data").and_then(Value::as_str) == Some("enc_abc")
        }),
        "replay JSON must include redacted_thinking, got {body}"
    );
}

#[test]
fn chat_skips_thinking_and_protocol_parts() {
    let ir = IrRequest {
        model: "grok-4".into(),
        items: vec![IrItem::Assistant {
            parts: vec![
                IrPart::Thinking {
                    text: "plan".into(),
                    signature: Some("sig".into()),
                },
                IrPart::Raw {
                    type_name: "redacted_thinking".into(),
                    raw: serde_json::json!({"type": "redacted_thinking", "data": "enc"}),
                },
                IrPart::Text("Hello".into()),
            ],
        }],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let content = &body["messages"][0]["content"];
    assert_eq!(
        content, "Hello",
        "Chat must send only visible text, got {body}"
    );
    assert!(
        loss_dropped(&report, "part.thinking"),
        "thinking drop missing, got {report:?}"
    );
    assert!(
        loss_dropped(&report, "part.raw"),
        "raw drop missing, got {report:?}"
    );
}

#[test]
fn responses_asks_for_encrypted_reasoning_and_drops_unsigned_thinking() {
    let ir = IrRequest {
        model: "gpt-5".into(),
        items: vec![IrItem::Assistant {
            parts: vec![
                IrPart::Thinking {
                    text: "secret plan".into(),
                    signature: None,
                },
                IrPart::Raw {
                    type_name: "redacted_thinking".into(),
                    raw: serde_json::json!({"type": "redacted_thinking", "data": "enc"}),
                },
                IrPart::Text("Hello".into()),
            ],
        }],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("include"),
        Some(&serde_json::json!(["reasoning.encrypted_content"])),
        "Responses must request encrypted reasoning, got {body}"
    );
    let dumped = body.to_string();
    assert!(
        !dumped.contains("secret plan"),
        "unsigned thinking must not become output_text, got {body}"
    );
    assert!(
        !dumped.contains("redacted_thinking"),
        "raw protocol part must not be replayed, got {body}"
    );
    assert!(
        loss_dropped(&report, "part.thinking"),
        "thinking drop missing, got {report:?}"
    );
    assert!(
        loss_dropped(&report, "part.raw"),
        "raw drop missing, got {report:?}"
    );
}

#[test]
fn chat_stream_true_requests_include_usage() {
    let req = br#"{
        "model": "grok-4",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, req).expect("decode");
    let (bytes, _) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/stream_options/include_usage"),
        Some(&Value::Bool(true)),
        "stream:true must request usage, got {body}"
    );
}

#[test]
fn messages_whitespace_only_assistant_becomes_dot() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![IrItem::Assistant {
            parts: vec![IrPart::Text("\n".into())],
        }],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/messages/0/content/0/text")
            .and_then(Value::as_str),
        Some("."),
        "whitespace-only text must become '.', got {body}"
    );
}

#[test]
fn chat_data_url_becomes_image_base64_for_gemini() {
    let req = br#"{
        "model": "gpt-4o",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "see" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo=" } }
            ]
        }]
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, req).expect("decode");
    assert!(
        ir.items.iter().any(|item| matches!(
            item,
            IrItem::User { parts } if parts.iter().any(|p| matches!(
                p,
                IrPart::ImageBase64 { media_type, data }
                    if media_type == "image/png" && data == "iVBORw0KGgo="
            ))
        )),
        "data URL must become ImageBase64 with payload, got {:?}",
        ir.items
    );
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/contents/0/parts/1/inlineData/mimeType")
            .and_then(Value::as_str),
        Some("image/png"),
        "Gemini must get inlineData, got {body}"
    );
    assert_eq!(
        body.pointer("/contents/0/parts/1/inlineData/data")
            .and_then(Value::as_str),
        Some("iVBORw0KGgo="),
        "Gemini must encode inlineData/data, got {body}"
    );
}

#[test]
fn responses_data_url_becomes_image_base64_for_gemini() {
    let req = br#"{
        "model": "gpt-4o",
        "input": [{
            "role": "user",
            "content": [
                { "type": "input_text", "text": "see" },
                { "type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo=" }
            ]
        }]
    }"#;
    let (ir, _) = decode(Wire::Responses, req).expect("decode");
    assert!(
        ir.items.iter().any(|item| matches!(
            item,
            IrItem::User { parts } if parts.iter().any(|p| matches!(
                p,
                IrPart::ImageBase64 { media_type, data }
                    if media_type == "image/png" && data == "iVBORw0KGgo="
            ))
        )),
        "Responses data URL must become ImageBase64 with payload, got {:?}",
        ir.items
    );
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/contents/0/parts/1/inlineData/mimeType")
            .and_then(Value::as_str),
        Some("image/png"),
        "Gemini must get inlineData, got {body}"
    );
    assert_eq!(
        body.pointer("/contents/0/parts/1/inlineData/data")
            .and_then(Value::as_str),
        Some("iVBORw0KGgo="),
        "Gemini must encode inlineData/data, got {body}"
    );
}

#[test]
fn gemini_https_image_url_degrades_to_text_placeholder() {
    let ir = IrRequest {
        model: "gemini-2.5-flash".into(),
        items: vec![IrItem::User {
            parts: vec![
                IrPart::Text("see".into()),
                IrPart::ImageUrl("https://example.com/cat.png".into()),
            ],
        }],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/contents/0/parts/1/text")
            .and_then(Value::as_str),
        Some("[image: https://example.com/cat.png]"),
        "https ImageUrl must become a text placeholder, got {body}"
    );
    assert!(
        body.pointer("/contents/0/parts/1/inlineData").is_none(),
        "https ImageUrl must not become inlineData, got {body}"
    );
    assert!(
        body.pointer("/contents/0/parts/1/fileData").is_none(),
        "this pass must not invent fileData.fileUri, got {body}"
    );
    assert!(
        loss_degraded(&report, "part.image_url"),
        "ImageUrl degrade missing, got {report:?}"
    );
}

#[test]
fn gemini_raw_part_is_dropped_with_loss_report() {
    let ir = IrRequest {
        model: "gemini-2.5-flash".into(),
        items: vec![IrItem::User {
            parts: vec![
                IrPart::Text("hi".into()),
                IrPart::Raw {
                    type_name: "redacted_thinking".into(),
                    raw: serde_json::json!({"type": "redacted_thinking", "data": "enc"}),
                },
            ],
        }],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let parts = body
        .pointer("/contents/0/parts")
        .and_then(Value::as_array)
        .expect("parts");
    assert_eq!(parts.len(), 1, "Raw must be omitted, got {body}");
    assert_eq!(
        parts[0].get("text").and_then(Value::as_str),
        Some("hi"),
        "visible text must stay, got {body}"
    );
    assert!(
        !body.to_string().contains("redacted_thinking"),
        "Raw must not be replayed, got {body}"
    );
    assert!(
        loss_dropped(&report, "part.raw"),
        "Raw drop missing, got {report:?}"
    );
}

#[test]
fn gemini_raw_tool_is_dropped_with_loss_report() {
    let ir = IrRequest {
        model: "gemini-2.5-flash".into(),
        items: vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
        tools: vec![
            IrTool::Function {
                name: "lookup".into(),
                description: "Look up".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
            IrTool::Unknown {
                type_name: "weird".into(),
                raw: serde_json::json!({"type": "weird", "name": "do_thing"}),
            },
        ],
        sampling: IrSampling::default(),
    };
    let passthrough = profile(
        r#"
schema_version = 1
id = "test-gemini-passthrough"
wire = "gemini"
tool_type_policy = "passthrough"
"#,
    );
    let (bytes, report) = encode(Wire::Gemini, &ir, &passthrough).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let decls = body
        .pointer("/tools/0/functionDeclarations")
        .and_then(Value::as_array)
        .expect("functionDeclarations");
    assert_eq!(decls.len(), 1, "Raw must be omitted from decls, got {body}");
    assert_eq!(
        decls[0].get("name").and_then(Value::as_str),
        Some("lookup"),
        "function tool must stay, got {body}"
    );
    assert!(
        !body.to_string().contains("weird") && !body.to_string().contains("do_thing"),
        "Raw tool must not appear in generateContent JSON, got {body}"
    );
    assert!(
        body.pointer("/tools/0/fileData").is_none(),
        "this pass must not invent fileData, got {body}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.action == LossAction::Drop
                && (event.path == "tools[1]" || event.path == "tool.raw")
                && event
                    .detail
                    .contains("raw tool has no generateContent slot")
        }),
        "Raw tool drop missing, got {report:?}"
    );
}

fn count_cache_control(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            let here = usize::from(map.contains_key("cache_control"));
            here + map.values().map(count_cache_control).sum::<usize>()
        }
        Value::Array(arr) => arr.iter().map(count_cache_control).sum(),
        _ => 0,
    }
}

#[test]
fn prompt_caching_on_system_and_first_user() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::System {
                text: "rules".into(),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello world".into())],
            },
        ],
        tools: vec![],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: None,
                ..Default::default()
            },
            ..IrSampling::default()
        },
    };
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.pointer("/system/cache_control").is_some()
            || body.pointer("/system/0/cache_control").is_some(),
        "system must have cache_control, got {body}"
    );
    assert!(
        body.pointer("/messages/0/content/0/cache_control")
            .is_some(),
        "first user text must have cache_control, got {body}"
    );
    assert!(
        count_cache_control(&body) <= 4,
        "must stay under Anthropic cap, got {body}"
    );
}

#[test]
fn long_ttl_with_tools_tags_last_tool_before_system() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::System {
                text: "rules".into(),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
        tools: vec![
            IrTool::Function {
                name: "one".into(),
                description: "a".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            IrTool::Function {
                name: "two".into(),
                description: "b".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: Some("1h".into()),
                ..Default::default()
            },
            ..IrSampling::default()
        },
    };
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let tools = body.get("tools").and_then(Value::as_array).expect("tools");
    assert!(
        tools[0].get("cache_control").is_none(),
        "only last tool is tagged, got {body}"
    );
    assert_eq!(
        tools[1]
            .pointer("/cache_control/ttl")
            .and_then(Value::as_str),
        Some("1h"),
        "last tool must carry 1h, got {body}"
    );
    let sys_ttl = body
        .pointer("/system/cache_control/ttl")
        .or_else(|| body.pointer("/system/0/cache_control/ttl"))
        .and_then(Value::as_str);
    assert_eq!(
        sys_ttl,
        Some("1h"),
        "first system must carry 1h, got {body}"
    );
    assert!(
        count_cache_control(&body) <= 2,
        "preferred pair is two markers, got {body}"
    );
}

#[test]
fn cache_disabled_no_cache_control_blocks() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::System {
                text: "rules".into(),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
        tools: vec![IrTool::Function {
            name: "one".into(),
            description: "a".into(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        sampling: IrSampling::default(),
    };
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(count_cache_control(&body), 0, "got {body}");
}

#[test]
fn cache_retention_none_skips_cache_control() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![IrItem::System {
            text: "rules".into(),
        }],
        tools: vec![],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: Some("none".into()),
                ..Default::default()
            },
            ..IrSampling::default()
        },
    };
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(count_cache_control(&body), 0, "retention=none, got {body}");
}

const ANTHROPIC_MAX_CACHE_CONTROL: usize = 4;
const ANTHROPIC_PREFERRED_CACHE_CONTROL: usize = 2;

fn system_ttl(body: &Value, index: usize) -> Option<&str> {
    body.pointer(&format!("/system/{index}/cache_control/ttl"))
        .and_then(Value::as_str)
}

fn system_has_cache_control(body: &Value, index: usize) -> bool {
    body.pointer(&format!("/system/{index}/cache_control"))
        .is_some()
}

#[test]
fn multi_fragment_system_stays_at_or_under_cache_control_limit() {
    let mut items: Vec<IrItem> = (0..6)
        .map(|i| IrItem::System {
            text: format!("system fragment {i}"),
        })
        .collect();
    items.push(IrItem::User {
        parts: vec![IrPart::Text("do the work".into())],
    });
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items,
        tools: vec![],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: Some("1h".into()),
                ..Default::default()
            },
            ..IrSampling::default()
        },
    };
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let system = body
        .get("system")
        .and_then(Value::as_array)
        .expect("system");
    assert_eq!(
        system.len(),
        6,
        "each system item is its own block, got {body}"
    );
    assert_eq!(
        system_ttl(&body, 0),
        Some("1h"),
        "first system must carry 1h, got {body}"
    );
    for i in 1..6 {
        assert!(
            !system_has_cache_control(&body, i),
            "system fragment {i} must not get cache_control, got {body}"
        );
    }
    let n = count_cache_control(&body);
    assert!(
        n <= ANTHROPIC_MAX_CACHE_CONTROL,
        "must not exceed Anthropic max of {ANTHROPIC_MAX_CACHE_CONTROL}, got {n} in {body}"
    );
    assert!(
        n <= ANTHROPIC_PREFERRED_CACHE_CONTROL,
        "prefer at most {ANTHROPIC_PREFERRED_CACHE_CONTROL} breakpoints, got {n} in {body}"
    );
    assert_eq!(n, 2, "first system + first user, got {body}");
    assert!(
        body.pointer("/messages/0/content/0/cache_control")
            .is_some(),
        "first user must be tagged, got {body}"
    );
}

#[test]
fn multi_fragment_long_ttl_with_tools_tags_first_system_not_last() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::System {
                text: "You are a coding agent.".into(),
            },
            IrItem::System {
                text: "Restored goal state from previous session:\nGoal: current task".into(),
            },
            IrItem::User {
                parts: vec![IrPart::Text("continue the work".into())],
            },
        ],
        tools: vec![
            IrTool::Function {
                name: "one".into(),
                description: "a".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            IrTool::Function {
                name: "two".into(),
                description: "b".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: Some("1h".into()),
                ..Default::default()
            },
            ..IrSampling::default()
        },
    };
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let tools = body.get("tools").and_then(Value::as_array).expect("tools");
    assert!(tools[0].get("cache_control").is_none());
    assert_eq!(
        tools[1]
            .pointer("/cache_control/ttl")
            .and_then(Value::as_str),
        Some("1h")
    );
    assert_eq!(
        system_ttl(&body, 0),
        Some("1h"),
        "first (static) system must hold long TTL, got {body}"
    );
    assert!(
        !system_has_cache_control(&body, 1),
        "resume goal fragment must not hold 1h after main system, got {body}"
    );
    assert_eq!(count_cache_control(&body), 2, "got {body}");
}

#[test]
fn short_ttl_multi_system_tags_last_system_and_first_user() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::System {
                text: "static".into(),
            },
            IrItem::System {
                text: "resume".into(),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
        tools: vec![],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: None,
                ..Default::default()
            },
            ..IrSampling::default()
        },
    };
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        !system_has_cache_control(&body, 0),
        "short TTL must not tag first system, got {body}"
    );
    assert!(
        system_has_cache_control(&body, 1),
        "short TTL tags last system, got {body}"
    );
    assert!(
        body.pointer("/messages/0/content/0/cache_control")
            .is_some(),
        "first user tagged, got {body}"
    );
    assert_eq!(count_cache_control(&body), 2, "got {body}");
}

#[test]
fn encode_after_decode_strips_six_cache_markers_to_preferred_pair() {
    let req = br#"{
        "model": "claude-opus-4-6",
        "system": [
            { "type": "text", "text": "a", "cache_control": { "type": "ephemeral" } },
            { "type": "text", "text": "b", "cache_control": { "type": "ephemeral" } },
            { "type": "text", "text": "c", "cache_control": { "type": "ephemeral" } }
        ],
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "u1", "cache_control": { "type": "ephemeral" } }] },
            { "role": "assistant", "content": [{ "type": "text", "text": "a1", "cache_control": { "type": "ephemeral" } }] },
            { "role": "user", "content": [{ "type": "text", "text": "u2", "cache_control": { "type": "ephemeral" } }] }
        ]
    }"#;
    let incoming: Value = serde_json::from_slice(req).expect("fixture");
    assert_eq!(
        count_cache_control(&incoming),
        6,
        "fixture starts with 6 markers"
    );
    let (ir, _) = decode(Wire::Messages, req).expect("decode");
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let n = count_cache_control(&body);
    assert!(
        n <= ANTHROPIC_MAX_CACHE_CONTROL,
        "hard cap 4, got {n} in {body}"
    );
    assert_eq!(n, 2, "preferred pair after remap, got {body}");
    assert!(
        system_has_cache_control(&body, 2),
        "short TTL keeps last system, got {body}"
    );
    assert!(
        !system_has_cache_control(&body, 0),
        "first system must be stripped on short TTL, got {body}"
    );
    assert!(
        body.pointer("/messages/0/content/0/cache_control")
            .is_some(),
        "first user kept, got {body}"
    );
}

fn cached_messages_ir(text: &str, floor: Option<u32>) -> IrRequest {
    IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::System { text: text.into() },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
        tools: vec![],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: None,
                min_cacheable_tokens: floor,
            },
            ..IrSampling::default()
        },
    }
}

#[test]
fn short_prompt_below_min_cacheable_tokens_drops_cache_control() {
    let ir = cached_messages_ir("rules", Some(1024));
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(count_cache_control(&body), 0, "short prompt, got {body}");
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.cache"
                && event.action == LossAction::Drop
                && event.detail.contains("1024")
        }),
        "must Drop sampling.cache naming the floor, got {report:?}"
    );
}

#[test]
fn long_prompt_at_min_cacheable_tokens_still_tags() {
    let ir = cached_messages_ir(&"x".repeat(5000), Some(1024));
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        count_cache_control(&body) > 0,
        "long prompt must still tag, got {body}"
    );
}

#[test]
fn min_cacheable_tokens_zero_does_not_skip() {
    let ir = cached_messages_ir("rules", Some(0));
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        count_cache_control(&body) > 0,
        "floor 0 must still tag, got {body}"
    );
}

#[test]
fn long_developer_at_min_cacheable_tokens_still_tags() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::Developer {
                text: "x".repeat(5000),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
        tools: vec![],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: None,
                min_cacheable_tokens: Some(1024),
            },
            ..IrSampling::default()
        },
    };
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        count_cache_control(&body) > 0,
        "long developer must still tag, got {body}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.cache" && event.action == LossAction::Preserve
        }),
        "must Preserve sampling.cache, got {report:?}"
    );
    assert!(
        !report
            .events
            .iter()
            .any(|event| { event.path == "sampling.cache" && event.action == LossAction::Drop }),
        "must not Drop sampling.cache, got {report:?}"
    );
    let (decoded, _) = decode(Wire::Messages, &bytes).expect("decode");
    assert_eq!(
        decoded.sampling.cache.min_cacheable_tokens, None,
        "floor is host policy, not a wire field"
    );
}

#[test]
fn large_function_output_at_min_cacheable_tokens_still_tags() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::System {
                text: "rules".into(),
            },
            IrItem::FunctionOutput {
                call_id: "c1".into(),
                output: "x".repeat(5000),
            },
        ],
        tools: vec![],
        sampling: IrSampling {
            cache: IrCache {
                enabled: true,
                retention: None,
                min_cacheable_tokens: Some(1024),
            },
            ..IrSampling::default()
        },
    };
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        count_cache_control(&body) > 0,
        "large tool result must still tag, got {body}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.cache" && event.action == LossAction::Preserve
        }),
        "must Preserve sampling.cache, got {report:?}"
    );
    let (decoded, _) = decode(Wire::Messages, &bytes).expect("decode");
    assert_eq!(
        decoded.sampling.cache.min_cacheable_tokens, None,
        "floor is host policy, not a wire field"
    );
}

fn user_ir(sampling: IrSampling) -> IrRequest {
    IrRequest {
        model: "gpt-4".into(),
        items: vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
        tools: vec![],
        sampling,
    }
}

fn loss_dropped(report: &LossReport, path: &str) -> bool {
    report
        .events
        .iter()
        .any(|event| event.path == path && event.action == LossAction::Drop)
}

fn loss_degraded(report: &LossReport, path: &str) -> bool {
    report
        .events
        .iter()
        .any(|event| event.path == path && event.action == LossAction::Degrade)
}

#[test]
fn chat_encode_emits_reasoning_effort_when_set() {
    let ir = user_ir(IrSampling {
        reasoning_effort: Some("high".into()),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("reasoning_effort").and_then(Value::as_str),
        Some("high"),
        "Chat must emit reasoning_effort, got {body}"
    );
    assert!(
        body.get("reasoning").is_none(),
        "Chat must not invent a reasoning object, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.reasoning_effort"),
        "effort has a Chat slot, got {report:?}"
    );
}

#[test]
fn chat_encode_does_not_invent_reasoning_when_unset() {
    let ir = user_ir(IrSampling::default());
    let (bytes, _) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("reasoning_effort").is_none(),
        "unset effort must omit the field, got {body}"
    );
    assert!(
        body.get("reasoning").is_none(),
        "unset effort must not invent reasoning, got {body}"
    );
}

#[test]
fn chat_decode_reads_reasoning_effort() {
    let req = br#"{
        "model": "o4-mini",
        "reasoning_effort": "low",
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, req).expect("decode");
    assert_eq!(ir.sampling.reasoning_effort.as_deref(), Some("low"));
}

#[test]
fn responses_decode_reads_reasoning_effort() {
    let req = br#"{
        "model": "gpt-5",
        "input": "hi",
        "reasoning": { "effort": "high" }
    }"#;
    let (ir, _) = decode(Wire::Responses, req).expect("decode");
    assert_eq!(ir.sampling.reasoning_effort.as_deref(), Some("high"));
}

#[test]
fn chat_decode_reads_max_reasoning_tokens() {
    let req = br#"{
        "model": "o4-mini",
        "max_reasoning_tokens": 2048,
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, req).expect("decode");
    assert_eq!(ir.sampling.max_reasoning_tokens, Some(2048));
}

#[test]
fn chat_decode_empty_reasoning_effort_is_unset() {
    for effort in ["", "  \t"] {
        let req = format!(
            r#"{{
                "model": "o4-mini",
                "reasoning_effort": {effort},
                "messages": [{{"role": "user", "content": "hi"}}]
            }}"#,
            effort = serde_json::to_string(effort).expect("json")
        );
        let (ir, _) = decode(Wire::ChatCompletions, req.as_bytes()).expect("decode");
        assert_eq!(
            ir.sampling.reasoning_effort, None,
            "empty or whitespace-only effort must be unset, got {:?} for {effort:?}",
            ir.sampling.reasoning_effort
        );
    }
}

#[test]
fn chat_drops_max_reasoning_tokens() {
    let ir = user_ir(IrSampling {
        max_reasoning_tokens: Some(2048),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("max_reasoning_tokens").is_none(),
        "Chat has no max_reasoning_tokens slot, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.max_reasoning_tokens"),
        "must record drop, got {report:?}"
    );
}

#[test]
fn responses_encode_drops_max_reasoning_tokens() {
    let ir = user_ir(IrSampling {
        max_reasoning_tokens: Some(2048),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("reasoning").is_none(),
        "max alone must not invent reasoning, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.max_reasoning_tokens"),
        "must record drop, got {report:?}"
    );
}

#[test]
fn responses_encode_folds_effort_into_reasoning() {
    let ir = user_ir(IrSampling {
        reasoning_effort: Some("high".into()),
        ..IrSampling::default()
    });
    let (bytes, _) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/reasoning/effort").and_then(Value::as_str),
        Some("high"),
        "Responses must fold effort into reasoning, got {body}"
    );
    assert_eq!(
        body.get("include"),
        Some(&serde_json::json!(["reasoning.encrypted_content"])),
        "include must stay, got {body}"
    );
}

#[test]
fn responses_encode_does_not_invent_reasoning_object_when_unset() {
    let ir = user_ir(IrSampling::default());
    let (bytes, _) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("reasoning").is_none(),
        "unset effort must not invent reasoning, got {body}"
    );
}

#[test]
fn responses_encode_does_not_invent_reasoning_for_empty_effort() {
    for effort in ["", "  \t"] {
        let ir = user_ir(IrSampling {
            reasoning_effort: Some(effort.into()),
            ..IrSampling::default()
        });
        let (bytes, _) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(
            body.get("reasoning").is_none(),
            "empty or whitespace-only effort must not invent reasoning, got {body}"
        );
    }
}

#[test]
fn messages_encode_records_loss_for_reasoning_sampling() {
    let ir = user_ir(IrSampling {
        reasoning_effort: Some("high".into()),
        max_reasoning_tokens: Some(4096),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("reasoning_effort").is_none(),
        "Messages must not emit Chat-only effort, got {body}"
    );
    assert!(
        body.get("reasoning").is_none(),
        "Messages must not invent reasoning, got {body}"
    );
    assert!(
        body.get("thinking").is_none(),
        "must not invent thinking from effort, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.reasoning_effort"),
        "effort drop missing, got {report:?}"
    );
    assert!(
        loss_dropped(&report, "sampling.max_reasoning_tokens"),
        "max drop missing, got {report:?}"
    );
}

#[test]
fn gemini_thinking_config_survives_reasoning_sampling_fields() {
    let ir = user_ir(IrSampling {
        include_thoughts: Some(true),
        thinking_budget: Some(24576),
        reasoning_effort: Some("high".into()),
        max_reasoning_tokens: Some(1024),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let tc = body
        .pointer("/generationConfig/thinkingConfig")
        .expect("thinkingConfig should be nested under generationConfig");
    assert_eq!(tc.get("includeThoughts"), Some(&Value::Bool(true)));
    assert_eq!(tc.get("thinkingBudget"), Some(&serde_json::json!(24576)));
    assert!(
        body.get("thinkingConfig").is_none(),
        "must not emit top-level thinkingConfig: {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.reasoning_effort"),
        "Gemini effort drop missing, got {report:?}"
    );
    assert!(
        loss_dropped(&report, "sampling.max_reasoning_tokens"),
        "Gemini max drop missing, got {report:?}"
    );
}

#[test]
fn gemini_thinking_ir_drops_on_chat_and_messages() {
    let ir = user_ir(IrSampling {
        include_thoughts: Some(true),
        thinking_budget: Some(24576),
        ..IrSampling::default()
    });
    for (wire, profile) in [
        (Wire::ChatCompletions, chat_profile()),
        (Wire::Messages, messages_profile()),
    ] {
        let (bytes, report) = encode(wire, &ir, &profile).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(
            body.get("thinkingConfig").is_none(),
            "{wire:?} must not invent thinkingConfig, got {body}"
        );
        assert!(
            body.get("include_thoughts").is_none() && body.get("includeThoughts").is_none(),
            "{wire:?} must not invent include_thoughts, got {body}"
        );
        assert!(
            body.get("thinking_budget").is_none() && body.get("thinkingBudget").is_none(),
            "{wire:?} must not invent thinking_budget, got {body}"
        );
        assert!(
            loss_dropped(&report, "sampling.include_thoughts"),
            "{wire:?} include_thoughts drop missing, got {report:?}"
        );
        assert!(
            loss_dropped(&report, "sampling.thinking_budget"),
            "{wire:?} thinking_budget drop missing, got {report:?}"
        );
    }
}

#[test]
fn chat_required_tool_choice_drops_on_gemini() {
    let ir = user_ir(IrSampling {
        tool_choice: IrToolChoice::Required,
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("tool_choice").is_none(),
        "Gemini must not invent tool_choice, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.tool_choice"),
        "Gemini tool_choice drop missing, got {report:?}"
    );
}

#[test]
fn chat_grouped_function_call_records_thought_signature_drop() {
    let ir = IrRequest {
        model: "gpt-4".into(),
        items: vec![
            IrItem::Assistant {
                parts: vec![IrPart::Text("calling".into())],
            },
            IrItem::FunctionCall {
                call_id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{}".into(),
                thought_signature: Some("sig_grouped".into()),
            },
        ],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (_, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    assert!(
        report.events.iter().any(|event| {
            event.action == LossAction::Drop
                && event.detail == "thoughtSignature has no Chat Completions slot"
        }),
        "grouped FunctionCall must record the same thought_signature Drop as standalone, got {report:?}"
    );
}

#[test]
fn gemini_encode_does_not_invent_empty_function_call_args() {
    let ir = IrRequest {
        model: "gemini-2.5-flash".into(),
        items: vec![IrItem::FunctionCall {
            call_id: "lookup".into(),
            name: "lookup".into(),
            arguments: "not-json".into(),
            thought_signature: None,
        }],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let args = body.pointer("/contents/0/parts/0/functionCall/args");
    assert_ne!(
        args,
        Some(&serde_json::json!({})),
        "invalid JSON must not become empty object, got {body}"
    );
    assert_eq!(
        args,
        Some(&Value::String("not-json".into())),
        "invalid JSON must stay a string, got {body}"
    );
}

#[test]
fn messages_sanitizes_gemini_shaped_tool_use_id() {
    let ir = IrRequest {
        model: "claude-opus-4-6".into(),
        items: vec![
            IrItem::FunctionCall {
                call_id: "lookup.v2".into(),
                name: "lookup.v2".into(),
                arguments: "{}".into(),
                thought_signature: None,
            },
            IrItem::FunctionOutput {
                call_id: "lookup.v2".into(),
                output: "ok".into(),
            },
        ],
        tools: vec![],
        sampling: IrSampling::default(),
    };
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/messages/0/content/0/id")
            .and_then(Value::as_str),
        Some("lookup_v2"),
        "tool_use.id must drop the Gemini dot, got {body}"
    );
    assert_eq!(
        body.pointer("/messages/1/content/0/tool_use_id")
            .and_then(Value::as_str),
        Some("lookup_v2"),
        "tool_result.tool_use_id must match the rewritten tool_use.id, got {body}"
    );
    assert!(
        loss_degraded(&report, "items[0]"),
        "rewritten tool_use.id must record Degrade on the original path, got {report:?}"
    );
    assert!(
        loss_degraded(&report, "items[1]"),
        "rewritten tool_result.tool_use_id must record Degrade on the original path, got {report:?}"
    );
}

#[test]
fn chat_json_schema_round_trips() {
    let req = br#"{
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi"}],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "answer",
                "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
            }
        }
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, req).expect("decode");
    assert_eq!(ir.sampling.json_schema_name.as_deref(), Some("answer"));
    assert_eq!(
        ir.sampling.json_schema,
        Some(serde_json::json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}))
    );
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/response_format/type")
            .and_then(Value::as_str),
        Some("json_schema"),
        "Chat must emit response_format.type, got {body}"
    );
    assert_eq!(
        body.pointer("/response_format/json_schema/name")
            .and_then(Value::as_str),
        Some("answer"),
        "Chat must emit json_schema.name, got {body}"
    );
    assert_eq!(
        body.pointer("/response_format/json_schema/schema"),
        Some(&serde_json::json!({"type": "object", "properties": {"ok": {"type": "boolean"}}})),
        "Chat must emit json_schema.schema, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.json_schema"),
        "Chat has a slot and must not Drop json_schema, got {report:?}"
    );
}

#[test]
fn responses_json_schema_round_trips() {
    let req = br#"{
        "model": "gpt-4",
        "input": "hi",
        "text": {
            "format": {
                "type": "json_schema",
                "name": "answer",
                "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
            }
        }
    }"#;
    let (ir, _) = decode(Wire::Responses, req).expect("decode");
    assert_eq!(ir.sampling.json_schema_name.as_deref(), Some("answer"));
    assert_eq!(
        ir.sampling.json_schema,
        Some(serde_json::json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}))
    );
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/text/format/type").and_then(Value::as_str),
        Some("json_schema"),
        "Responses must emit text.format.type, got {body}"
    );
    assert_eq!(
        body.pointer("/text/format/name").and_then(Value::as_str),
        Some("answer"),
        "Responses must emit text.format.name, got {body}"
    );
    assert_eq!(
        body.pointer("/text/format/schema"),
        Some(&serde_json::json!({"type": "object", "properties": {"ok": {"type": "boolean"}}})),
        "Responses must emit text.format.schema, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.json_schema"),
        "Responses has a slot and must not Drop json_schema, got {report:?}"
    );
}

#[test]
fn chat_json_schema_without_name_is_dropped() {
    let ir = user_ir(IrSampling {
        json_schema: Some(serde_json::json!({"type": "object"})),
        json_schema_name: None,
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("response_format").is_none(),
        "nameless json_schema must not emit response_format, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.json_schema"),
        "nameless json_schema must Drop, got {report:?}"
    );

    let ir = user_ir(IrSampling {
        json_schema: Some(serde_json::json!(["not", "object"])),
        json_schema_name: Some("answer".into()),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("response_format").is_none(),
        "non-object json_schema must not emit response_format, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.json_schema"),
        "non-object json_schema must Drop, got {report:?}"
    );
}

#[test]
fn responses_json_schema_without_name_is_dropped() {
    let ir = user_ir(IrSampling {
        json_schema: Some(serde_json::json!({"type": "object"})),
        json_schema_name: None,
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("text").is_none(),
        "nameless json_schema must not emit text.format, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.json_schema"),
        "nameless json_schema must Drop, got {report:?}"
    );

    let ir = user_ir(IrSampling {
        json_schema: Some(serde_json::json!("not-object")),
        json_schema_name: Some("answer".into()),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("text").is_none(),
        "non-object json_schema must not emit text.format, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.json_schema"),
        "non-object json_schema must Drop, got {report:?}"
    );
}

#[test]
fn messages_json_schema_is_dropped() {
    let ir = user_ir(IrSampling {
        json_schema: Some(serde_json::json!({"type": "object"})),
        json_schema_name: Some("answer".into()),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("response_format").is_none() && body.get("output_format").is_none(),
        "Messages must not invent a structured-output slot, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.json_schema"),
        "Messages must Drop json_schema with no slot, got {report:?}"
    );
}

#[test]
fn gemini_json_schema_round_trips() {
    let req = br#"{
        "model": "gemini-2.5-pro",
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseSchema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
        }
    }"#;
    let (ir, decode_report) = decode(Wire::Gemini, req).expect("decode");
    assert_eq!(
        ir.sampling.json_schema,
        Some(serde_json::json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}))
    );
    assert!(
        !loss_dropped(&decode_report, "sampling.json_schema"),
        "Gemini has responseSchema and must not Drop on decode, got {decode_report:?}"
    );
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/generationConfig/responseMimeType")
            .and_then(Value::as_str),
        Some("application/json"),
        "Gemini must emit responseMimeType, got {body}"
    );
    assert_eq!(
        body.pointer("/generationConfig/responseSchema"),
        Some(&serde_json::json!({"type": "object", "properties": {"ok": {"type": "boolean"}}})),
        "Gemini must emit responseSchema, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.json_schema"),
        "Gemini has a slot and must not Drop json_schema, got {report:?}"
    );
}

#[test]
fn gemini_nameless_json_schema_still_encodes() {
    let ir = user_ir(IrSampling {
        json_schema: Some(
            serde_json::json!({"type": "object", "properties": {"n": {"type": "number"}}}),
        ),
        json_schema_name: None,
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/generationConfig/responseSchema"),
        Some(&serde_json::json!({"type": "object", "properties": {"n": {"type": "number"}}})),
        "Gemini has no name slot; object schema must still encode, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.json_schema"),
        "nameless object schema must not Drop on Gemini, got {report:?}"
    );
}

#[test]
fn gemini_non_object_json_schema_is_dropped() {
    let ir = user_ir(IrSampling {
        json_schema: Some(serde_json::json!(["not", "object"])),
        json_schema_name: Some("answer".into()),
        ..IrSampling::default()
    });
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.pointer("/generationConfig/responseSchema").is_none(),
        "non-object json_schema must not emit responseSchema, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.json_schema"),
        "non-object json_schema must Drop, got {report:?}"
    );
    assert!(
        !report.events.iter().any(|event| {
            event.path == "sampling.json_schema"
                && event.action == LossAction::Drop
                && event.detail == "no slot"
        }),
        "Gemini has a slot; Drop detail must not be no slot, got {report:?}"
    );
}

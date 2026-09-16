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
fn chat_encode_emits_store_true() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.store = Some(true);
    }));
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("store"),
        Some(&Value::Bool(true)),
        "Chat must emit store=true, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.store"),
        "Chat store must not Drop as no slot, got {report:?}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.store" && event.action == LossAction::Preserve
        }),
        "expected Preserve store, got {report:?}"
    );
}

#[test]
fn chat_encode_emits_store_false() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.store = Some(false);
    }));
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("store"),
        Some(&Value::Bool(false)),
        "Chat must emit store=false, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.store"),
        "Chat store=false must not Drop, got {report:?}"
    );
}

#[test]
fn chat_decode_reads_store_true() {
    let req = br#"{
        "model": "gpt-5",
        "store": true,
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, req).expect("decode");
    assert_eq!(ir.sampling.store, Some(true));
}

#[test]
fn chat_store_forbidden_is_hard_error() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.store = Some(true);
    }));
    let err = encode(Wire::ChatCompletions, &ir, &openrouter_forbid_store())
        .expect_err("OpenRouter forbidden store must hard-error on Chat");
    match err {
        MapError::HardError { path, detail } => {
            assert!(path.contains("store"), "path={path}");
            assert!(detail.contains("store"), "detail={detail}");
        }
        other => panic!("expected HardError, got {other}"),
    }
}

#[test]
fn messages_and_gemini_do_not_invent_store() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.store = Some(true);
    }));
    let (msg_bytes, msg_report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let msg: Value = serde_json::from_slice(&msg_bytes).expect("json");
    assert!(
        msg.get("store").is_none(),
        "Messages must not invent store, got {msg}"
    );
    assert!(
        loss_dropped(&msg_report, "sampling.store"),
        "Messages store drop missing, got {msg_report:?}"
    );
    let (gem_bytes, gem_report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let gem: Value = serde_json::from_slice(&gem_bytes).expect("json");
    assert!(
        gem.get("store").is_none(),
        "Gemini must not invent store, got {gem}"
    );
    assert!(
        loss_dropped(&gem_report, "sampling.store"),
        "Gemini store drop missing, got {gem_report:?}"
    );
}

#[test]
fn responses_and_chat_round_trip_codex_cache_key_and_tier() {
    let req = br#"{
        "model": "gpt-5",
        "prompt_cache_key": "sess-1",
        "service_tier": "flex",
        "input": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::Responses, req).expect("decode responses");
    assert_eq!(ir.sampling.prompt_cache_key.as_deref(), Some("sess-1"));
    assert_eq!(ir.sampling.service_tier.as_deref(), Some("flex"));

    let (bytes, report) = encode(Wire::Responses, &ir, &hard_error_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["prompt_cache_key"], "sess-1");
    assert_eq!(body["service_tier"], "flex");
    assert!(
        !loss_dropped(&report, "sampling.prompt_cache_key"),
        "Responses prompt_cache_key must not Drop, got {report:?}"
    );
    assert!(
        !loss_dropped(&report, "sampling.service_tier"),
        "Responses service_tier must not Drop, got {report:?}"
    );

    let chat_req = br#"{
        "model": "gpt-5",
        "prompt_cache_key": "sess-1",
        "service_tier": "priority",
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (chat_ir, _) = decode(Wire::ChatCompletions, chat_req).expect("decode chat");
    assert_eq!(chat_ir.sampling.prompt_cache_key.as_deref(), Some("sess-1"));
    assert_eq!(chat_ir.sampling.service_tier.as_deref(), Some("priority"));
    let (chat_bytes, chat_report) =
        encode(Wire::ChatCompletions, &chat_ir, &chat_profile()).expect("encode chat");
    let chat: Value = serde_json::from_slice(&chat_bytes).expect("json");
    assert_eq!(chat["prompt_cache_key"], "sess-1");
    assert_eq!(chat["service_tier"], "priority");
    assert!(
        !loss_dropped(&chat_report, "sampling.prompt_cache_key"),
        "Chat prompt_cache_key must not Drop, got {chat_report:?}"
    );
}

#[test]
fn messages_and_gemini_drop_codex_cache_key_and_tier() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.prompt_cache_key = Some("sess-1".into());
        s.service_tier = Some("flex".into());
    }));
    let (msg_bytes, msg_report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let msg: Value = serde_json::from_slice(&msg_bytes).expect("json");
    assert!(
        msg.get("prompt_cache_key").is_none() && msg.get("service_tier").is_none(),
        "Messages must not invent Codex cache/tier, got {msg}"
    );
    assert!(
        loss_dropped(&msg_report, "sampling.prompt_cache_key"),
        "Messages prompt_cache_key drop missing, got {msg_report:?}"
    );
    assert!(
        loss_dropped(&msg_report, "sampling.service_tier"),
        "Messages service_tier drop missing, got {msg_report:?}"
    );
    let (gem_bytes, gem_report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let gem: Value = serde_json::from_slice(&gem_bytes).expect("json");
    assert!(
        gem.get("prompt_cache_key").is_none() && gem.get("service_tier").is_none(),
        "Gemini must not invent Codex cache/tier, got {gem}"
    );
    assert!(
        loss_dropped(&gem_report, "sampling.prompt_cache_key"),
        "Gemini prompt_cache_key drop missing, got {gem_report:?}"
    );
    assert!(
        loss_dropped(&gem_report, "sampling.service_tier"),
        "Gemini service_tier drop missing, got {gem_report:?}"
    );
    let (cv_bytes, cv_report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let cv: Value = serde_json::from_slice(&cv_bytes).expect("json");
    assert!(
        cv.get("prompt_cache_key").is_none() && cv.get("service_tier").is_none(),
        "Converse must not invent Codex cache/tier keys, got {cv}"
    );
    assert!(
        loss_dropped(&cv_report, "sampling.prompt_cache_key"),
        "Converse prompt_cache_key drop missing, got {cv_report:?}"
    );
    assert_eq!(
        cv.pointer("/serviceTier/type").and_then(Value::as_str),
        Some("flex"),
        "Converse flex must emit serviceTier.type, got {cv}"
    );
    assert!(
        !loss_dropped(&cv_report, "sampling.service_tier"),
        "Converse has serviceTier; flex must not Drop, got {cv_report:?}"
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
fn gemini_reasoning_summary_encodes_as_thought_part() {
    let ir = IrRequest::new(
        "gemini-2.5-flash",
        vec![
            IrItem::User {
                parts: vec![IrPart::Text("hi".into())],
            },
            IrItem::Reasoning {
                encrypted: Some("enc-openai-not-gemini".into()),
                summary: Some("I should greet them".into()),
                raw: None,
            },
        ],
    );
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let parts: Vec<&Value> = body
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("parts").and_then(Value::as_array))
        .flatten()
        .collect();
    let thought = parts.iter().find(|p| {
        p.get("thought").and_then(Value::as_bool) == Some(true)
            && p.get("text").and_then(Value::as_str) == Some("I should greet them")
    });
    assert!(
        thought.is_some(),
        "Reasoning summary must encode as a thought part, got {body}"
    );
    assert!(
        thought
            .and_then(|p| p.get("thoughtSignature"))
            .and_then(Value::as_str)
            != Some("enc-openai-not-gemini"),
        "encrypted_content must not become thoughtSignature, got {body}"
    );
    assert!(
        !report.events.iter().any(|event| {
            event.action == LossAction::Drop && event.detail.contains("no generateContent slot")
        }),
        "summary remaps to a thought part, must not Drop as no slot, got {report:?}"
    );
}

#[test]
fn gemini_reasoning_without_summary_omits_thought() {
    let ir = IrRequest::new(
        "gemini-2.5-flash",
        vec![IrItem::Reasoning {
            encrypted: Some("enc-only".into()),
            summary: None,
            raw: None,
        }],
    );
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let has_thought = body
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("parts").and_then(Value::as_array))
        .flatten()
        .any(|p| p.get("thought").and_then(Value::as_bool) == Some(true));
    assert!(
        !has_thought,
        "encrypted-only Reasoning must not invent a thought part, got {body}"
    );
    assert!(
        !body.to_string().contains("enc-only"),
        "encrypted_content must not become thoughtSignature, got {body}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.action == LossAction::Drop
                && event
                    .detail
                    .contains("reasoning omitted on generateContent")
        }),
        "empty Reasoning must Drop naming generateContent, got {report:?}"
    );
    assert!(
        !report.events.iter().any(|event| {
            event.action == LossAction::Drop && event.detail.contains("no generateContent slot")
        }),
        "must not claim no slot after remapping Reasoning, got {report:?}"
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
fn gemini_user_text_then_function_response_stays_user() {
    let req = br#"{
        "contents": [{
            "role": "user",
            "parts": [
                { "text": "here is the result" },
                { "functionResponse": { "name": "lookup", "response": { "ok": true } } }
            ]
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    assert!(
        matches!(
            ir.items.as_slice(),
            [
                IrItem::User { parts },
                IrItem::FunctionOutput { call_id, .. }
            ] if parts.iter().any(|p| matches!(p, IrPart::Text(t) if t == "here is the result"))
                && call_id == "lookup"
        ),
        "mixed user text+functionResponse must be User then FunctionOutput, got {:?}",
        ir.items
    );
    assert!(
        !ir.items
            .iter()
            .any(|item| matches!(item, IrItem::Assistant { .. })),
        "user-role pending text must not flush as Assistant, got {:?}",
        ir.items
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
    let ir = wiremux::IrRequest::new(
        "gemini-2.5-flash",
        vec![
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
    )
    .with_sampling(wiremux::IrSampling::default());
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
    assert!(
        ir.items.iter().any(|item| matches!(
            item,
            IrItem::Assistant { parts } if parts.iter().any(|part| matches!(
                part,
                IrPart::Thinking { text, signature }
                    if text == "I should greet them"
                        && signature.as_deref() == Some("sig_abc")
            ))
        )),
        "decode must produce IrPart::Thinking {{ text: \"I should greet them\", signature: Some(\"sig_abc\") }}, got {:?}",
        ir.items
    );
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![IrItem::Assistant {
            parts: vec![
                IrPart::Thinking {
                    text: "scratch".into(),
                    signature: None,
                },
                IrPart::Text("Hello".into()),
            ],
        }],
    );
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
        !body.to_string().contains("scratch"),
        "unsigned thinking text must not leak onto the wire, got {body}"
    );
    assert!(
        content
            .iter()
            .all(|block| block.get("text").and_then(Value::as_str) != Some("scratch")),
        "unsigned thinking must not leak as a text block, got {body}"
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
    let ir = IrRequest::new(
        "grok-4",
        vec![IrItem::Assistant {
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
    );
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
    let ir = IrRequest::new(
        "gpt-5",
        vec![IrItem::Assistant {
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
    );
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
        dumped.contains("redacted_thinking"),
        "Raw part must be replayed as a content part, got {body}"
    );
    assert!(
        loss_dropped(&report, "part.thinking"),
        "thinking drop missing, got {report:?}"
    );
    assert!(
        !loss_dropped(&report, "part.raw"),
        "Raw must not be dropped, got {report:?}"
    );
}

#[test]
fn replay_thinking_signed_encodes_as_responses_reasoning() {
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
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let input = body
        .get("input")
        .and_then(Value::as_array)
        .expect("Responses input");
    assert!(
        input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("reasoning")
                && item.get("encrypted_content").and_then(Value::as_str) == Some("sig_abc")
        }),
        "signed thinking must encode as a sibling reasoning item, got {body}"
    );
    assert!(
        !loss_dropped(&report, "part.thinking"),
        "signed thinking must not Drop, got {report:?}"
    );
    assert!(
        report
            .events
            .iter()
            .any(|event| { event.path == "part.thinking" && event.action == LossAction::Preserve }),
        "signed thinking remapped to reasoning must Preserve, got {report:?}"
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![IrItem::Assistant {
            parts: vec![IrPart::Text("\n".into())],
        }],
    );
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
    let ir = IrRequest::new(
        "gemini-2.5-flash",
        vec![IrItem::User {
            parts: vec![
                IrPart::Text("see".into()),
                IrPart::ImageUrl("https://example.com/cat.png".into()),
            ],
        }],
    );
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
    let ir = IrRequest::new(
        "gemini-2.5-flash",
        vec![IrItem::User {
            parts: vec![
                IrPart::Text("hi".into()),
                IrPart::Raw {
                    type_name: "redacted_thinking".into(),
                    raw: serde_json::json!({"type": "redacted_thinking", "data": "enc"}),
                },
            ],
        }],
    );
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
    let ir = IrRequest::new(
        "gemini-2.5-flash",
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    )
    .with_tools(vec![
        IrTool::Function {
            name: "lookup".into(),
            description: "Look up".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        IrTool::Unknown {
            type_name: "weird".into(),
            raw: serde_json::json!({"type": "weird", "name": "do_thing"}),
        },
    ]);
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

#[test]
fn gemini_hosted_google_search_tool_is_not_dropped() {
    let bytes = br#"{
        "model": "gemini-2.5-flash",
        "contents": [{"role": "user", "parts": [{"text": "search"}]}],
        "tools": [{"googleSearch": {}}]
    }"#;
    let (ir, report) = decode(Wire::Gemini, bytes).expect("decode");
    assert!(
        !ir.tools.is_empty(),
        "googleSearch must not vanish, tools={:?} report={report:?}",
        ir.tools
    );
    assert!(
        ir.tools.iter().any(|tool| match tool {
            IrTool::Unknown { type_name, .. } => type_name == "googleSearch",
            IrTool::Hosted { kind, .. } => kind == "googleSearch",
            _ => false,
        }),
        "googleSearch must be Unknown or Hosted, got {:?}",
        ir.tools
    );
}

#[test]
fn gemini_mixed_function_declarations_and_google_search() {
    let bytes = br#"{
        "model": "gemini-2.5-flash",
        "contents": [{"role": "user", "parts": [{"text": "search"}]}],
        "tools": [{
            "functionDeclarations": [{"name": "lookup"}],
            "googleSearch": {}
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, bytes).expect("decode");
    assert!(
        ir.tools
            .iter()
            .any(|tool| matches!(tool, IrTool::Function { name, .. } if name == "lookup")),
        "lookup Function missing, got {:?}",
        ir.tools
    );
    assert!(
        ir.tools.iter().any(|tool| match tool {
            IrTool::Unknown { type_name, .. } => type_name == "googleSearch",
            IrTool::Hosted { kind, .. } => kind == "googleSearch",
            _ => false,
        }),
        "googleSearch must decode beside functionDeclarations, got {:?}",
        ir.tools
    );
}

#[test]
fn gemini_hosted_google_search_passthrough_encodes() {
    let bytes = br#"{
        "model": "gemini-2.5-flash",
        "contents": [{"role": "user", "parts": [{"text": "search"}]}],
        "tools": [{"googleSearch": {}}]
    }"#;
    let (ir, _) = decode(Wire::Gemini, bytes).expect("decode");
    let passthrough = profile(
        r#"
schema_version = 1
id = "test-gemini-hosted-passthrough"
wire = "gemini"
tool_type_policy = "passthrough"
"#,
    );
    let (out, report) = encode(Wire::Gemini, &ir, &passthrough).expect("encode");
    let body: Value = serde_json::from_slice(&out).expect("json");
    let tools = body.get("tools").and_then(Value::as_array);
    assert!(
        tools.is_some_and(|tools| tools.iter().any(|tool| tool.get("googleSearch").is_some())),
        "passthrough encode must emit googleSearch, got {body} report={report:?}"
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
            IrItem::System {
                text: "rules".into(),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello world".into())],
            },
        ],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: None,
            ..Default::default()
        };
    }));
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
            IrItem::System {
                text: "rules".into(),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
    )
    .with_tools(vec![
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
    ])
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: Some("1h".into()),
            ..Default::default()
        };
    }));
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
            IrItem::System {
                text: "rules".into(),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
    )
    .with_tools(vec![IrTool::Function {
        name: "one".into(),
        description: "a".into(),
        parameters: serde_json::json!({"type": "object"}),
    }]);
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(count_cache_control(&body), 0, "got {body}");
}

#[test]
fn cache_retention_none_skips_cache_control() {
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![IrItem::System {
            text: "rules".into(),
        }],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: Some("none".into()),
            ..Default::default()
        };
    }));
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
    let ir = IrRequest::new("claude-opus-4-6", items).with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: Some("1h".into()),
            ..Default::default()
        };
    }));
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
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
    )
    .with_tools(vec![
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
    ])
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: Some("1h".into()),
            ..Default::default()
        };
    }));
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
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
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: None,
            ..Default::default()
        };
    }));
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
    IrRequest::new(
        "claude-opus-4-6",
        vec![
            IrItem::System { text: text.into() },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: None,
            min_cacheable_tokens: floor,
        };
    }))
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
            IrItem::Developer {
                text: "x".repeat(5000),
            },
            IrItem::User {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: None,
            min_cacheable_tokens: Some(1024),
        };
    }));
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
            IrItem::System {
                text: "rules".into(),
            },
            IrItem::FunctionOutput {
                call_id: "c1".into(),
                output: "x".repeat(5000),
            },
        ],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache {
            enabled: true,
            retention: None,
            min_cacheable_tokens: Some(1024),
        };
    }));
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
    IrRequest::new(
        "gpt-4",
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    )
    .with_sampling(sampling)
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.reasoning_effort = Some("high".into());
    }));
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.max_reasoning_tokens = Some(2048);
    }));
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.max_reasoning_tokens = Some(2048);
    }));
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.reasoning_effort = Some("high".into());
    }));
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
        let ir = user_ir(IrSampling::patch(|s| {
            s.reasoning_effort = Some(effort.into());
        }));
        let (bytes, _) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(
            body.get("reasoning").is_none(),
            "empty or whitespace-only effort must not invent reasoning, got {body}"
        );
    }
}

#[test]
fn responses_encode_include_thoughts_as_reasoning_summary_auto() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
    }));
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/reasoning/summary").and_then(Value::as_str),
        Some("auto"),
        "include_thoughts=true must emit reasoning.summary=auto, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.include_thoughts"),
        "include_thoughts has a Responses summary slot, got {report:?}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.include_thoughts" && event.action == LossAction::Preserve
        }),
        "expected Preserve include_thoughts, got {report:?}"
    );
}

#[test]
fn responses_encode_merges_summary_into_existing_reasoning() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
        s.reasoning_effort = Some("high".into());
    }));
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/reasoning/effort").and_then(Value::as_str),
        Some("high"),
        "must keep effort when merging summary, got {body}"
    );
    assert_eq!(
        body.pointer("/reasoning/summary").and_then(Value::as_str),
        Some("auto"),
        "must merge summary=auto into reasoning, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.include_thoughts"),
        "include_thoughts must Preserve, got {report:?}"
    );
}

#[test]
fn responses_encode_false_include_thoughts_does_not_invent_summary() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(false);
        s.reasoning_effort = Some("high".into());
    }));
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/reasoning/effort").and_then(Value::as_str),
        Some("high"),
        "effort must still emit, got {body}"
    );
    assert!(
        body.pointer("/reasoning/summary").is_none(),
        "include_thoughts=false must not invent summary, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.include_thoughts"),
        "include_thoughts=false has no summary dest, got {report:?}"
    );
}

#[test]
fn responses_decode_reasoning_summary_sets_include_thoughts() {
    let req = br#"{
        "model": "gpt-5",
        "input": "hi",
        "reasoning": { "summary": "auto" }
    }"#;
    let (ir, _) = decode(Wire::Responses, req).expect("decode");
    assert_eq!(ir.sampling.include_thoughts, Some(true));
}

#[test]
fn responses_decode_nonempty_reasoning_summary_sets_include_thoughts() {
    let req = br#"{
        "model": "gpt-5",
        "input": "hi",
        "reasoning": { "effort": "low", "summary": "detailed" }
    }"#;
    let (ir, _) = decode(Wire::Responses, req).expect("decode");
    assert_eq!(ir.sampling.include_thoughts, Some(true));
    assert_eq!(ir.sampling.reasoning_effort.as_deref(), Some("low"));
}

#[test]
fn responses_decode_empty_reasoning_summary_leaves_include_thoughts_unset() {
    for summary in ["", "  \t"] {
        let req = format!(
            r#"{{
                "model": "gpt-5",
                "input": "hi",
                "reasoning": {{ "summary": {summary} }}
            }}"#,
            summary = serde_json::to_string(summary).expect("json")
        );
        let (ir, _) = decode(Wire::Responses, req.as_bytes()).expect("decode");
        assert_eq!(
            ir.sampling.include_thoughts, None,
            "empty summary must not invent include_thoughts, got {:?} for {summary:?}",
            ir.sampling.include_thoughts
        );
    }
}

#[test]
fn messages_encode_emits_thinking_from_include_thoughts_and_budget() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
        s.max_reasoning_tokens = Some(2048);
        s.reasoning_effort = Some("high".into());
    }));
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/thinking/type").and_then(Value::as_str),
        Some("enabled"),
        "Messages must emit thinking, got {body}"
    );
    assert_eq!(
        body.pointer("/thinking/budget_tokens"),
        Some(&serde_json::json!(2048)),
        "max_reasoning_tokens wins budget_tokens, got {body}"
    );
    assert!(
        body.get("reasoning_effort").is_none(),
        "Messages must not emit Chat-only effort, got {body}"
    );
    assert!(
        !report.events.iter().any(|event| {
            event.path.starts_with("sampling.")
                && event.path.contains("reason")
                && event.action == LossAction::Drop
        }),
        "reasoning fields have a Messages slot, got {report:?}"
    );
}

#[test]
fn messages_encode_effort_high_maps_to_thinking_budget() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.reasoning_effort = Some("high".into());
    }));
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/thinking/type").and_then(Value::as_str),
        Some("enabled"),
        "effort must enable thinking, got {body}"
    );
    assert_eq!(
        body.pointer("/thinking/budget_tokens"),
        Some(&serde_json::json!(32768)),
        "high effort default budget, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.reasoning_effort"),
        "effort has a Messages slot, got {report:?}"
    );
}

#[test]
fn messages_encode_disables_thinking_when_include_thoughts_false() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(false);
        s.reasoning_effort = Some("high".into());
        s.max_reasoning_tokens = Some(2048);
    }));
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/thinking/type").and_then(Value::as_str),
        Some("disabled"),
        "explicit include_thoughts=false must disable, got {body}"
    );
    assert!(
        body.pointer("/thinking/budget_tokens").is_none(),
        "disabled thinking must not carry budget_tokens, got {body}"
    );
}

#[test]
fn messages_encode_raises_max_tokens_above_budget() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
        s.max_reasoning_tokens = Some(2048);
        s.max_tokens = Some(100);
    }));
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("max_tokens"),
        Some(&serde_json::json!(6144)),
        "raise max_tokens by 4096 completion room above budget, got {body}"
    );
}

#[test]
fn messages_encode_defaults_max_tokens_when_unset() {
    let ir = user_ir(IrSampling::default());
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("max_tokens"),
        Some(&serde_json::json!(4096)),
        "Messages requires max_tokens; chat clients often omit it, got {body}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.max_tokens" && event.action == LossAction::Preserve
        }),
        "default must be recorded, got {report:?}"
    );
}

#[test]
fn messages_encode_does_not_invent_thinking_when_unset() {
    let ir = user_ir(IrSampling::default());
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("thinking").is_none(),
        "unset sampling must omit thinking, got {body}"
    );
}

#[test]
fn messages_encode_empty_effort_does_not_invent_thinking() {
    for effort in ["", "  \t"] {
        let ir = user_ir(IrSampling::patch(|s| {
            s.reasoning_effort = Some(effort.into());
        }));
        let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(
            body.get("thinking").is_none(),
            "empty or whitespace-only effort must not invent thinking, got {body}"
        );
    }
}

#[test]
fn messages_encode_thinking_budget_wins_when_max_reasoning_unset() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.thinking_budget = Some(24576);
    }));
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/thinking/type").and_then(Value::as_str),
        Some("enabled"),
        "thinking_budget-only encode must write type enabled, got {body}"
    );
    assert_eq!(
        body.pointer("/thinking/budget_tokens"),
        Some(&serde_json::json!(24576)),
        "thinking_budget is the Messages slot when max_reasoning_tokens is unset, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.thinking_budget"),
        "thinking_budget has a Messages slot, got {report:?}"
    );
}

#[test]
fn messages_encode_keeps_explicit_thinking_budget_zero() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
        s.max_reasoning_tokens = Some(0);
    }));
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/thinking/type").and_then(Value::as_str),
        Some("enabled"),
        "include_thoughts=true with budget 0 stays enabled, got {body}"
    );
    assert_eq!(
        body.pointer("/thinking/budget_tokens"),
        Some(&serde_json::json!(0)),
        "explicit max_reasoning_tokens 0 must not become 10240, got {body}"
    );
}

#[test]
fn messages_encode_thinking_budget_zero_wins_when_max_reasoning_unset() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
        s.thinking_budget = Some(0);
    }));
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/thinking/budget_tokens"),
        Some(&serde_json::json!(0)),
        "explicit thinking_budget 0 must not become 10240, got {body}"
    );
}

#[test]
fn messages_decode_reads_thinking_enabled() {
    let req = br#"{
        "model": "claude-haiku-4-5-20251001",
        "thinking": { "type": "enabled", "budget_tokens": 2048 },
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::Messages, req).expect("decode");
    assert_eq!(ir.sampling.include_thoughts, Some(true));
    assert_eq!(ir.sampling.max_reasoning_tokens, Some(2048));
}

#[test]
fn messages_decode_reads_thinking_disabled() {
    let req = br#"{
        "model": "claude-haiku-4-5-20251001",
        "thinking": { "type": "disabled" },
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::Messages, req).expect("decode");
    assert_eq!(ir.sampling.include_thoughts, Some(false));
    assert_eq!(ir.sampling.max_reasoning_tokens, None);
}

#[test]
fn messages_decode_unknown_thinking_missing_type_drops() {
    let req = br#"{
        "model": "claude-haiku-4-5-20251001",
        "thinking": { "budget_tokens": 2048 },
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, report) = decode(Wire::Messages, req).expect("decode");
    assert_eq!(ir.sampling.include_thoughts, None);
    assert_eq!(ir.sampling.max_reasoning_tokens, None);
    assert!(
        loss_dropped(&report, "sampling.thinking"),
        "unknown thinking object must Drop, got {report:?}"
    );
}

#[test]
fn messages_decode_unknown_thinking_array_drops() {
    let req = br#"{
        "model": "claude-haiku-4-5-20251001",
        "thinking": [],
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, report) = decode(Wire::Messages, req).expect("decode");
    assert_eq!(ir.sampling.include_thoughts, None);
    assert_eq!(ir.sampling.max_reasoning_tokens, None);
    assert!(
        loss_dropped(&report, "sampling.thinking"),
        "array thinking must Drop, got {report:?}"
    );
}

#[test]
fn messages_decode_unknown_thinking_adaptive_drops() {
    let req = br#"{
        "model": "claude-haiku-4-5-20251001",
        "thinking": { "type": "adaptive" },
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, report) = decode(Wire::Messages, req).expect("decode");
    assert_eq!(ir.sampling.include_thoughts, None);
    assert_eq!(ir.sampling.max_reasoning_tokens, None);
    assert!(
        loss_dropped(&report, "sampling.thinking"),
        "adaptive thinking must Drop, got {report:?}"
    );
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("thinking").is_none(),
        "must not invent adaptive thinking on re-encode, got {body}"
    );
}

#[test]
fn messages_thinking_round_trips() {
    let req = br#"{
        "model": "claude-haiku-4-5-20251001",
        "thinking": { "type": "enabled", "budget_tokens": 2048 },
        "max_tokens": 8192,
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::Messages, req).expect("decode");
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/thinking/type").and_then(Value::as_str),
        Some("enabled")
    );
    assert_eq!(
        body.pointer("/thinking/budget_tokens"),
        Some(&serde_json::json!(2048))
    );
    assert_eq!(body.get("max_tokens"), Some(&serde_json::json!(8192)));
}

#[test]
fn gemini_thinking_config_survives_reasoning_sampling_fields() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
        s.thinking_budget = Some(24576);
        s.reasoning_effort = Some("high".into());
        s.max_reasoning_tokens = Some(1024);
    }));
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let tc = body
        .pointer("/generationConfig/thinkingConfig")
        .expect("thinkingConfig should be nested under generationConfig");
    assert_eq!(tc.get("includeThoughts"), Some(&Value::Bool(true)));
    assert_eq!(tc.get("thinkingBudget"), Some(&serde_json::json!(24576)));
    assert_eq!(
        tc.get("thinkingLevel").and_then(Value::as_str),
        Some("high"),
        "Gemini effort must emit thinkingLevel, got {body}"
    );
    assert!(
        body.get("thinkingConfig").is_none(),
        "must not emit top-level thinkingConfig: {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.reasoning_effort"),
        "Gemini effort has thinkingLevel, got {report:?}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.max_reasoning_tokens"
                && event.action == LossAction::Drop
                && event.detail.contains("sibling")
                && !event.detail.contains("no slot")
        }),
        "thinking_budget sibling win must not say no slot, got {report:?}"
    );
}

#[test]
fn gemini_encode_effort_as_thinking_level() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.reasoning_effort = Some("high".into());
    }));
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/generationConfig/thinkingConfig/thinkingLevel")
            .and_then(Value::as_str),
        Some("high"),
        "Gemini must emit thinkingLevel from effort, got {body}"
    );
    assert!(
        body.pointer("/generationConfig/thinkingConfig/thinkingBudget")
            .is_none(),
        "effort must not map to thinkingBudget, got {body}"
    );
    assert!(
        body.get("thinkingConfig").is_none(),
        "must not emit top-level thinkingConfig: {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.reasoning_effort"),
        "effort has a Gemini thinkingLevel slot, got {report:?}"
    );
}

#[test]
fn gemini_encode_thinking_level_is_lowercase() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.reasoning_effort = Some("MEDIUM".into());
    }));
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/generationConfig/thinkingConfig/thinkingLevel")
            .and_then(Value::as_str),
        Some("medium"),
        "thinkingLevel must be lowercase, got {body}"
    );
}

#[test]
fn gemini_encode_xhigh_effort_degrades_to_high() {
    for effort in ["xhigh", "x-high", "XHIGH", "X-High"] {
        let ir = user_ir(IrSampling::patch(|s| {
            s.reasoning_effort = Some(effort.into());
        }));
        let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body.pointer("/generationConfig/thinkingConfig/thinkingLevel")
                .and_then(Value::as_str),
            Some("high"),
            "{effort} must map to thinkingLevel high, got {body}"
        );
        assert!(
            loss_degraded(&report, "sampling.reasoning_effort"),
            "{effort} must Degrade to high, got {report:?}"
        );
        assert!(
            body.pointer("/generationConfig/thinkingConfig/thinkingBudget")
                .is_none(),
            "{effort} must not invent thinkingBudget, got {body}"
        );
    }
}

#[test]
fn gemini_encode_empty_effort_does_not_invent_thinking_level() {
    for effort in ["", "  \t"] {
        let ir = user_ir(IrSampling::patch(|s| {
            s.reasoning_effort = Some(effort.into());
        }));
        let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(
            body.pointer("/generationConfig/thinkingConfig").is_none(),
            "empty effort must not invent thinkingConfig, got {body}"
        );
        assert!(
            !loss_dropped(&report, "sampling.reasoning_effort"),
            "empty effort is unset, not a Drop, got {report:?}"
        );
    }
}

#[test]
fn gemini_decode_reads_thinking_level() {
    let req = br#"{
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        "generationConfig": {
            "thinkingConfig": { "thinkingLevel": "low" }
        }
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    assert_eq!(ir.sampling.reasoning_effort.as_deref(), Some("low"));
}

#[test]
fn gemini_decode_reads_snake_thinking_level() {
    let req = br#"{
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        "generationConfig": {
            "thinkingConfig": { "thinking_level": "minimal" }
        }
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    assert_eq!(ir.sampling.reasoning_effort.as_deref(), Some("minimal"));
}

#[test]
fn gemini_thinking_budget_from_messages_max_reasoning_tokens() {
    let req = br#"{
        "thinking": {"type": "enabled", "budget_tokens": 2048},
        "messages": [{"role": "user", "content": "hi"}]
    }"#;
    let (ir, _) = decode(Wire::Messages, req).expect("decode");
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/generationConfig/thinkingConfig/thinkingBudget"),
        Some(&serde_json::json!(2048)),
        "Messages budget_tokens must encode as Gemini thinkingBudget, got {body}"
    );
    assert!(
        !report.events.iter().any(|event| {
            event.path == "sampling.max_reasoning_tokens"
                && event.action == LossAction::Drop
                && event.detail.contains("no slot")
        }),
        "max_reasoning_tokens used as thinkingBudget must not Drop as no slot, got {report:?}"
    );
}

#[test]
fn gemini_thinking_budget_zero_from_max_reasoning_tokens() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
        s.max_reasoning_tokens = Some(0);
    }));
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/generationConfig/thinkingConfig/thinkingBudget"),
        Some(&serde_json::json!(0)),
        "max_reasoning_tokens 0 is official Gemini disable, got {body}"
    );
    assert_eq!(
        body.pointer("/generationConfig/thinkingConfig/includeThoughts"),
        Some(&Value::Bool(true)),
        "include_thoughts must still emit, got {body}"
    );
    assert!(
        !report.events.iter().any(|event| {
            event.path == "sampling.max_reasoning_tokens"
                && event.action == LossAction::Drop
                && event.detail.contains("no slot")
        }),
        "max_reasoning_tokens 0 has a Gemini slot, got {report:?}"
    );
}

#[test]
fn gemini_thinking_ir_drops_on_chat() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include_thoughts = Some(true);
        s.thinking_budget = Some(24576);
    }));
    let (bytes, report) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("thinkingConfig").is_none(),
        "Chat must not invent thinkingConfig, got {body}"
    );
    assert!(
        body.get("include_thoughts").is_none() && body.get("includeThoughts").is_none(),
        "Chat must not invent include_thoughts, got {body}"
    );
    assert!(
        body.get("thinking_budget").is_none() && body.get("thinkingBudget").is_none(),
        "Chat must not invent thinking_budget, got {body}"
    );
    assert!(
        body.get("thinking").is_none(),
        "Chat must not invent Messages thinking, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.include_thoughts"),
        "Chat include_thoughts drop missing, got {report:?}"
    );
    assert!(
        loss_dropped(&report, "sampling.thinking_budget"),
        "Chat thinking_budget drop missing, got {report:?}"
    );
}

#[test]
fn chat_required_tool_choice_maps_on_gemini() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.tool_choice = IrToolChoice::Required;
    }));
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("tool_choice").is_none(),
        "Gemini must not invent tool_choice, got {body}"
    );
    assert_eq!(
        body.pointer("/toolConfig/functionCallingConfig/mode")
            .and_then(Value::as_str),
        Some("ANY"),
        "Required must encode as functionCallingConfig.mode ANY, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.tool_choice"),
        "Gemini has a slot and must not Drop tool_choice, got {report:?}"
    );
}

#[test]
fn gemini_none_tool_choice_encodes() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.tool_choice = IrToolChoice::None;
    }));
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/toolConfig/functionCallingConfig/mode")
            .and_then(Value::as_str),
        Some("NONE"),
        "None must encode as functionCallingConfig.mode NONE, got {body}"
    );
    assert!(
        body.pointer("/toolConfig/functionCallingConfig/allowedFunctionNames")
            .is_none(),
        "None must not invent allowedFunctionNames, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.tool_choice"),
        "Gemini has a slot and must not Drop tool_choice, got {report:?}"
    );
}

#[test]
fn gemini_named_tool_choice_encodes() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.tool_choice = IrToolChoice::Named("lookup".into());
    }));
    let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/toolConfig/functionCallingConfig/mode")
            .and_then(Value::as_str),
        Some("ANY"),
        "Named must encode as functionCallingConfig.mode ANY, got {body}"
    );
    assert_eq!(
        body.pointer("/toolConfig/functionCallingConfig/allowedFunctionNames"),
        Some(&serde_json::json!(["lookup"])),
        "Named must encode allowedFunctionNames, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.tool_choice"),
        "Gemini has a slot and must not Drop tool_choice, got {report:?}"
    );
}

#[test]
fn gemini_tool_choice_round_trips() {
    let cases: &[(&str, IrToolChoice, Value)] = &[
        (
            r#"{
                "model": "gemini-2.5-pro",
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "toolConfig": {"functionCallingConfig": {"mode": "ANY"}}
            }"#,
            IrToolChoice::Required,
            serde_json::json!({"mode": "ANY"}),
        ),
        (
            r#"{
                "model": "gemini-2.5-pro",
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "toolConfig": {"functionCallingConfig": {"mode": "NONE"}}
            }"#,
            IrToolChoice::None,
            serde_json::json!({"mode": "NONE"}),
        ),
        (
            r#"{
                "model": "gemini-2.5-pro",
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "toolConfig": {
                    "functionCallingConfig": {
                        "mode": "ANY",
                        "allowedFunctionNames": ["lookup"]
                    }
                }
            }"#,
            IrToolChoice::Named("lookup".into()),
            serde_json::json!({"mode": "ANY", "allowedFunctionNames": ["lookup"]}),
        ),
    ];
    for (req, expected, fcc) in cases {
        let (ir, decode_report) = decode(Wire::Gemini, req.as_bytes()).expect("decode");
        assert_eq!(
            ir.sampling.tool_choice, *expected,
            "decode tool_choice from {req}"
        );
        assert!(
            !loss_dropped(&decode_report, "sampling.tool_choice"),
            "Gemini has toolConfig and must not Drop on decode, got {decode_report:?}"
        );
        let (bytes, report) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body.get("toolConfig")
                .and_then(|v| v.get("functionCallingConfig")),
            Some(fcc),
            "Gemini must emit functionCallingConfig {fcc}, got {body}"
        );
        assert!(
            !loss_dropped(&report, "sampling.tool_choice"),
            "Gemini has a slot and must not Drop tool_choice, got {report:?}"
        );
    }
}

#[test]
fn chat_grouped_function_call_records_thought_signature_drop() {
    let ir = IrRequest::new(
        "gpt-4",
        vec![
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
    );
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
fn chat_standalone_function_calls_encode_one_tool_calls_message() {
    let ir = IrRequest::new(
        "gpt-4",
        vec![
            IrItem::FunctionCall {
                call_id: "call_1".into(),
                name: "lookup".into(),
                arguments: r#"{"q":"x"}"#.into(),
                thought_signature: None,
            },
            IrItem::FunctionCall {
                call_id: "call_2".into(),
                name: "search".into(),
                arguments: r#"{"q":"y"}"#.into(),
                thought_signature: None,
            },
        ],
    );
    let (bytes, _) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .expect("messages");
    assert_eq!(
        messages.len(),
        1,
        "standalone FunctionCalls must be one assistant turn, got {body}"
    );
    assert_eq!(
        messages[0].get("content"),
        Some(&Value::Null),
        "tool-only assistant content must stay null, got {body}"
    );
    let calls = messages[0]
        .get("tool_calls")
        .and_then(Value::as_array)
        .expect("tool_calls");
    assert_eq!(
        calls.len(),
        2,
        "one tool_calls array with both calls, got {body}"
    );
    assert_eq!(
        calls[0].pointer("/function/name").and_then(Value::as_str),
        Some("lookup"),
        "first tool_call name, got {body}"
    );
    assert_eq!(
        calls[1].pointer("/function/name").and_then(Value::as_str),
        Some("search"),
        "second tool_call name, got {body}"
    );
}

#[test]
fn gemini_encode_does_not_invent_empty_function_call_args() {
    let ir = IrRequest::new(
        "gemini-2.5-flash",
        vec![IrItem::FunctionCall {
            call_id: "lookup".into(),
            name: "lookup".into(),
            arguments: "not-json".into(),
            thought_signature: None,
        }],
    );
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
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
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
    );
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema = Some(serde_json::json!({"type": "object"}));
        s.json_schema_name = None;
    }));
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

    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema = Some(serde_json::json!(["not", "object"]));
        s.json_schema_name = Some("answer".into());
    }));
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema = Some(serde_json::json!({"type": "object"}));
        s.json_schema_name = None;
    }));
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

    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema = Some(serde_json::json!("not-object"));
        s.json_schema_name = Some("answer".into());
    }));
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema = Some(serde_json::json!({"type": "object"}));
        s.json_schema_name = Some("answer".into());
    }));
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema =
            Some(serde_json::json!({"type": "object", "properties": {"n": {"type": "number"}}}));
        s.json_schema_name = None;
    }));
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
    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema = Some(serde_json::json!(["not", "object"]));
        s.json_schema_name = Some("answer".into());
    }));
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

fn chat_sampling_ir(model: &str) -> IrRequest {
    IrRequest::new(model, vec![]).with_sampling(IrSampling::patch(|s| {
        s.max_tokens = Some(64);
        s.temperature = Some(0.2);
    }))
}

fn assert_chat_max_completion(model: &str) {
    let (bytes, report) = encode(
        Wire::ChatCompletions,
        &chat_sampling_ir(model),
        &chat_profile(),
    )
    .expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("max_completion_tokens").and_then(Value::as_u64),
        Some(64),
        "{model} must write max_completion_tokens, got {body}"
    );
    assert!(
        body.get("max_tokens").is_none(),
        "{model} must omit max_tokens, got {body}"
    );
    assert!(
        body.get("temperature").is_none(),
        "{model} must omit temperature, got {body}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.temperature" && event.action == LossAction::Drop
        }),
        "{model} must Drop sampling.temperature, got {report:?}"
    );
}

fn assert_chat_classic_max_tokens(model: &str) {
    let (bytes, report) = encode(
        Wire::ChatCompletions,
        &chat_sampling_ir(model),
        &chat_profile(),
    )
    .expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("max_tokens").and_then(Value::as_u64),
        Some(64),
        "{model} must write max_tokens, got {body}"
    );
    assert!(
        body.get("max_completion_tokens").is_none(),
        "{model} must omit max_completion_tokens, got {body}"
    );
    assert!(
        body.get("temperature").is_some(),
        "{model} must keep temperature, got {body}"
    );
    assert!(
        !report.events.iter().any(|event| {
            event.path == "sampling.temperature" && event.action == LossAction::Drop
        }),
        "{model} must not Drop temperature, got {report:?}"
    );
}

#[test]
fn chat_encode_o1_uses_max_completion_tokens() {
    assert_chat_max_completion("o1");
}

#[test]
fn chat_encode_o3_mini_uses_max_completion_tokens() {
    assert_chat_max_completion("o3-mini");
}

#[test]
fn chat_encode_o4_mini_uses_max_completion_tokens() {
    assert_chat_max_completion("o4-mini");
}

#[test]
fn chat_encode_openai_o3_mini_uses_max_completion_tokens() {
    assert_chat_max_completion("openai/o3-mini");
}

#[test]
fn chat_encode_gpt_5_uses_max_completion_tokens() {
    assert_chat_max_completion("gpt-5");
}

#[test]
fn chat_encode_gpt_5_mini_uses_max_completion_tokens() {
    assert_chat_max_completion("gpt-5-mini");
}

#[test]
fn chat_encode_o3_mini_casefold_uses_max_completion_tokens() {
    assert_chat_max_completion("O3-MINI");
}

#[test]
fn chat_encode_gpt_4o_does_not_use_max_completion_tokens() {
    assert_chat_classic_max_tokens("gpt-4o");
}

#[test]
fn chat_encode_o10_does_not_use_max_completion_tokens() {
    assert_chat_classic_max_tokens("o10");
}

#[test]
fn chat_decode_reads_max_completion_tokens() {
    let req = br#"{
        "model": "o3-mini",
        "messages": [{"role": "user", "content": "hi"}],
        "max_completion_tokens": 64
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, req).expect("decode");
    assert_eq!(ir.sampling.max_tokens, Some(64));
}

#[test]
fn chat_encode_decode_roundtrip_keeps_max_completion_tokens() {
    let ir = chat_sampling_ir("o3-mini");
    let (bytes, _) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("encode");
    let (decoded, _) = decode(Wire::ChatCompletions, &bytes).expect("decode");
    assert_eq!(decoded.sampling.max_tokens, Some(64));
}

#[test]
fn chat_decode_reads_max_tokens_for_classic_models() {
    let req = br#"{
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 32
    }"#;
    let (ir, _) = decode(Wire::ChatCompletions, req).expect("decode");
    assert_eq!(ir.sampling.max_tokens, Some(32));
}

fn chat_sampling_ir_with_top_p(model: &str) -> IrRequest {
    let mut ir = chat_sampling_ir(model);
    ir.sampling.top_p = Some(0.9);
    ir
}

#[test]
fn chat_encode_o3_mini_omits_top_p() {
    let (bytes, report) = encode(
        Wire::ChatCompletions,
        &chat_sampling_ir_with_top_p("o3-mini"),
        &chat_profile(),
    )
    .expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("top_p").is_none(),
        "o3-mini must omit top_p, got {body}"
    );
    assert!(
        body.get("temperature").is_none(),
        "o3-mini must omit temperature, got {body}"
    );
    assert_eq!(
        body.get("max_completion_tokens").and_then(Value::as_u64),
        Some(64),
        "o3-mini must write max_completion_tokens, got {body}"
    );
    assert!(
        report
            .events
            .iter()
            .any(|event| { event.path == "sampling.top_p" && event.action == LossAction::Drop }),
        "o3-mini must Drop sampling.top_p, got {report:?}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.temperature" && event.action == LossAction::Drop
        }),
        "o3-mini must Drop sampling.temperature, got {report:?}"
    );
}

#[test]
fn chat_encode_gpt_4o_keeps_top_p() {
    let (bytes, report) = encode(
        Wire::ChatCompletions,
        &chat_sampling_ir_with_top_p("gpt-4o"),
        &chat_profile(),
    )
    .expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let top_p = body
        .get("top_p")
        .and_then(Value::as_f64)
        .expect("gpt-4o must write top_p");
    assert!(
        (top_p - 0.9).abs() < 1e-6,
        "gpt-4o top_p={top_p}, got {body}"
    );
    assert!(
        !report
            .events
            .iter()
            .any(|event| { event.path == "sampling.top_p" && event.action == LossAction::Drop }),
        "gpt-4o must not Drop top_p, got {report:?}"
    );
}

#[test]
fn messages_document_part_round_trips_as_raw() {
    let req = br#"{
        "model": "claude-opus-4-6",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": "application/pdf",
                    "data": "AAAA"
                }
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Messages, req).expect("decode");
    assert!(
        ir.items.iter().any(|item| matches!(
            item,
            IrItem::User { parts } if parts.iter().any(|p| matches!(
                p,
                IrPart::Raw { type_name, raw }
                    if type_name == "document"
                        && raw.get("type").and_then(Value::as_str) == Some("document")
            ))
        )),
        "document part must become IrPart::Raw, got {:?}",
        ir.items
    );
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let content = body
        .pointer("/messages/0/content")
        .and_then(Value::as_array)
        .expect("content");
    assert!(
        content
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("document")),
        "encode Messages must keep type document, got {body}"
    );
}

#[test]
fn responses_input_file_part_round_trips_as_raw() {
    let req = br#"{
        "model": "gpt-5",
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_file",
                "file_id": "file-abc"
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Responses, req).expect("decode");
    assert!(
        ir.items.iter().any(|item| matches!(
            item,
            IrItem::User { parts } if parts.iter().any(|p| matches!(
                p,
                IrPart::Raw { type_name, raw }
                    if type_name == "input_file"
                        && raw.get("file_id").and_then(Value::as_str) == Some("file-abc")
            ))
        )),
        "input_file part must become IrPart::Raw, got {:?}",
        ir.items
    );
    let (bytes, _) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let content = body
        .pointer("/input/0/content")
        .and_then(Value::as_array)
        .expect("content");
    assert!(
        content
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("input_file")),
        "encode Responses must keep type input_file, got {body}"
    );
}

#[test]
#[allow(non_snake_case)]
fn gemini_fileData_part_round_trips_as_raw() {
    let req = br#"{
        "model": "gemini-2.5-flash",
        "contents": [{
            "role": "user",
            "parts": [{
                "fileData": {
                    "fileUri": "files/abc",
                    "mimeType": "application/pdf"
                }
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    assert!(
        ir.items.iter().any(|item| matches!(
            item,
            IrItem::User { parts } if parts.iter().any(|p| matches!(
                p,
                IrPart::Raw { type_name, raw }
                    if type_name == "fileData"
                        && raw.pointer("/fileData/fileUri").and_then(Value::as_str)
                            == Some("files/abc")
            ))
        )),
        "fileData part must become IrPart::Raw, got {:?}",
        ir.items
    );
    let (bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/contents/0/parts/0/fileData/fileUri")
            .and_then(Value::as_str),
        Some("files/abc"),
        "encode Gemini must keep fileData, got {body}"
    );
}

#[test]
#[allow(non_snake_case)]
fn gemini_fileData_encode_messages_does_not_leak() {
    let req = br#"{
        "model": "gemini-2.5-flash",
        "contents": [{
            "role": "user",
            "parts": [{
                "fileData": {
                    "fileUri": "files/abc",
                    "mimeType": "application/pdf"
                }
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    let (bytes, report) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        !body.to_string().contains("fileData"),
        "Messages encode must not leak Gemini fileData, got {body}"
    );
    assert!(
        report
            .events
            .iter()
            .any(|event| { event.action == LossAction::Drop && event.path.contains("fileData") }),
        "Gemini fileData Raw must Drop on Messages encode, got {report:?}"
    );
}

#[test]
#[allow(non_snake_case)]
fn gemini_fileData_encode_responses_does_not_leak() {
    let req = br#"{
        "model": "gemini-2.5-flash",
        "contents": [{
            "role": "user",
            "parts": [{
                "fileData": {
                    "fileUri": "files/abc",
                    "mimeType": "application/pdf"
                }
            }]
        }]
    }"#;
    let (ir, _) = decode(Wire::Gemini, req).expect("decode");
    let (bytes, report) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        !body.to_string().contains("fileData"),
        "Responses encode must not leak Gemini fileData, got {body}"
    );
    assert!(
        report
            .events
            .iter()
            .any(|event| { event.action == LossAction::Drop && event.path.contains("fileData") }),
        "Gemini fileData Raw must Drop on Responses encode, got {report:?}"
    );
}

#[test]
fn responses_include_extras_survive_remap() {
    let req = br#"{
        "model": "gpt-5",
        "input": [{"role": "user", "content": "hi"}],
        "include": ["file_search_call.results", "reasoning.encrypted_content"]
    }"#;
    let (ir, _) = decode(Wire::Responses, req).expect("decode");
    assert!(
        ir.sampling
            .include
            .iter()
            .any(|item| item == "file_search_call.results"),
        "decode must keep include extras, got {:?}",
        ir.sampling.include
    );
    let (bytes, _) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let include = body
        .get("include")
        .and_then(Value::as_array)
        .expect("include");
    let as_str: Vec<&str> = include.iter().filter_map(Value::as_str).collect();
    assert!(
        as_str.contains(&"file_search_call.results"),
        "encode must keep file_search_call.results, got {body}"
    );
    assert!(
        as_str.contains(&"reasoning.encrypted_content"),
        "encode must keep reasoning.encrypted_content, got {body}"
    );
}

#[test]
fn responses_include_default_still_writes_encrypted_reasoning() {
    let ir = IrRequest::new(
        "gpt-5",
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    );
    assert!(
        ir.sampling.include.is_empty(),
        "default include must be empty"
    );
    let (bytes, _) = encode(Wire::Responses, &ir, &flatten_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("include"),
        Some(&serde_json::json!(["reasoning.encrypted_content"])),
        "empty include must still write encrypted reasoning, got {body}"
    );
}

#[test]
fn sampling_include_drops_off_responses() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.include = vec!["file_search_call.results".into()];
    }));
    for (wire, profile) in [
        (Wire::ChatCompletions, chat_profile()),
        (Wire::Messages, messages_profile()),
        (Wire::Gemini, gemini_profile()),
        (Wire::Converse, converse_profile()),
    ] {
        let (bytes, report) = encode(wire, &ir, &profile).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(
            body.get("include").is_none(),
            "{wire:?} must not write include, got {body}"
        );
        assert!(
            loss_dropped(&report, "sampling.include"),
            "{wire:?} include drop missing, got {report:?}"
        );
    }
}

fn messages_roles(body: &Value) -> Vec<(String, Option<String>)> {
    body.get("messages")
        .and_then(Value::as_array)
        .expect("messages")
        .iter()
        .map(|msg| {
            let role = msg
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let text = msg
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .map(str::to_owned);
            (role, text)
        })
        .collect()
}

#[test]
fn messages_encode_appends_continue_on_assistant_last() {
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![
            IrItem::User {
                parts: vec![IrPart::Text("hi".into())],
            },
            IrItem::Assistant {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
    );
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let roles = messages_roles(&body);
    assert_eq!(
        roles.last(),
        Some(&("user".into(), Some("Continue.".into()))),
        "assistant-last must append Continue., got {body}"
    );
    assert_eq!(roles.len(), 3, "user + assistant + Continue., got {body}");
}

#[test]
fn messages_encode_appends_continue_on_function_call_last() {
    let ir = IrRequest::new(
        "claude-opus-4-6",
        vec![IrItem::FunctionCall {
            call_id: "call_1".into(),
            name: "lookup".into(),
            arguments: "{}".into(),
            thought_signature: None,
        }],
    );
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let roles = messages_roles(&body);
    assert_eq!(
        roles.last(),
        Some(&("user".into(), Some("Continue.".into()))),
        "function-call-last must append Continue., got {body}"
    );
}

#[test]
fn messages_encode_skips_continue_when_user_last_or_empty() {
    let user_last = IrRequest::new(
        "claude-opus-4-6",
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    );
    let (bytes, _) = encode(Wire::Messages, &user_last, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let roles = messages_roles(&body);
    assert_eq!(
        roles,
        vec![("user".into(), Some("hi".into()))],
        "user-last must stay unchanged, got {body}"
    );

    let empty = IrRequest::new("claude-opus-4-6", vec![]);
    let (bytes, _) = encode(Wire::Messages, &empty, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .expect("messages");
    assert!(messages.is_empty(), "empty IR must stay empty, got {body}");
}

#[test]
fn other_wires_do_not_append_continue_on_assistant_last() {
    let ir = IrRequest::new(
        "gpt-4",
        vec![
            IrItem::User {
                parts: vec![IrPart::Text("hi".into())],
            },
            IrItem::Assistant {
                parts: vec![IrPart::Text("hello".into())],
            },
        ],
    );
    let (chat_bytes, _) = encode(Wire::ChatCompletions, &ir, &chat_profile()).expect("chat");
    let chat: Value = serde_json::from_slice(&chat_bytes).expect("json");
    let chat_msgs = chat
        .get("messages")
        .and_then(Value::as_array)
        .expect("chat messages");
    assert_eq!(
        chat_msgs.len(),
        2,
        "Chat must not append Continue., got {chat}"
    );
    assert_eq!(
        chat_msgs
            .last()
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str),
        Some("assistant"),
        "Chat last role, got {chat}"
    );

    let (gemini_bytes, _) = encode(Wire::Gemini, &ir, &gemini_profile()).expect("gemini");
    let gemini: Value = serde_json::from_slice(&gemini_bytes).expect("json");
    let contents = gemini
        .get("contents")
        .and_then(Value::as_array)
        .expect("gemini contents");
    assert_eq!(
        contents.len(),
        2,
        "Gemini must not append Continue., got {gemini}"
    );

    let (resp_bytes, _) = encode(Wire::Responses, &ir, &flatten_profile()).expect("responses");
    let resp: Value = serde_json::from_slice(&resp_bytes).expect("json");
    let input = resp
        .get("input")
        .and_then(Value::as_array)
        .expect("responses input");
    assert!(
        !serde_json::to_string(&input)
            .expect("ser")
            .contains("Continue."),
        "Responses must not append Continue., got {resp}"
    );
}

#[test]
fn messages_encode_object_tool_schema_emits_required_array() {
    let ir = IrRequest::new(
        "grok-4",
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    )
    .with_tools(vec![
        IrTool::Function {
            name: "lookup".into(),
            description: "lookup".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        IrTool::Function {
            name: "null_required".into(),
            description: "null required".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": null
            }),
        },
        IrTool::Function {
            name: "keep".into(),
            description: "keep listed required".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"q": {"type": "string"}},
                "required": ["q"]
            }),
        },
    ]);
    let (bytes, _) = encode(Wire::Messages, &ir, &messages_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let tools = body.get("tools").and_then(Value::as_array).expect("tools");
    assert_eq!(
        tools[0].pointer("/input_schema/required"),
        Some(&serde_json::json!([])),
        "omitted required becomes [], else Grok Build Messages 400s, got {body}"
    );
    assert_eq!(
        tools[1].pointer("/input_schema/required"),
        Some(&serde_json::json!([])),
        "null required becomes [], else Grok Build Messages 400s, got {body}"
    );
    assert_eq!(
        tools[2].pointer("/input_schema/required"),
        Some(&serde_json::json!(["q"])),
        "listed required must stay, got {body}"
    );
}

fn converse_profile() -> ResolvedProfile {
    profile(
        r#"
schema_version = 1
id = "amazon-bedrock"
wire = "converse"
aws_service = "bedrock"
aws_region = "us-east-1"
"#,
    )
}

#[test]
fn converse_encode_does_not_invent_empty_function_call_args() {
    let ir = IrRequest::new(
        "amazon.nova-lite-v1:0",
        vec![IrItem::FunctionCall {
            call_id: "t1".into(),
            name: "lookup".into(),
            arguments: "not-json".into(),
            thought_signature: None,
        }],
    );
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let input = body.pointer("/messages/0/content/0/toolUse/input");
    assert_ne!(
        input,
        Some(&serde_json::json!({})),
        "invalid JSON must not become empty object, got {body}"
    );
    assert_eq!(
        input,
        Some(&Value::String("not-json".into())),
        "invalid JSON must stay a string, got {body}"
    );
}

#[test]
fn converse_mixed_assistant_text_then_tool_use_round_trips_one_message() {
    let req = br#"{
      "modelId": "amazon.nova-lite-v1:0",
      "messages": [
        {"role": "assistant", "content": [
          {"text": "I'll look that up."},
          {"toolUse": {"toolUseId": "t1", "name": "lookup", "input": {"q": "x"}}}
        ]}
      ]
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode mixed assistant");
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let messages = body["messages"]
        .as_array()
        .expect("messages must be an array");
    assert_eq!(
        messages.len(),
        1,
        "re-encode must stay one assistant message, got {body}"
    );
    assert_eq!(messages[0]["role"], "assistant");
    let content = messages[0]["content"]
        .as_array()
        .expect("content must be an array");
    assert_eq!(content.len(), 2, "expected text then toolUse, got {body}");
    assert_eq!(content[0]["text"], "I'll look that up.");
    assert_eq!(content[1]["toolUse"]["toolUseId"], "t1");
    assert_eq!(content[1]["toolUse"]["name"], "lookup");
}

#[test]
fn converse_round_trip_text_and_tool() {
    let req = br#"{
      "modelId": "amazon.nova-lite-v1:0",
      "system": [{"text": "sys"}],
      "messages": [
        {"role": "user", "content": [{"text": "hi"}]},
        {"role": "assistant", "content": [{"toolUse": {"toolUseId": "t1", "name": "lookup", "input": {"q": "x"}}}]},
        {"role": "user", "content": [{"toolResult": {"toolUseId": "t1", "content": [{"text": "ok"}]}}]}
      ],
      "inferenceConfig": {"maxTokens": 32, "temperature": 0.2},
      "toolConfig": {
        "tools": [{"toolSpec": {"name": "lookup", "description": "d", "inputSchema": {"json": {"type": "object"}}}}],
        "toolChoice": {"auto": {}}
      }
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode converse");
    assert_eq!(ir.model, "amazon.nova-lite-v1:0");
    assert!(matches!(&ir.items[0], IrItem::System { text } if text == "sys"));
    assert_eq!(ir.sampling.max_tokens, Some(32));
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("modelId").is_none(),
        "model stays in chat_path, not the JSON body: {body}"
    );
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(
        body["messages"][1]["content"][0]["toolUse"]["name"],
        "lookup"
    );
    assert_eq!(body["inferenceConfig"]["maxTokens"], 32);
    assert!(body["toolConfig"]["tools"][0]["toolSpec"]["name"] == "lookup");
}

#[test]
fn converse_encode_empty_messages_fails() {
    let ir = IrRequest::new(
        "amazon.nova-lite-v1:0",
        vec![IrItem::System { text: "sys".into() }],
    );
    let err = encode(Wire::Converse, &ir, &converse_profile())
        .expect_err("Converse encode must fail when messages would be empty");
    let msg = err.to_string();
    assert!(msg.contains("messages") && msg.contains("empty"), "{msg}");
}

#[test]
fn converse_decode_tool_use_missing_id_fails() {
    let req = br#"{
      "messages": [
        {"role": "assistant", "content": [{"toolUse": {"name": "lookup", "input": {}}}]}
      ]
    }"#;
    let err = decode(Wire::Converse, req).expect_err("toolUse without toolUseId");
    let msg = err.to_string();
    assert!(
        msg.contains("toolUseId") || msg.contains("toolUse"),
        "{msg}"
    );
}

#[test]
fn converse_decode_tool_result_missing_id_fails() {
    let req = br#"{
      "messages": [
        {"role": "user", "content": [{"toolResult": {"content": [{"text": "ok"}]}}]}
      ]
    }"#;
    let err = decode(Wire::Converse, req).expect_err("toolResult without toolUseId");
    let msg = err.to_string();
    assert!(
        msg.contains("toolUseId") || msg.contains("toolResult"),
        "{msg}"
    );
}

#[test]
fn converse_parallel_function_outputs_encode_one_user_message() {
    let ir = IrRequest::new(
        "amazon.nova-lite-v1:0",
        vec![
            IrItem::FunctionOutput {
                call_id: "t1".into(),
                output: "one".into(),
            },
            IrItem::FunctionOutput {
                call_id: "t2".into(),
                output: "two".into(),
            },
        ],
    );
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let messages = body["messages"]
        .as_array()
        .expect("messages must be an array");
    assert_eq!(
        messages.len(),
        1,
        "parallel toolResults must be one user turn, got {body}"
    );
    assert_eq!(messages[0]["role"], "user");
    let content = messages[0]["content"]
        .as_array()
        .expect("content must be an array");
    assert_eq!(
        content.len(),
        2,
        "expected two toolResult blocks, got {body}"
    );
    assert_eq!(content[0]["toolResult"]["toolUseId"], "t1");
    assert_eq!(content[0]["toolResult"]["content"][0]["text"], "one");
    assert_eq!(content[1]["toolResult"]["toolUseId"], "t2");
    assert_eq!(content[1]["toolResult"]["content"][0]["text"], "two");
}

#[test]
fn converse_mixed_assistant_tool_use_then_text_round_trips_one_message() {
    let req = br#"{
      "modelId": "amazon.nova-lite-v1:0",
      "messages": [
        {"role": "assistant", "content": [
          {"toolUse": {"toolUseId": "t1", "name": "lookup", "input": {"q": "x"}}},
          {"text": "done looking."}
        ]}
      ]
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode mixed assistant");
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let messages = body["messages"]
        .as_array()
        .expect("messages must be an array");
    assert_eq!(
        messages.len(),
        1,
        "re-encode must stay one assistant message, got {body}"
    );
    assert_eq!(messages[0]["role"], "assistant");
    let content = messages[0]["content"]
        .as_array()
        .expect("content must be an array");
    assert_eq!(content.len(), 2, "expected toolUse then text, got {body}");
    assert_eq!(content[0]["toolUse"]["toolUseId"], "t1");
    assert_eq!(content[0]["toolUse"]["name"], "lookup");
    assert_eq!(content[1]["text"], "done looking.");
}

#[test]
fn converse_decode_tool_result_json_round_trips_nonempty() {
    let req = br#"{
      "modelId": "amazon.nova-lite-v1:0",
      "messages": [
        {"role": "user", "content": [{
          "toolResult": {
            "toolUseId": "t1",
            "content": [{ "json": { "ok": true, "n": 1 } }]
          }
        }]}
      ]
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode json toolResult");
    let output = ir.items.iter().find_map(|item| match item {
        IrItem::FunctionOutput { call_id, output } if call_id == "t1" => Some(output.as_str()),
        _ => None,
    });
    let output = output.expect("FunctionOutput t1");
    assert!(
        !output.is_empty(),
        "json toolResult must not decode to empty string, got {output:?}"
    );
    assert!(
        output.contains("ok") && output.contains("true"),
        "json payload must be serialized, got {output:?}"
    );
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let arr = body
        .pointer("/messages/0/content/0/toolResult/content")
        .and_then(Value::as_array)
        .expect("toolResult content");
    assert!(
        !arr.is_empty(),
        "re-encode must keep toolResult content, got {body}"
    );
    let lost = arr.iter().all(|p| {
        p.get("text")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            && p.get("json").is_none()
    });
    assert!(
        !lost,
        "re-encode must not be empty text-only loss, got {body}"
    );
}

#[test]
fn converse_whitespace_only_assistant_text_with_tool_use_omits_blank_text() {
    let ir = IrRequest::new(
        "amazon.nova-lite-v1:0",
        vec![
            IrItem::Assistant {
                parts: vec![IrPart::Text("  \n\t  ".into())],
            },
            IrItem::FunctionCall {
                call_id: "t1".into(),
                name: "lookup".into(),
                arguments: r#"{"q":"x"}"#.into(),
                thought_signature: None,
            },
        ],
    );
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let messages = body["messages"]
        .as_array()
        .expect("messages must be an array");
    assert_eq!(
        messages.len(),
        1,
        "whitespace text plus toolUse must stay one assistant turn, got {body}"
    );
    assert_eq!(messages[0]["role"], "assistant");
    let content = messages[0]["content"]
        .as_array()
        .expect("content must be an array");
    assert_eq!(
        content.len(),
        1,
        "blank text block must be omitted; only toolUse remains, got {body}"
    );
    assert!(
        content[0].get("text").is_none(),
        "must not emit a blank text contentBlock, got {body}"
    );
    assert_eq!(content[0]["toolUse"]["toolUseId"], "t1");
    assert_eq!(content[0]["toolUse"]["name"], "lookup");
}

#[test]
fn converse_empty_function_output_encodes_nonempty_tool_result_text() {
    let ir = IrRequest::new(
        "amazon.nova-lite-v1:0",
        vec![IrItem::FunctionOutput {
            call_id: "t1".into(),
            output: String::new(),
        }],
    );
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let text = body
        .pointer("/messages/0/content/0/toolResult/content/0/text")
        .and_then(Value::as_str);
    let text = text.expect("toolResult text");
    assert!(
        !text.trim().is_empty(),
        "empty FunctionOutput must not emit blank toolResult text, got {body}"
    );
    assert_eq!(
        text, ".",
        "empty tool result uses the same '.' placeholder as Messages, got {body}"
    );
    assert_eq!(
        body.pointer("/messages/0/content/0/toolResult/toolUseId")
            .and_then(Value::as_str),
        Some("t1"),
        "toolUseId must stay, got {body}"
    );
}

#[test]
fn converse_mixed_user_tool_result_then_text_round_trips_one_message() {
    let req = br#"{
      "modelId": "anthropic.claude-sonnet-4-20250514-v1:0",
      "messages": [
        {"role": "user", "content": [
          {"toolResult": {"toolUseId": "t1", "content": [{"text": "ok"}]}},
          {"text": "thanks"}
        ]}
      ]
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode mixed user");
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let messages = body["messages"]
        .as_array()
        .expect("messages must be an array");
    assert_eq!(
        messages.len(),
        1,
        "re-encode must stay one user message, got {body}"
    );
    assert_eq!(messages[0]["role"], "user");
    let content = messages[0]["content"]
        .as_array()
        .expect("content must be an array");
    assert_eq!(
        content.len(),
        2,
        "expected toolResult then text, got {body}"
    );
    assert_eq!(content[0]["toolResult"]["toolUseId"], "t1");
    assert_eq!(content[0]["toolResult"]["content"][0]["text"], "ok");
    assert_eq!(content[1]["text"], "thanks");
}

#[test]
fn converse_mixed_user_text_then_tool_result_round_trips_one_message() {
    let req = br#"{
      "modelId": "anthropic.claude-sonnet-4-20250514-v1:0",
      "messages": [
        {"role": "user", "content": [
          {"text": "here is the result"},
          {"toolResult": {"toolUseId": "t1", "content": [{"text": "ok"}]}}
        ]}
      ]
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode mixed user");
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let messages = body["messages"]
        .as_array()
        .expect("messages must be an array");
    assert_eq!(
        messages.len(),
        1,
        "re-encode must stay one user message, got {body}"
    );
    assert_eq!(messages[0]["role"], "user");
    let content = messages[0]["content"]
        .as_array()
        .expect("content must be an array");
    assert_eq!(
        content.len(),
        2,
        "expected text then toolResult, got {body}"
    );
    assert_eq!(content[0]["text"], "here is the result");
    assert_eq!(content[1]["toolResult"]["toolUseId"], "t1");
    assert_eq!(content[1]["toolResult"]["content"][0]["text"], "ok");
}

#[test]
fn converse_reasoning_text_signature_round_trips() {
    let req = br#"{
      "modelId": "anthropic.claude-sonnet-4-20250514-v1:0",
      "messages": [
        {"role": "assistant", "content": [
          {"reasoningContent": {"reasoningText": {"text": "plan", "signature": "sig_abc"}}},
          {"text": "done"}
        ]}
      ]
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode signed reasoning");
    assert!(
        ir.items.iter().any(|item| matches!(
            item,
            IrItem::Assistant { parts } if parts.iter().any(|part| matches!(
                part,
                IrPart::Thinking { text, signature }
                    if text == "plan" && signature.as_deref() == Some("sig_abc")
            ))
        )),
        "decode must produce IrPart::Thinking {{ text: \"plan\", signature: Some(\"sig_abc\") }}, got {:?}",
        ir.items
    );
    let (bytes, _) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let content = body
        .pointer("/messages/0/content")
        .and_then(Value::as_array)
        .expect("content");
    assert!(
        content.iter().any(|block| {
            block
                .pointer("/reasoningContent/reasoningText/text")
                .and_then(Value::as_str)
                == Some("plan")
                && block
                    .pointer("/reasoningContent/reasoningText/signature")
                    .and_then(Value::as_str)
                    == Some("sig_abc")
        }),
        "replay must keep reasoningText.signature, got {body}"
    );
    assert_eq!(content[1]["text"], "done");
}

#[test]
fn converse_encode_drops_thinking_schema_and_parallel() {
    let schema = serde_json::json!({"type": "object"});
    let ir = user_ir(IrSampling::patch(|s| {
        s.max_reasoning_tokens = Some(2048);
        s.include_thoughts = Some(true);
        s.reasoning_effort = Some("high".into());
        s.thinking_budget = Some(1024);
        s.json_schema = Some(schema.clone());
        s.json_schema_name = Some("answer".into());
        s.parallel_tool_calls = Some(true);
    }));
    let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/outputConfig/textFormat/type")
            .and_then(Value::as_str),
        Some("json_schema"),
        "Converse must emit outputConfig.textFormat, got {body}"
    );
    assert_eq!(
        body.pointer("/outputConfig/textFormat/structure/jsonSchema/name")
            .and_then(Value::as_str),
        Some("answer"),
        "Converse must emit jsonSchema.name, got {body}"
    );
    assert_eq!(
        body.pointer("/outputConfig/effort").and_then(Value::as_str),
        Some("high"),
        "Converse must emit outputConfig.effort, got {body}"
    );
    assert!(
        body.get("max_reasoning_tokens").is_none()
            && body.get("maxReasoningTokens").is_none()
            && body.get("include_thoughts").is_none()
            && body.get("includeThoughts").is_none()
            && body.get("reasoning_effort").is_none()
            && body.get("reasoningEffort").is_none()
            && body.get("thinking_budget").is_none()
            && body.get("thinkingBudget").is_none()
            && body.get("thinkingConfig").is_none()
            && body.get("thinking").is_none()
            && body.get("json_schema").is_none()
            && body.get("jsonSchema").is_none()
            && body.get("responseSchema").is_none()
            && body.get("response_format").is_none()
            && body.get("output_format").is_none()
            && body.get("parallel_tool_calls").is_none()
            && body.get("parallelToolCalls").is_none(),
        "Converse must not invent thinking/schema/parallel slots, got {body}"
    );
    for path in [
        "sampling.max_reasoning_tokens",
        "sampling.include_thoughts",
        "sampling.thinking_budget",
        "sampling.parallel_tool_calls",
    ] {
        assert!(
            loss_dropped(&report, path),
            "Converse {path} drop missing, got {report:?}"
        );
    }
    for path in [
        "sampling.reasoning_effort",
        "sampling.json_schema",
        "sampling.json_schema_name",
    ] {
        assert!(
            !loss_dropped(&report, path),
            "Converse has outputConfig and must not Drop {path}, got {report:?}"
        );
    }
}

#[test]
fn converse_encode_output_config_schema_and_effort() {
    let schema = serde_json::json!({"type": "object", "properties": {"ok": {"type": "boolean"}}});
    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema = Some(schema.clone());
        s.json_schema_name = Some("answer".into());
        s.reasoning_effort = Some("high".into());
    }));
    let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/outputConfig/textFormat/type")
            .and_then(Value::as_str),
        Some("json_schema"),
        "textFormat.type must be json_schema, got {body}"
    );
    let schema_str = body
        .pointer("/outputConfig/textFormat/structure/jsonSchema/schema")
        .and_then(Value::as_str)
        .expect("schema must be a JSON string");
    let parsed: Value = serde_json::from_str(schema_str).expect("schema string parses");
    assert_eq!(
        parsed, schema,
        "schema string must parse back to the object"
    );
    assert_eq!(
        body.pointer("/outputConfig/textFormat/structure/jsonSchema/name")
            .and_then(Value::as_str),
        Some("answer"),
        "jsonSchema.name must be answer, got {body}"
    );
    assert_eq!(
        body.pointer("/outputConfig/effort").and_then(Value::as_str),
        Some("high"),
        "effort must be high, got {body}"
    );
    assert!(
        !loss_dropped(&report, "sampling.json_schema")
            && !loss_dropped(&report, "sampling.json_schema_name")
            && !loss_dropped(&report, "sampling.reasoning_effort"),
        "mapped outputConfig fields must not Drop, got {report:?}"
    );
}

#[test]
fn converse_decode_reads_output_config_schema_and_effort() {
    let req = br#"{
      "messages": [{"role": "user", "content": [{"text": "hi"}]}],
      "outputConfig": {
        "textFormat": {
          "type": "json_schema",
          "structure": {
            "jsonSchema": {
              "schema": "{\"type\":\"object\"}",
              "name": "answer"
            }
          }
        },
        "effort": "high"
      }
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode");
    assert_eq!(ir.sampling.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(ir.sampling.json_schema_name.as_deref(), Some("answer"));
    assert_eq!(
        ir.sampling.json_schema,
        Some(serde_json::json!({"type": "object"}))
    );
}

#[test]
fn converse_non_object_json_schema_is_dropped() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.json_schema = Some(serde_json::json!(["not", "object"]));
        s.json_schema_name = Some("answer".into());
    }));
    let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("outputConfig").is_none(),
        "non-object json_schema must not emit outputConfig, got {body}"
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
        "Converse has a slot; Drop detail must not be no slot, got {report:?}"
    );
}

#[test]
fn converse_encode_unknown_effort_is_dropped() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.reasoning_effort = Some("ultra".into());
    }));
    let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("outputConfig").is_none(),
        "unknown effort must not invent outputConfig, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.reasoning_effort"),
        "unknown effort must Drop, got {report:?}"
    );
}

#[test]
fn converse_encode_service_tier_auto_degrades_to_default() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.service_tier = Some("auto".into());
    }));
    let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.pointer("/serviceTier/type").and_then(Value::as_str),
        Some("default"),
        "auto must map to serviceTier.type default, got {body}"
    );
    assert!(
        loss_degraded(&report, "sampling.service_tier"),
        "auto must Degrade to default, got {report:?}"
    );
}

#[test]
fn converse_encode_service_tier_passthrough() {
    for (input, want) in [
        ("priority", "priority"),
        ("reserved", "reserved"),
        ("DEFAULT", "default"),
    ] {
        let ir = user_ir(IrSampling::patch(|s| {
            s.service_tier = Some(input.into());
        }));
        let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body.pointer("/serviceTier/type").and_then(Value::as_str),
            Some(want),
            "{input} must emit serviceTier.type {want}, got {body}"
        );
        assert!(
            !loss_dropped(&report, "sampling.service_tier")
                && !loss_degraded(&report, "sampling.service_tier"),
            "{input} must pass through, got {report:?}"
        );
    }
}

#[test]
fn converse_encode_unknown_service_tier_is_dropped() {
    let ir = user_ir(IrSampling::patch(|s| {
        s.service_tier = Some("turbo".into());
    }));
    let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("serviceTier").is_none(),
        "unknown service_tier must not invent serviceTier, got {body}"
    );
    assert!(
        loss_dropped(&report, "sampling.service_tier"),
        "unknown service_tier must Drop, got {report:?}"
    );
    assert!(
        !report.events.iter().any(|event| {
            event.path == "sampling.service_tier"
                && event.action == LossAction::Drop
                && event.detail == "no slot"
        }),
        "Converse has a slot; Drop detail must not be no slot, got {report:?}"
    );
}

#[test]
fn converse_decode_reads_service_tier() {
    let req = br#"{
      "messages": [{"role": "user", "content": [{"text": "hi"}]}],
      "serviceTier": { "type": "priority" }
    }"#;
    let (ir, _) = decode(Wire::Converse, req).expect("decode");
    assert_eq!(ir.sampling.service_tier.as_deref(), Some("priority"));
}

#[test]
fn converse_encode_x_high_effort_degrades_to_xhigh() {
    for effort in ["x-high", "X-High"] {
        let ir = user_ir(IrSampling::patch(|s| {
            s.reasoning_effort = Some(effort.into());
        }));
        let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body.pointer("/outputConfig/effort").and_then(Value::as_str),
            Some("xhigh"),
            "{effort} must map to outputConfig.effort xhigh, got {body}"
        );
        assert!(
            loss_degraded(&report, "sampling.reasoning_effort"),
            "{effort} must Degrade to xhigh, got {report:?}"
        );
    }
    for effort in ["xhigh", "XHIGH"] {
        let ir = user_ir(IrSampling::patch(|s| {
            s.reasoning_effort = Some(effort.into());
        }));
        let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body.pointer("/outputConfig/effort").and_then(Value::as_str),
            Some("xhigh"),
            "{effort} must emit xhigh, got {body}"
        );
        assert!(
            !loss_degraded(&report, "sampling.reasoning_effort")
                && !loss_dropped(&report, "sampling.reasoning_effort"),
            "{effort} is a first-class effort and must not Drop/Degrade, got {report:?}"
        );
    }
}

#[test]
fn converse_none_tool_choice_omits_tool_config() {
    let ir = IrRequest::new(
        "amazon.nova-lite-v1:0",
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    )
    .with_tools(vec![IrTool::Function {
        name: "lookup".into(),
        description: "d".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }])
    .with_sampling(IrSampling::patch(|s| {
        s.tool_choice = IrToolChoice::None;
    }));
    let (bytes, report) = encode(Wire::Converse, &ir, &converse_profile()).expect("encode");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("toolConfig").is_none(),
        "None must omit toolConfig; Bedrock has no none slot, got {body}"
    );
    assert!(
        body.pointer("/toolConfig/toolChoice/auto").is_none(),
        "None must not become auto, got {body}"
    );
    assert!(
        report.events.iter().any(|event| {
            event.path == "sampling.tool_choice"
                && matches!(event.action, LossAction::Degrade | LossAction::Drop)
                && event.detail.contains("no none slot")
        }),
        "None with tools must record tool_choice no none slot, got {report:?}"
    );
}

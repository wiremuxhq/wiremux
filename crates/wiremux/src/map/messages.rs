//! Anthropic Messages request map.

use serde_json::{Value, json};

use super::tools::{PreparedTool, decode_tool};
use super::{MapError, bool_field, f32_field, stop_values, str_field, u32_field, value_as_string};
use crate::ir::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction, LossReport,
};

pub(super) fn decode(value: &Value) -> Result<(IrRequest, LossReport), MapError> {
    let report = LossReport::default();
    let mut items = Vec::new();
    decode_system(value.get("system"), &mut items);

    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for msg in messages {
            decode_message(msg, &mut items);
        }
    }

    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(decode_tool).collect())
        .unwrap_or_default();

    let mut sampling = decode_sampling(value);
    sampling.cache = cache_from(value);

    let ir = IrRequest {
        model: str_field(value, "model").unwrap_or_default(),
        items,
        tools,
        sampling,
    };
    Ok((ir, report))
}

fn decode_system(system: Option<&Value>, items: &mut Vec<IrItem>) {
    let Some(system) = system else {
        return;
    };
    if let Some(text) = system.as_str() {
        items.push(IrItem::System {
            text: text.to_string(),
        });
        return;
    }
    let Some(arr) = system.as_array() else {
        return;
    };
    for block in arr {
        let text = block
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !text.is_empty() {
            items.push(IrItem::System { text });
        }
    }
}

fn decode_message(msg: &Value, items: &mut Vec<IrItem>) {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
    let content = msg.get("content");
    match role {
        "assistant" => decode_assistant(content, items),
        _ => decode_user(content, items),
    }
}

fn decode_assistant(content: Option<&Value>, items: &mut Vec<IrItem>) {
    if let Some(Value::String(text)) = content {
        items.push(IrItem::Assistant {
            parts: vec![IrPart::Text(text.clone())],
        });
        return;
    }
    let mut parts = Vec::new();
    for block in content_blocks(content) {
        match block.get("type").and_then(Value::as_str).unwrap_or("text") {
            "tool_use" => {
                flush_assistant(&mut parts, items);
                items.push(IrItem::FunctionCall {
                    call_id: str_field(block, "id").unwrap_or_default(),
                    name: str_field(block, "name").unwrap_or_default(),
                    arguments: block
                        .get("input")
                        .map(value_as_string)
                        .unwrap_or_else(|| "{}".into()),
                    thought_signature: None,
                });
            }
            "thinking" => parts.push(IrPart::Thinking {
                text: block
                    .get("thinking")
                    .or_else(|| block.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                signature: str_field(block, "signature"),
            }),
            _ => {
                if let Some(part) = decode_content_part(block) {
                    parts.push(part);
                }
            }
        }
    }
    flush_assistant(&mut parts, items);
}

fn decode_user(content: Option<&Value>, items: &mut Vec<IrItem>) {
    if let Some(Value::String(text)) = content {
        items.push(IrItem::User {
            parts: vec![IrPart::Text(text.clone())],
        });
        return;
    }
    let mut parts = Vec::new();
    for block in content_blocks(content) {
        match block.get("type").and_then(Value::as_str).unwrap_or("text") {
            "tool_result" => {
                flush_user(&mut parts, items);
                items.push(IrItem::FunctionOutput {
                    call_id: str_field(block, "tool_use_id").unwrap_or_default(),
                    output: tool_result_output(block),
                });
            }
            _ => {
                if let Some(part) = decode_content_part(block) {
                    parts.push(part);
                }
            }
        }
    }
    flush_user(&mut parts, items);
}

fn flush_assistant(parts: &mut Vec<IrPart>, items: &mut Vec<IrItem>) {
    if !parts.is_empty() {
        items.push(IrItem::Assistant {
            parts: std::mem::take(parts),
        });
    }
}

fn flush_user(parts: &mut Vec<IrPart>, items: &mut Vec<IrItem>) {
    if !parts.is_empty() {
        items.push(IrItem::User {
            parts: std::mem::take(parts),
        });
    }
}

fn content_blocks(content: Option<&Value>) -> Vec<&Value> {
    match content {
        Some(Value::Array(arr)) => arr.iter().collect(),
        _ => Vec::new(),
    }
}

fn decode_content_part(block: &Value) -> Option<IrPart> {
    if let Some(text) = block.as_str() {
        return Some(IrPart::Text(text.to_string()));
    }
    match block.get("type").and_then(Value::as_str).unwrap_or("text") {
        "text" => block
            .get("text")
            .and_then(Value::as_str)
            .map(|t| IrPart::Text(t.to_string())),
        "image" => decode_image(block),
        _ => block
            .get("text")
            .and_then(Value::as_str)
            .map(|t| IrPart::Text(t.to_string())),
    }
}

fn decode_image(block: &Value) -> Option<IrPart> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("url") => str_field(source, "url").map(IrPart::ImageUrl),
        Some("base64") => Some(IrPart::ImageBase64 {
            media_type: str_field(source, "media_type").unwrap_or_else(|| "image/png".into()),
            data: str_field(source, "data").unwrap_or_default(),
        }),
        _ => str_field(source, "url").map(IrPart::ImageUrl),
    }
}

fn tool_result_output(block: &Value) -> String {
    if let Some(s) = block.get("content").and_then(Value::as_str) {
        return s.to_string();
    }
    block
        .get("content")
        .map(value_as_string)
        .or_else(|| block.get("text").map(value_as_string))
        .unwrap_or_default()
}

fn decode_sampling(value: &Value) -> IrSampling {
    IrSampling {
        temperature: f32_field(value, "temperature"),
        top_p: f32_field(value, "top_p"),
        max_tokens: u32_field(value, "max_tokens"),
        stop: stop_values(value, &["stop_sequences", "stop"]),
        tool_choice: decode_tool_choice(value.get("tool_choice")),
        parallel_tool_calls: bool_field(value, "parallel_tool_calls"),
        store: bool_field(value, "store"),
        previous_response_id: str_field(value, "previous_response_id"),
        cache: IrCache::default(),
        stream: bool_field(value, "stream"),
        include_thoughts: None,
        thinking_budget: None,
    }
}

fn decode_tool_choice(value: Option<&Value>) -> IrToolChoice {
    let Some(value) = value else {
        return IrToolChoice::Auto;
    };
    if let Some(s) = value.as_str() {
        return match s {
            "none" => IrToolChoice::None,
            "any" | "required" => IrToolChoice::Required,
            _ => IrToolChoice::Auto,
        };
    }
    match value.get("type").and_then(Value::as_str) {
        Some("none") => IrToolChoice::None,
        Some("any") | Some("required") => IrToolChoice::Required,
        Some("tool") => IrToolChoice::Named(str_field(value, "name").unwrap_or_default()),
        _ => {
            if let Some(name) = str_field(value, "name") {
                IrToolChoice::Named(name)
            } else {
                IrToolChoice::Auto
            }
        }
    }
}

fn cache_from(value: &Value) -> IrCache {
    let mut found = None;
    visit_cache(value, &mut found);
    match found {
        Some(ttl) => IrCache {
            enabled: true,
            retention: ttl,
        },
        None => IrCache::default(),
    }
}

fn visit_cache(value: &Value, found: &mut Option<Option<String>>) {
    if found.is_some() {
        return;
    }
    match value {
        Value::Object(map) => {
            if let Some(cc) = map.get("cache_control") {
                let ttl = cc.get("ttl").and_then(Value::as_str).map(str::to_string);
                *found = Some(ttl);
                return;
            }
            for child in map.values() {
                visit_cache(child, found);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                visit_cache(child, found);
            }
        }
        _ => {}
    }
}

pub(super) fn encode(
    ir: &IrRequest,
    tools: &[PreparedTool],
    report: &mut LossReport,
) -> Result<Value, MapError> {
    let (system, messages) = encode_items(ir, report);
    let mut body = json!({
        "model": ir.model,
        "messages": messages,
    });
    if let Some(system) = system {
        body["system"] = system;
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| encode_tool(tool, ir.sampling.cache.enabled, &ir.sampling.cache))
                .collect(),
        );
    }
    encode_sampling(ir, &mut body, report);
    Ok(body)
}

fn encode_items(ir: &IrRequest, report: &mut LossReport) -> (Option<Value>, Value) {
    let mut system_blocks = Vec::new();
    let mut messages = Vec::new();
    let mut idx = 0;
    while idx < ir.items.len() {
        match &ir.items[idx] {
            IrItem::System { text } => {
                system_blocks.push(text_block(text, false, None));
                idx += 1;
            }
            IrItem::Developer { text } => {
                report.record(
                    format!("items[{idx}]"),
                    LossAction::Degrade,
                    "developer to system",
                );
                system_blocks.push(text_block(text, false, None));
                idx += 1;
            }
            IrItem::User { parts } => {
                let (msg, consumed) = encode_user(ir, idx, parts, report);
                messages.push(msg);
                idx += consumed;
            }
            IrItem::Assistant { parts } => {
                let (msg, consumed) = encode_assistant(ir, idx, parts, report);
                messages.push(msg);
                idx += consumed;
            }
            IrItem::FunctionCall {
                call_id,
                name,
                arguments,
                thought_signature,
            } => {
                if thought_signature.is_some() {
                    report.record(
                        format!("items[{idx}]"),
                        LossAction::Drop,
                        "thoughtSignature has no Messages slot",
                    );
                }
                messages.push(json!({
                    "role": "assistant",
                    "content": [tool_use_block(call_id, name, arguments)],
                }));
                idx += 1;
            }
            IrItem::FunctionOutput { call_id, output } => {
                messages.push(json!({
                    "role": "user",
                    "content": [tool_result_block(call_id, output)],
                }));
                idx += 1;
            }
            IrItem::Reasoning {
                encrypted,
                summary,
                raw,
            } => {
                idx += encode_reasoning(
                    &mut messages,
                    encrypted.as_deref(),
                    summary.as_deref(),
                    raw.as_ref(),
                    idx,
                    report,
                );
            }
            IrItem::HostedToolCall { kind, raw } => {
                report.record(
                    format!("items[{idx}]"),
                    LossAction::Preserve,
                    format!("hosted item `{kind}` passthrough"),
                );
                messages.push(raw.clone());
                idx += 1;
            }
            IrItem::Unknown { raw, .. } => {
                messages.push(raw.clone());
                idx += 1;
            }
        }
    }

    apply_cache_to_last_system(&mut system_blocks, &ir.sampling.cache, report);
    let system = if system_blocks.is_empty() {
        None
    } else if system_blocks.len() == 1
        && system_blocks[0].get("cache_control").is_none()
        && let Some(text) = system_blocks[0].get("text").cloned()
    {
        Some(text)
    } else {
        Some(Value::Array(system_blocks))
    };
    (system, Value::Array(messages))
}

fn encode_user(
    ir: &IrRequest,
    start: usize,
    parts: &[IrPart],
    _report: &mut LossReport,
) -> (Value, usize) {
    let mut consumed = 1;
    let mut content = encode_user_parts(parts);
    while let Some(IrItem::FunctionOutput { call_id, output }) = ir.items.get(start + consumed) {
        content.push(tool_result_block(call_id, output));
        consumed += 1;
    }
    (
        json!({
            "role": "user",
            "content": content,
        }),
        consumed,
    )
}

fn encode_assistant(
    ir: &IrRequest,
    start: usize,
    parts: &[IrPart],
    report: &mut LossReport,
) -> (Value, usize) {
    let mut consumed = 1;
    let mut content = encode_assistant_parts(parts);
    loop {
        match ir.items.get(start + consumed) {
            Some(IrItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            }) => {
                content.push(tool_use_block(call_id, name, arguments));
                consumed += 1;
            }
            Some(IrItem::Reasoning {
                encrypted,
                summary,
                raw,
            }) => {
                if let Some(block) =
                    reasoning_block(encrypted.as_deref(), summary.as_deref(), raw.as_ref())
                {
                    content.push(block);
                    report.record(
                        format!("items[{}]", start + consumed),
                        LossAction::Preserve,
                        "reasoning as thinking",
                    );
                } else {
                    report.record(
                        format!("items[{}]", start + consumed),
                        LossAction::Degrade,
                        "reasoning omitted (no thinking)",
                    );
                }
                consumed += 1;
            }
            _ => break,
        }
    }
    (
        json!({
            "role": "assistant",
            "content": content,
        }),
        consumed,
    )
}

fn encode_reasoning(
    messages: &mut Vec<Value>,
    encrypted: Option<&str>,
    summary: Option<&str>,
    raw: Option<&Value>,
    idx: usize,
    report: &mut LossReport,
) -> usize {
    if let Some(block) = reasoning_block(encrypted, summary, raw) {
        messages.push(json!({
            "role": "assistant",
            "content": [block],
        }));
        report.record(
            format!("items[{idx}]"),
            LossAction::Preserve,
            "reasoning as thinking",
        );
    } else {
        report.record(
            format!("items[{idx}]"),
            LossAction::Degrade,
            "reasoning omitted (no thinking)",
        );
    }
    1
}

fn reasoning_block(
    encrypted: Option<&str>,
    summary: Option<&str>,
    raw: Option<&Value>,
) -> Option<Value> {
    if let Some(raw) = raw
        && raw.get("type").and_then(Value::as_str) == Some("thinking")
    {
        return Some(raw.clone());
    }
    let text = summary
        .map(str::to_string)
        .or_else(|| raw.and_then(|v| str_field(v, "summary")));
    let text = text?;
    let mut block = json!({
        "type": "thinking",
        "thinking": text,
    });
    if let Some(sig) = encrypted {
        block["signature"] = json!(sig);
    }
    Some(block)
}

fn encode_user_parts(parts: &[IrPart]) -> Vec<Value> {
    if parts.is_empty() {
        return vec![json!({"type": "text", "text": ""})];
    }
    parts.iter().map(encode_part).collect()
}

fn encode_assistant_parts(parts: &[IrPart]) -> Vec<Value> {
    parts.iter().map(encode_part).collect()
}

fn encode_part(part: &IrPart) -> Value {
    match part {
        IrPart::Text(text) => json!({"type": "text", "text": text}),
        IrPart::ImageUrl(url) => json!({
            "type": "image",
            "source": {"type": "url", "url": url}
        }),
        IrPart::ImageBase64 { media_type, data } => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }),
        IrPart::Thinking { text, signature } => {
            let mut block = json!({"type": "thinking", "thinking": text});
            if let Some(sig) = signature {
                block["signature"] = json!(sig);
            }
            block
        }
    }
}

fn text_block(text: &str, cache: bool, retention: Option<&str>) -> Value {
    let mut block = json!({"type": "text", "text": text});
    if cache {
        let mut cc = json!({"type": "ephemeral"});
        if let Some(ttl) = retention {
            cc["ttl"] = json!(ttl);
        }
        block["cache_control"] = cc;
    }
    block
}

fn apply_cache_to_last_system(blocks: &mut [Value], cache: &IrCache, report: &mut LossReport) {
    if !cache.enabled {
        return;
    }
    report.record("sampling.cache", LossAction::Preserve, "messages cache");
    if let Some(last) = blocks.last_mut() {
        let mut cc = json!({"type": "ephemeral"});
        if let Some(ttl) = &cache.retention {
            cc["ttl"] = json!(ttl);
        }
        last["cache_control"] = cc;
    }
}

fn tool_use_block(call_id: &str, name: &str, arguments: &str) -> Value {
    let input = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!(arguments));
    json!({
        "type": "tool_use",
        "id": call_id,
        "name": name,
        "input": input,
    })
}

fn tool_result_block(call_id: &str, output: &str) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": output,
    })
}

fn encode_tool(tool: &PreparedTool, cache: bool, spec: &IrCache) -> Value {
    match tool {
        PreparedTool::Function {
            name,
            description,
            parameters,
        } => {
            let mut obj = json!({
                "name": name,
                "description": description,
                "input_schema": parameters,
            });
            if cache {
                let mut cc = json!({"type": "ephemeral"});
                if let Some(ttl) = &spec.retention {
                    cc["ttl"] = json!(ttl);
                }
                obj["cache_control"] = cc;
            }
            obj
        }
        PreparedTool::Raw(raw) => raw.clone(),
    }
}

fn encode_sampling(ir: &IrRequest, body: &mut Value, report: &mut LossReport) {
    let s = &ir.sampling;
    if let Some(t) = s.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = s.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(max) = s.max_tokens {
        body["max_tokens"] = json!(max);
    }
    if !s.stop.is_empty() {
        body["stop_sequences"] = json!(s.stop);
    }
    encode_tool_choice(&s.tool_choice, body);
    if s.parallel_tool_calls.is_some() {
        report.record(
            "sampling.parallel_tool_calls",
            LossAction::Preserve,
            "implicit parallel tool_use",
        );
    }
    if s.store.is_some() {
        report.record("sampling.store", LossAction::Drop, "no slot");
    }
    if s.previous_response_id.is_some() {
        report.record(
            "sampling.previous_response_id",
            LossAction::Degrade,
            "drop id, rebuild full input",
        );
    }
    if s.cache.enabled && body.get("system").is_none() {
        report.record("sampling.cache", LossAction::Preserve, "messages cache");
    }
    if let Some(stream) = s.stream {
        body["stream"] = json!(stream);
    }
}

fn encode_tool_choice(choice: &IrToolChoice, body: &mut Value) {
    match choice {
        IrToolChoice::Auto => {}
        IrToolChoice::None => body["tool_choice"] = json!({"type": "none"}),
        IrToolChoice::Required => body["tool_choice"] = json!({"type": "any"}),
        IrToolChoice::Named(name) => {
            body["tool_choice"] = json!({"type": "tool", "name": name});
        }
    }
}

//! Amazon Bedrock Converse request maps.

use serde_json::{Value, json};

use super::MapError;
use super::tools::PreparedTool;
use crate::ir::{IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction, LossReport};

pub(super) fn decode(value: &Value) -> Result<(IrRequest, LossReport), MapError> {
    let mut report = LossReport::default();
    let mut items = Vec::new();
    if let Some(system) = value.get("system") {
        decode_system(system, &mut items);
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for msg in messages {
            decode_message(msg, &mut items)?;
        }
    }
    let model = value
        .get("modelId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut sampling = decode_sampling(value);
    sampling.tool_choice =
        decode_tool_choice(value.get("toolConfig").and_then(|c| c.get("toolChoice")));
    let tools = value
        .get("toolConfig")
        .and_then(|c| c.get("tools"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let spec = t.get("toolSpec")?;
                    Some(super::tools::decode_tool(&json!({
                        "type": "function",
                        "function": {
                            "name": spec.get("name"),
                            "description": spec.get("description"),
                            "parameters": spec.pointer("/inputSchema/json").cloned().unwrap_or(json!({})),
                        }
                    })))
                })
                .collect()
        })
        .unwrap_or_default();
    if value.get("inferenceConfig").is_none() {
        report.record("inferenceConfig", LossAction::Drop, "absent");
    }
    Ok((
        IrRequest {
            model,
            items,
            tools,
            sampling,
        },
        report,
    ))
}

fn decode_system(system: &Value, items: &mut Vec<IrItem>) {
    let texts: Vec<String> = match system {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    for text in texts {
        if !text.is_empty() {
            items.push(IrItem::System { text });
        }
    }
}

fn decode_message(msg: &Value, items: &mut Vec<IrItem>) -> Result<(), MapError> {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
    let content = msg.get("content");
    match role {
        "user" => decode_user(content, items),
        "assistant" => decode_assistant(content, items),
        _ => Ok(()),
    }
}

fn decode_user(content: Option<&Value>, items: &mut Vec<IrItem>) -> Result<(), MapError> {
    let Some(arr) = content.and_then(Value::as_array) else {
        if let Some(s) = content.and_then(Value::as_str) {
            items.push(IrItem::User {
                parts: vec![IrPart::Text(s.to_string())],
            });
        }
        return Ok(());
    };
    let mut parts = Vec::new();
    for block in arr {
        if let Some(result) = block.get("toolResult") {
            let id = result
                .get("toolUseId")
                .and_then(Value::as_str)
                .ok_or_else(|| MapError::Invalid("toolResult omitted toolUseId".into()))?;
            let output = tool_result_output(result);
            items.push(IrItem::FunctionOutput {
                call_id: id.to_string(),
                output,
            });
            continue;
        }
        if let Some(part) = decode_part(block) {
            parts.push(part);
        }
    }
    if !parts.is_empty() {
        items.push(IrItem::User { parts });
    }
    Ok(())
}

fn decode_assistant(content: Option<&Value>, items: &mut Vec<IrItem>) -> Result<(), MapError> {
    let Some(arr) = content.and_then(Value::as_array) else {
        if let Some(s) = content.and_then(Value::as_str) {
            items.push(IrItem::Assistant {
                parts: vec![IrPart::Text(s.to_string())],
            });
        }
        return Ok(());
    };
    let mut parts = Vec::new();
    for block in arr {
        if let Some(tool) = block.get("toolUse") {
            flush_assistant(&mut parts, items);
            let id = tool
                .get("toolUseId")
                .and_then(Value::as_str)
                .ok_or_else(|| MapError::Invalid("toolUse omitted toolUseId".into()))?;
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = tool
                .get("input")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "{}".into());
            items.push(IrItem::FunctionCall {
                call_id: id.to_string(),
                name,
                arguments,
                thought_signature: None,
            });
            continue;
        }
        if let Some(part) = decode_part(block) {
            parts.push(part);
        }
    }
    flush_assistant(&mut parts, items);
    Ok(())
}

fn flush_assistant(parts: &mut Vec<IrPart>, items: &mut Vec<IrItem>) {
    if !parts.is_empty() {
        items.push(IrItem::Assistant {
            parts: std::mem::take(parts),
        });
    }
}

fn tool_result_output(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|c| {
            c.iter()
                .filter_map(|p| {
                    if let Some(v) = p.get("json") {
                        Some(v.to_string())
                    } else {
                        p.get("text").and_then(Value::as_str).map(str::to_string)
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn decode_part(block: &Value) -> Option<IrPart> {
    if let Some(text) = block.get("text").and_then(Value::as_str) {
        return Some(IrPart::Text(text.to_string()));
    }
    if let Some(reason) = block
        .pointer("/reasoningContent/reasoningText/text")
        .and_then(Value::as_str)
    {
        return Some(IrPart::Thinking {
            text: reason.to_string(),
            signature: None,
        });
    }
    None
}

fn decode_sampling(value: &Value) -> IrSampling {
    let mut sampling = IrSampling::default();
    let Some(cfg) = value.get("inferenceConfig") else {
        return sampling;
    };
    sampling.max_tokens = cfg
        .get("maxTokens")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    sampling.temperature = cfg
        .get("temperature")
        .and_then(Value::as_f64)
        .map(|n| n as f32);
    sampling.top_p = cfg.get("topP").and_then(Value::as_f64).map(|n| n as f32);
    if let Some(stops) = cfg.get("stopSequences").and_then(Value::as_array) {
        sampling.stop = stops
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }
    sampling
}

fn decode_tool_choice(value: Option<&Value>) -> IrToolChoice {
    let Some(value) = value else {
        return IrToolChoice::Auto;
    };
    if value.get("auto").is_some() {
        return IrToolChoice::Auto;
    }
    if value.get("any").is_some() {
        return IrToolChoice::Required;
    }
    if let Some(name) = value.pointer("/tool/name").and_then(Value::as_str) {
        return IrToolChoice::Named(name.to_string());
    }
    IrToolChoice::Auto
}

pub(super) fn encode(
    ir: &IrRequest,
    prepared: &[PreparedTool],
    report: &mut LossReport,
) -> Result<Value, MapError> {
    let (system, messages) = encode_items(ir, report);
    if messages.as_array().is_none_or(|a| a.is_empty()) {
        return Err(MapError::Invalid(
            "converse messages must not be empty".into(),
        ));
    }
    let mut body = json!({ "messages": messages });
    if let Some(system) = system {
        body["system"] = system;
    }
    encode_sampling(ir, &mut body);
    if !prepared.is_empty() {
        let tools: Vec<Value> = prepared.iter().filter_map(encode_tool).collect();
        if !tools.is_empty() {
            let mut cfg = json!({ "tools": tools });
            encode_tool_choice(&ir.sampling.tool_choice, &mut cfg);
            body["toolConfig"] = cfg;
        }
    }
    Ok(body)
}

fn encode_items(ir: &IrRequest, report: &mut LossReport) -> (Option<Value>, Value) {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for item in &ir.items {
        match item {
            IrItem::System { text } | IrItem::Developer { text } => {
                if !text.is_empty() {
                    system.push(json!({ "text": text }));
                }
            }
            IrItem::User { parts } => {
                let blocks: Vec<Value> = parts
                    .iter()
                    .filter_map(|p| encode_part(p, report))
                    .collect();
                if !blocks.is_empty() {
                    messages.push(json!({ "role": "user", "content": blocks }));
                }
            }
            IrItem::Assistant { parts } => {
                let blocks: Vec<Value> = parts
                    .iter()
                    .filter_map(|p| encode_part(p, report))
                    .collect();
                if blocks.is_empty() {
                    continue;
                }
                if let Some(last) = messages.last_mut()
                    && last.get("role").and_then(Value::as_str) == Some("assistant")
                    && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
                {
                    arr.extend(blocks);
                    continue;
                }
                messages.push(json!({ "role": "assistant", "content": blocks }));
            }
            IrItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let input: Value =
                    serde_json::from_str(arguments).unwrap_or_else(|_| json!(arguments));
                let block = json!({
                    "toolUse": {
                        "toolUseId": call_id,
                        "name": name,
                        "input": input
                    }
                });
                if let Some(last) = messages.last_mut()
                    && last.get("role").and_then(Value::as_str) == Some("assistant")
                    && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
                {
                    arr.push(block);
                    continue;
                }
                messages.push(json!({ "role": "assistant", "content": [block] }));
            }
            IrItem::FunctionOutput { call_id, output } => {
                let block = json!({
                    "toolResult": {
                        "toolUseId": call_id,
                        "content": [{ "text": output }]
                    }
                });
                if last_user_has_tool_result(&messages)
                    && let Some(last) = messages.last_mut()
                    && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
                {
                    arr.push(block);
                    continue;
                }
                messages.push(json!({ "role": "user", "content": [block] }));
            }
            IrItem::Reasoning { summary, .. } => {
                if let Some(text) = summary {
                    if let Some(last) = messages.last_mut()
                        && last.get("role").and_then(Value::as_str) == Some("assistant")
                        && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
                    {
                        arr.push(json!({
                            "reasoningContent": { "reasoningText": { "text": text } }
                        }));
                        continue;
                    }
                    messages.push(json!({
                        "role": "assistant",
                        "content": [{
                            "reasoningContent": { "reasoningText": { "text": text } }
                        }]
                    }));
                }
            }
            IrItem::HostedToolCall { kind, .. }
            | IrItem::Unknown {
                type_name: kind, ..
            } => {
                report.record(
                    "messages",
                    LossAction::Drop,
                    format!("converse has no slot for {kind}"),
                );
            }
        }
    }
    let system = if system.is_empty() {
        None
    } else {
        Some(Value::Array(system))
    };
    (system, Value::Array(messages))
}

fn last_user_has_tool_result(messages: &[Value]) -> bool {
    let Some(last) = messages.last() else {
        return false;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    last.get("content")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(|b| b.get("toolResult").is_some()))
}

fn encode_part(part: &IrPart, report: &mut LossReport) -> Option<Value> {
    match part {
        IrPart::Text(text) => Some(json!({ "text": text })),
        IrPart::Thinking { text, .. } => Some(json!({
            "reasoningContent": { "reasoningText": { "text": text } }
        })),
        IrPart::ImageUrl(_) | IrPart::ImageBase64 { .. } | IrPart::Raw { .. } => {
            report.record("content", LossAction::Drop, "converse image/raw dropped");
            None
        }
    }
}

fn encode_tool(tool: &PreparedTool) -> Option<Value> {
    match tool {
        PreparedTool::Function {
            name,
            description,
            parameters,
        } => Some(json!({
            "toolSpec": {
                "name": name,
                "description": description,
                "inputSchema": { "json": parameters }
            }
        })),
        PreparedTool::Raw(raw) => Some(raw.clone()),
    }
}

fn encode_sampling(ir: &IrRequest, body: &mut Value) {
    let s = &ir.sampling;
    let mut cfg = serde_json::Map::new();
    if let Some(n) = s.max_tokens {
        cfg.insert("maxTokens".into(), json!(n));
    }
    if let Some(t) = s.temperature {
        cfg.insert("temperature".into(), json!(t));
    }
    if let Some(p) = s.top_p {
        cfg.insert("topP".into(), json!(p));
    }
    if !s.stop.is_empty() {
        cfg.insert("stopSequences".into(), json!(s.stop));
    }
    if !cfg.is_empty() {
        body["inferenceConfig"] = Value::Object(cfg);
    }
}

fn encode_tool_choice(choice: &IrToolChoice, cfg: &mut Value) {
    match choice {
        IrToolChoice::Auto | IrToolChoice::None => {
            cfg["toolChoice"] = json!({ "auto": {} });
        }
        IrToolChoice::Required => {
            cfg["toolChoice"] = json!({ "any": {} });
        }
        IrToolChoice::Named(name) => {
            cfg["toolChoice"] = json!({ "tool": { "name": name } });
        }
    }
}

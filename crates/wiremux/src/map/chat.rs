//! Chat Completions request map.

use serde_json::{Value, json};

use super::tools::{PreparedTool, decode_tool};
use super::{MapError, bool_field, f32_field, stop_values, str_field, u32_field, value_as_string};
use crate::ir::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction, LossReport,
};

pub(super) fn decode(value: &Value) -> Result<(IrRequest, LossReport), MapError> {
    let report = LossReport::default();
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut items = Vec::new();
    for msg in &messages {
        decode_message(msg, &mut items);
    }

    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(decode_tool).collect())
        .unwrap_or_default();

    let ir = IrRequest {
        model: str_field(value, "model").unwrap_or_default(),
        items,
        tools,
        sampling: decode_sampling(value),
    };
    Ok((ir, report))
}

fn decode_message(msg: &Value, items: &mut Vec<IrItem>) {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
    let parts = decode_content(msg.get("content"));
    match role {
        "system" => items.push(IrItem::System {
            text: parts_text(&parts),
        }),
        "developer" => items.push(IrItem::Developer {
            text: parts_text(&parts),
        }),
        "assistant" => {
            if !parts.is_empty() || msg.get("tool_calls").is_none() {
                items.push(IrItem::Assistant { parts });
            }
            if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    items.push(decode_tool_call(call));
                }
            }
        }
        "tool" => items.push(IrItem::FunctionOutput {
            call_id: str_field(msg, "tool_call_id").unwrap_or_default(),
            output: parts_text(&parts),
        }),
        _ => items.push(IrItem::User { parts }),
    }
}

fn decode_tool_call(call: &Value) -> IrItem {
    let func = call.get("function").unwrap_or(call);
    IrItem::FunctionCall {
        call_id: str_field(call, "id").unwrap_or_default(),
        name: str_field(func, "name").unwrap_or_default(),
        arguments: func
            .get("arguments")
            .map(value_as_string)
            .unwrap_or_else(|| "{}".into()),
        thought_signature: None,
    }
}

fn decode_content(content: Option<&Value>) -> Vec<IrPart> {
    let Some(content) = content else {
        return Vec::new();
    };
    if let Some(text) = content.as_str() {
        return vec![IrPart::Text(text.to_string())];
    }
    let Some(arr) = content.as_array() else {
        return Vec::new();
    };
    arr.iter().filter_map(decode_part).collect()
}

fn decode_part(part: &Value) -> Option<IrPart> {
    if let Some(text) = part.as_str() {
        return Some(IrPart::Text(text.to_string()));
    }
    match part.get("type").and_then(Value::as_str).unwrap_or("text") {
        "text" => part
            .get("text")
            .and_then(Value::as_str)
            .map(|t| IrPart::Text(t.to_string())),
        "image_url" => {
            let url = part.get("image_url").and_then(|u| {
                u.as_str()
                    .map(str::to_string)
                    .or_else(|| str_field(u, "url"))
            })?;
            if let Some((media_type, data)) = super::split_data_url(&url) {
                Some(IrPart::ImageBase64 {
                    media_type: media_type.to_string(),
                    data: data.to_string(),
                })
            } else {
                Some(IrPart::ImageUrl(url))
            }
        }
        "thinking" => Some(IrPart::Thinking {
            text: part
                .get("text")
                .or_else(|| part.get("thinking"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            signature: str_field(part, "signature"),
        }),
        _ => part
            .get("text")
            .and_then(Value::as_str)
            .map(|t| IrPart::Text(t.to_string())),
    }
}

fn parts_text(parts: &[IrPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            IrPart::Text(text) => Some(text.as_str()),
            IrPart::Thinking { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn decode_sampling(value: &Value) -> IrSampling {
    let (json_schema, json_schema_name) = chat_json_schema(value);
    IrSampling {
        temperature: f32_field(value, "temperature"),
        top_p: f32_field(value, "top_p"),
        max_tokens: u32_field(value, "max_tokens"),
        stop: stop_values(value, &["stop"]),
        tool_choice: decode_tool_choice(value.get("tool_choice")),
        parallel_tool_calls: bool_field(value, "parallel_tool_calls"),
        store: bool_field(value, "store"),
        previous_response_id: str_field(value, "previous_response_id"),
        cache: IrCache::default(),
        stream: bool_field(value, "stream"),
        include_thoughts: None,
        thinking_budget: None,
        reasoning_effort: str_field(value, "reasoning_effort").filter(|s| !s.trim().is_empty()),
        max_reasoning_tokens: u32_field(value, "max_reasoning_tokens"),
        json_schema,
        json_schema_name,
    }
}

fn chat_json_schema(value: &Value) -> (Option<Value>, Option<String>) {
    let format = value.get("response_format");
    let Some(format) = format else {
        return (None, None);
    };
    if format.get("type").and_then(Value::as_str) != Some("json_schema") {
        return (None, None);
    }
    let js = format.get("json_schema");
    let schema = js.and_then(|js| js.get("schema")).cloned();
    let name = js
        .and_then(|js| js.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (schema, name)
}

fn decode_tool_choice(value: Option<&Value>) -> IrToolChoice {
    let Some(value) = value else {
        return IrToolChoice::Auto;
    };
    if let Some(s) = value.as_str() {
        return match s {
            "none" => IrToolChoice::None,
            "required" => IrToolChoice::Required,
            _ => IrToolChoice::Auto,
        };
    }
    let Some(obj) = value.as_object() else {
        return IrToolChoice::Auto;
    };
    if let Some(name) = obj
        .get("function")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .or_else(|| obj.get("name").and_then(Value::as_str))
    {
        return IrToolChoice::Named(name.to_string());
    }
    match obj.get("type").and_then(Value::as_str) {
        Some("none") => IrToolChoice::None,
        Some("required") | Some("any") => IrToolChoice::Required,
        _ => IrToolChoice::Auto,
    }
}

pub(super) fn encode(
    ir: &IrRequest,
    tools: &[PreparedTool],
    report: &mut LossReport,
) -> Result<Value, MapError> {
    let mut body = json!({
        "model": ir.model,
        "messages": encode_messages(ir, report),
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(encode_tool).collect());
    }
    encode_sampling(ir, &mut body, report);
    Ok(body)
}

fn encode_messages(ir: &IrRequest, report: &mut LossReport) -> Value {
    let mut messages = Vec::new();
    let mut idx = 0;
    while idx < ir.items.len() {
        match &ir.items[idx] {
            IrItem::System { text } => {
                messages.push(json!({"role": "system", "content": text}));
                idx += 1;
            }
            IrItem::Developer { text } => {
                report.record(format!("items[{idx}]"), LossAction::Preserve, "developer");
                messages.push(json!({"role": "developer", "content": text}));
                idx += 1;
            }
            IrItem::User { parts } => {
                messages.push(json!({"role": "user", "content": encode_parts(parts, report)}));
                idx += 1;
            }
            IrItem::Assistant { parts } => {
                let (msg, consumed) = encode_assistant(ir, idx, parts, report);
                messages.push(msg);
                idx += consumed;
            }
            IrItem::FunctionCall { .. } => {
                let (msg, consumed) = encode_standalone_function_calls(ir, idx, report);
                messages.push(msg);
                idx += consumed;
            }
            IrItem::FunctionOutput { call_id, output } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output,
                }));
                idx += 1;
            }
            IrItem::Reasoning { .. } => {
                report.record(
                    format!("items[{idx}]"),
                    LossAction::Degrade,
                    "reasoning omitted on Chat Completions",
                );
                idx += 1;
            }
            IrItem::HostedToolCall { kind, raw } => {
                messages.push(raw.clone());
                report.record(
                    format!("items[{idx}]"),
                    LossAction::Preserve,
                    format!("hosted item `{kind}` passthrough"),
                );
                idx += 1;
            }
            IrItem::Unknown { raw, .. } => {
                messages.push(raw.clone());
                idx += 1;
            }
        }
    }
    Value::Array(messages)
}

fn encode_assistant(
    ir: &IrRequest,
    start: usize,
    parts: &[IrPart],
    report: &mut LossReport,
) -> (Value, usize) {
    let (calls, extra) = take_function_calls(ir, start + 1, report);
    let mut msg = json!({
        "role": "assistant",
        "content": encode_parts(parts, report),
    });
    if !calls.is_empty() {
        msg["tool_calls"] = Value::Array(calls);
    }
    (msg, 1 + extra)
}

fn encode_standalone_function_calls(
    ir: &IrRequest,
    start: usize,
    report: &mut LossReport,
) -> (Value, usize) {
    let (calls, consumed) = take_function_calls(ir, start, report);
    (
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": calls,
        }),
        consumed,
    )
}

fn take_function_calls(
    ir: &IrRequest,
    start: usize,
    report: &mut LossReport,
) -> (Vec<Value>, usize) {
    let mut consumed = 0;
    let mut calls = Vec::new();
    while let Some(IrItem::FunctionCall {
        call_id,
        name,
        arguments,
        thought_signature,
    }) = ir.items.get(start + consumed)
    {
        if thought_signature.is_some() {
            report.record(
                format!("items[{}]", start + consumed),
                LossAction::Drop,
                "thoughtSignature has no Chat Completions slot",
            );
        }
        calls.push(function_call_json(call_id, name, arguments));
        consumed += 1;
    }
    (calls, consumed)
}

fn function_call_json(call_id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        }
    })
}

fn encode_parts(parts: &[IrPart], report: &mut LossReport) -> Value {
    let visible: Vec<&IrPart> = parts
        .iter()
        .filter(|part| match part {
            IrPart::Thinking { .. } => {
                report.record(
                    "part.thinking",
                    LossAction::Drop,
                    "thinking has no Chat Completions slot",
                );
                false
            }
            IrPart::Raw { .. } => {
                report.record(
                    "part.raw",
                    LossAction::Drop,
                    "raw part has no Chat Completions slot",
                );
                false
            }
            _ => true,
        })
        .collect();
    if visible.is_empty() {
        return Value::String(String::new());
    }
    if visible.len() == 1
        && let IrPart::Text(text) = visible[0]
    {
        return Value::String(text.clone());
    }
    Value::Array(
        visible
            .iter()
            .map(|part| match part {
                IrPart::Text(text) => json!({"type": "text", "text": text}),
                IrPart::ImageUrl(url) => json!({"type": "image_url", "image_url": {"url": url}}),
                IrPart::ImageBase64 { media_type, data } => json!({
                    "type": "image_url",
                    "image_url": {"url": format!("data:{media_type};base64,{data}")}
                }),
                IrPart::Thinking { .. } | IrPart::Raw { .. } => unreachable!("filtered"),
            })
            .collect(),
    )
}

fn encode_tool(tool: &PreparedTool) -> Value {
    match tool {
        PreparedTool::Function {
            name,
            description,
            parameters,
        } => json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": parameters,
            }
        }),
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
        body["stop"] = json!(s.stop);
    }
    encode_tool_choice(&s.tool_choice, body);
    if let Some(parallel) = s.parallel_tool_calls {
        body["parallel_tool_calls"] = json!(parallel);
        report.record(
            "sampling.parallel_tool_calls",
            LossAction::Preserve,
            "chat parallel_tool_calls",
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
    if s.cache.enabled {
        report.record("sampling.cache", LossAction::Drop, "no slot");
    }
    if let Some(stream) = s.stream {
        body["stream"] = json!(stream);
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
    }
    if let Some(effort) = s
        .reasoning_effort
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        body["reasoning_effort"] = json!(effort);
    }
    if s.max_reasoning_tokens.is_some() {
        report.record("sampling.max_reasoning_tokens", LossAction::Drop, "no slot");
    }
    if s.include_thoughts.is_some() {
        report.record("sampling.include_thoughts", LossAction::Drop, "no slot");
    }
    if s.thinking_budget.is_some() {
        report.record("sampling.thinking_budget", LossAction::Drop, "no slot");
    }
    if let Some(schema) = &s.json_schema
        && let Some((schema, name)) =
            super::official_json_schema(schema, s.json_schema_name.as_deref(), report)
    {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": name,
                "schema": schema,
            },
        });
    }
}

fn encode_tool_choice(choice: &IrToolChoice, body: &mut Value) {
    match choice {
        IrToolChoice::Auto => {}
        IrToolChoice::None => body["tool_choice"] = json!("none"),
        IrToolChoice::Required => body["tool_choice"] = json!("required"),
        IrToolChoice::Named(name) => {
            body["tool_choice"] = json!({
                "type": "function",
                "function": {"name": name}
            });
        }
    }
}

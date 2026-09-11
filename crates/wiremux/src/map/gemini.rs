//! Gemini generateContent request map.

use serde_json::{Value, json};

use super::tools::PreparedTool;
use super::{MapError, bool_field, f32_field, stop_values, str_field, u32_field};
use crate::ir::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction, LossReport,
};

pub(super) fn decode(value: &Value) -> Result<(IrRequest, LossReport), MapError> {
    let report = LossReport::default();
    let mut items = Vec::new();
    if let Some(sys) = system_text(value.get("systemInstruction")) {
        items.push(IrItem::System { text: sys });
    }
    if let Some(contents) = value.get("contents").and_then(Value::as_array) {
        for content in contents {
            decode_content(content, &mut items);
        }
    }
    let tools = decode_tools(value);
    let ir = IrRequest {
        model: str_field(value, "model").unwrap_or_default(),
        items,
        tools,
        sampling: decode_sampling(value),
    };
    Ok((ir, report))
}

fn system_text(value: Option<&Value>) -> Option<String> {
    let parts = value?.get("parts").and_then(Value::as_array)?;
    let text: String = parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

fn decode_content(content: &Value, items: &mut Vec<IrItem>) {
    let role = content
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let parts = content.get("parts").and_then(Value::as_array);
    let Some(parts) = parts else {
        return;
    };
    let mut text_parts = Vec::new();
    for part in parts {
        if let Some(fc) = part.get("functionCall") {
            flush_assistant(&mut text_parts, items);
            let name = str_field(fc, "name").unwrap_or_default();
            let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
            items.push(IrItem::FunctionCall {
                call_id: name.clone(),
                name,
                arguments: args.to_string(),
                thought_signature: part
                    .get("thoughtSignature")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            });
            continue;
        }
        if let Some(fr) = part.get("functionResponse") {
            flush_assistant(&mut text_parts, items);
            let name = str_field(fr, "name").unwrap_or_default();
            let output = fr
                .get("response")
                .map(ToString::to_string)
                .unwrap_or_else(|| "{}".into());
            items.push(IrItem::FunctionOutput {
                call_id: name,
                output,
            });
            continue;
        }
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                text_parts.push(IrPart::Thinking {
                    text: text.to_string(),
                    signature: part
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
            continue;
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            text_parts.push(IrPart::Text(text.to_string()));
        }
        if let Some(inline) = part.get("inlineData") {
            let media = str_field(inline, "mimeType").unwrap_or_default();
            let data = str_field(inline, "data").unwrap_or_default();
            text_parts.push(IrPart::ImageBase64 {
                media_type: media,
                data,
            });
        }
    }
    if text_parts.is_empty() {
        return;
    }
    match role {
        "model" => items.push(IrItem::Assistant { parts: text_parts }),
        _ => items.push(IrItem::User { parts: text_parts }),
    }
}

fn flush_assistant(parts: &mut Vec<IrPart>, items: &mut Vec<IrItem>) {
    if parts.is_empty() {
        return;
    }
    items.push(IrItem::Assistant {
        parts: std::mem::take(parts),
    });
}

fn decode_tools(value: &Value) -> Vec<crate::ir::IrTool> {
    let Some(tools) = value.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tool in tools {
        let Some(decls) = tool.get("functionDeclarations").and_then(Value::as_array) else {
            continue;
        };
        for decl in decls {
            out.push(crate::ir::IrTool::Function {
                name: str_field(decl, "name").unwrap_or_default(),
                description: str_field(decl, "description").unwrap_or_default(),
                parameters: decl
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            });
        }
    }
    out
}

fn decode_sampling(value: &Value) -> IrSampling {
    let cfg = value.get("generationConfig").unwrap_or(value);
    let thinking = thinking_config_obj(value);
    IrSampling {
        temperature: f32_field(cfg, "temperature"),
        top_p: f32_field(cfg, "topP").or_else(|| f32_field(cfg, "top_p")),
        max_tokens: u32_field(cfg, "maxOutputTokens").or_else(|| u32_field(cfg, "max_tokens")),
        stop: stop_values(cfg, &["stopSequences", "stop"]),
        tool_choice: IrToolChoice::Auto,
        parallel_tool_calls: bool_field(value, "parallel_tool_calls"),
        store: None,
        previous_response_id: None,
        cache: IrCache::default(),
        stream: bool_field(value, "stream"),
        include_thoughts: bool_field(thinking, "includeThoughts")
            .or_else(|| bool_field(thinking, "include_thoughts")),
        thinking_budget: u32_field(thinking, "thinkingBudget")
            .or_else(|| u32_field(thinking, "thinking_budget")),
        reasoning_effort: None,
        max_reasoning_tokens: None,
    }
}

fn thinking_config_obj(value: &Value) -> &Value {
    value
        .get("thinkingConfig")
        .or_else(|| value.pointer("/generationConfig/thinkingConfig"))
        .unwrap_or(&Value::Null)
}

pub(super) fn encode(
    ir: &IrRequest,
    prepared: &[PreparedTool],
    report: &mut LossReport,
) -> Result<Value, MapError> {
    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    let mut call_names = Vec::new();
    for item in &ir.items {
        match item {
            IrItem::System { text } | IrItem::Developer { text } => {
                if matches!(item, IrItem::Developer { .. }) {
                    report.record(
                        "item.developer",
                        LossAction::Degrade,
                        "developer to systemInstruction",
                    );
                }
                system_parts.push(json!({ "text": text }));
            }
            IrItem::User { parts } => {
                for part in encode_parts(parts) {
                    push_role_part(&mut contents, "user", part);
                }
            }
            IrItem::Assistant { parts } => {
                for part in encode_parts(parts) {
                    push_role_part(&mut contents, "model", part);
                }
            }
            IrItem::FunctionCall {
                call_id,
                name,
                arguments,
                thought_signature,
            } => {
                call_names.push((call_id.as_str(), name.as_str()));
                let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
                let mut part = json!({ "functionCall": { "name": name, "args": args } });
                if let Some(sig) = thought_signature.as_deref().filter(|s| !s.is_empty()) {
                    part["thoughtSignature"] = json!(sig);
                }
                push_role_part(&mut contents, "model", part);
            }
            IrItem::FunctionOutput { call_id, output } => {
                let name = call_names
                    .iter()
                    .rev()
                    .find_map(|(id, name)| (*id == call_id).then_some(*name))
                    .unwrap_or(call_id.as_str());
                let response: Value =
                    serde_json::from_str(output).unwrap_or_else(|_| json!({ "result": output }));
                push_role_part(
                    &mut contents,
                    "user",
                    json!({
                        "functionResponse": { "name": name, "response": response }
                    }),
                );
            }
            IrItem::Reasoning { .. } => {
                report.record(
                    "item.reasoning",
                    LossAction::Drop,
                    "no generateContent slot",
                );
            }
            IrItem::HostedToolCall { kind, .. }
            | IrItem::Unknown {
                type_name: kind, ..
            } => {
                report.record(
                    "item.unknown",
                    LossAction::Drop,
                    format!("gemini cannot express {kind}"),
                );
            }
        }
    }

    let mut body = json!({ "contents": contents });
    if !ir.model.is_empty() {
        body["model"] = json!(ir.model);
    }
    if !system_parts.is_empty() {
        body["systemInstruction"] = json!({ "parts": system_parts });
    }
    let decls: Vec<Value> = prepared
        .iter()
        .filter_map(|tool| match tool {
            PreparedTool::Function {
                name,
                description,
                parameters,
            } => Some(json!({
                "name": name,
                "description": description,
                "parameters": parameters,
            })),
            PreparedTool::Raw(_) => None,
        })
        .collect();
    if !decls.is_empty() {
        body["tools"] = json!([{ "functionDeclarations": decls }]);
    }
    encode_sampling(ir, &mut body, report);
    Ok(body)
}

fn push_role_part(contents: &mut Vec<Value>, role: &str, part: Value) {
    if let Some(last) = contents.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(parts) = last.get_mut("parts").and_then(Value::as_array_mut)
    {
        parts.push(part);
        return;
    }
    contents.push(json!({ "role": role, "parts": [part] }));
}

fn encode_parts(parts: &[IrPart]) -> Vec<Value> {
    let mut out = Vec::new();
    for part in parts {
        match part {
            IrPart::Text(text) => out.push(json!({ "text": text })),
            IrPart::Thinking { text, signature } => {
                let mut obj = json!({ "text": text, "thought": true });
                if let Some(sig) = signature {
                    obj["thoughtSignature"] = json!(sig);
                }
                out.push(obj);
            }
            IrPart::ImageBase64 { media_type, data } => {
                out.push(json!({
                    "inlineData": { "mimeType": media_type, "data": data }
                }));
            }
            IrPart::Raw { .. } => {}
            IrPart::ImageUrl(url) => {
                if let Some(rest) = url.strip_prefix("data:")
                    && let Some((mime, b64)) = rest.split_once(";base64,")
                {
                    out.push(json!({
                        "inlineData": { "mimeType": mime, "data": b64 }
                    }));
                } else {
                    out.push(json!({ "text": format!("[image: {url}]") }));
                }
            }
        }
    }
    out
}

fn encode_sampling(ir: &IrRequest, body: &mut Value, report: &mut LossReport) {
    let s = &ir.sampling;
    let mut cfg = json!({});
    if let Some(t) = s.temperature {
        cfg["temperature"] = json!(t);
    }
    if let Some(p) = s.top_p {
        cfg["topP"] = json!(p);
    }
    if let Some(max) = s.max_tokens {
        cfg["maxOutputTokens"] = json!(max);
    }
    if !s.stop.is_empty() {
        cfg["stopSequences"] = json!(s.stop);
    }
    if cfg.as_object().is_some_and(|o| !o.is_empty()) {
        body["generationConfig"] = cfg;
    }
    if s.store.is_some() {
        report.record("sampling.store", LossAction::Drop, "no slot");
    }
    if s.previous_response_id.is_some() {
        report.record("sampling.previous_response_id", LossAction::Drop, "no slot");
    }
    if s.cache.enabled {
        report.record("sampling.cache", LossAction::Drop, "no slot");
    }
    if let Some(stream) = s.stream {
        body["stream"] = json!(stream);
    }
    if s.include_thoughts.is_some() || s.thinking_budget.is_some() {
        let mut tc = json!({});
        if let Some(include) = s.include_thoughts {
            tc["includeThoughts"] = json!(include);
        }
        if let Some(budget) = s.thinking_budget {
            tc["thinkingBudget"] = json!(budget);
        }
        body["thinkingConfig"] = tc;
    }
    if s.reasoning_effort
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        report.record("sampling.reasoning_effort", LossAction::Drop, "no slot");
    }
    if s.max_reasoning_tokens.is_some() {
        report.record("sampling.max_reasoning_tokens", LossAction::Drop, "no slot");
    }
}

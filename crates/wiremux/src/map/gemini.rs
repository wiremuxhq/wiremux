//! Gemini generateContent request map.

use serde_json::{Value, json};

use super::tools::PreparedTool;
use super::{MapError, bool_field, f32_field, stop_values, str_field, u32_field};
use crate::ir::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction, LossReport,
};

pub(super) fn decode(value: &Value) -> Result<(IrRequest, LossReport), MapError> {
    let mut report = LossReport::default();
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
        sampling: decode_sampling(value, &mut report),
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
            flush_parts(role, &mut text_parts, items);
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
            flush_parts(role, &mut text_parts, items);
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
        if part.get("fileData").is_some() {
            text_parts.push(IrPart::Raw {
                type_name: "fileData".into(),
                raw: part.clone(),
            });
        } else if part.get("fileUri").is_some() {
            text_parts.push(IrPart::Raw {
                type_name: "fileUri".into(),
                raw: part.clone(),
            });
        }
    }
    flush_parts(role, &mut text_parts, items);
}

fn flush_parts(role: &str, parts: &mut Vec<IrPart>, items: &mut Vec<IrItem>) {
    if parts.is_empty() {
        return;
    }
    let taken = std::mem::take(parts);
    match role {
        "model" => items.push(IrItem::Assistant { parts: taken }),
        _ => items.push(IrItem::User { parts: taken }),
    }
}

fn decode_tools(value: &Value) -> Vec<crate::ir::IrTool> {
    let Some(tools) = value.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tool in tools {
        let Some(obj) = tool.as_object() else {
            continue;
        };
        if let Some(decls) = obj.get("functionDeclarations").and_then(Value::as_array) {
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
            for (key, val) in obj {
                if key == "functionDeclarations" {
                    continue;
                }
                out.push(crate::ir::IrTool::Unknown {
                    type_name: key.clone(),
                    raw: json!({ key: val.clone() }),
                });
            }
            continue;
        }
        let Some(type_name) = obj.keys().next() else {
            continue;
        };
        out.push(crate::ir::IrTool::Unknown {
            type_name: type_name.clone(),
            raw: tool.clone(),
        });
    }
    out
}

fn decode_sampling(value: &Value, report: &mut LossReport) -> IrSampling {
    let cfg = value.get("generationConfig").unwrap_or(value);
    let thinking = thinking_config_obj(value);
    let tool_choice = decode_tool_choice(value);
    if gemini_source_has_unmapped_tool_choice(value) {
        report.record("sampling.tool_choice", LossAction::Drop, "no slot");
    }
    let json_schema = gemini_json_schema(cfg);
    if gemini_source_has_json_schema(value) && json_schema.is_none() {
        report.record(
            "sampling.json_schema",
            LossAction::Drop,
            "json_schema requires object schema",
        );
    }
    IrSampling {
        temperature: f32_field(cfg, "temperature"),
        top_p: f32_field(cfg, "topP").or_else(|| f32_field(cfg, "top_p")),
        max_tokens: u32_field(cfg, "maxOutputTokens").or_else(|| u32_field(cfg, "max_tokens")),
        stop: stop_values(cfg, &["stopSequences", "stop"]),
        tool_choice,
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
        json_schema,
        json_schema_name: None,
        include: Vec::new(),
    }
}

fn gemini_json_schema(cfg: &Value) -> Option<Value> {
    cfg.get("responseSchema")
        .or_else(|| cfg.get("responseJsonSchema"))
        .or_else(|| cfg.get("response_schema"))
        .or_else(|| cfg.get("response_json_schema"))
        .filter(|v| v.is_object())
        .cloned()
}

fn thinking_config_obj(value: &Value) -> &Value {
    value
        .get("thinkingConfig")
        .or_else(|| value.pointer("/generationConfig/thinkingConfig"))
        .unwrap_or(&Value::Null)
}

fn decode_tool_choice(value: &Value) -> IrToolChoice {
    let Some(fcc) = gemini_function_calling_config(value) else {
        return IrToolChoice::Auto;
    };
    let named = fcc
        .get("allowedFunctionNames")
        .or_else(|| fcc.get("allowed_function_names"))
        .and_then(Value::as_array)
        .and_then(|names| names.iter().find_map(|n| n.as_str()))
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    match str_field(fcc, "mode").as_deref() {
        Some("NONE") | Some("none") => IrToolChoice::None,
        Some("ANY") | Some("any") => match named {
            Some(name) => IrToolChoice::Named(name),
            None => IrToolChoice::Required,
        },
        _ => IrToolChoice::Auto,
    }
}

fn gemini_tool_config(value: &Value) -> Option<&Value> {
    value.get("toolConfig").or_else(|| value.get("tool_config"))
}

fn gemini_function_calling_config(value: &Value) -> Option<&Value> {
    let cfg = gemini_tool_config(value)?;
    cfg.get("functionCallingConfig")
        .or_else(|| cfg.get("function_calling_config"))
}

fn gemini_source_has_unmapped_tool_choice(value: &Value) -> bool {
    value.get("tool_choice").is_some() && gemini_tool_config(value).is_none()
}

fn gemini_source_has_json_schema(value: &Value) -> bool {
    let cfg = value.get("generationConfig").unwrap_or(value);
    cfg.get("responseSchema").is_some()
        || cfg.get("responseJsonSchema").is_some()
        || cfg.get("response_schema").is_some()
        || cfg.get("response_json_schema").is_some()
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
                for part in encode_parts(parts, report) {
                    push_role_part(&mut contents, "user", part);
                }
            }
            IrItem::Assistant { parts } => {
                for part in encode_parts(parts, report) {
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
                let args: Value =
                    serde_json::from_str(arguments).unwrap_or_else(|_| json!(arguments));
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
    let tools = encode_prepared_tools(prepared, report);
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    encode_sampling(ir, &mut body, report);
    Ok(body)
}

fn encode_prepared_tools(prepared: &[PreparedTool], report: &mut LossReport) -> Vec<Value> {
    let mut decls = Vec::new();
    let mut hosted = Vec::new();
    for (i, tool) in prepared.iter().enumerate() {
        match tool {
            PreparedTool::Function {
                name,
                description,
                parameters,
            } => decls.push(json!({
                "name": name,
                "description": description,
                "parameters": parameters,
            })),
            PreparedTool::Raw(raw) if is_gemini_hosted_raw(raw) => hosted.push(raw.clone()),
            PreparedTool::Raw(_) => {
                report.record(
                    format!("tools[{i}]"),
                    LossAction::Drop,
                    "raw tool has no generateContent slot",
                );
            }
        }
    }
    let mut tools = Vec::new();
    if !decls.is_empty() {
        tools.push(json!({ "functionDeclarations": decls }));
    }
    tools.extend(hosted);
    tools
}

fn is_gemini_hosted_raw(raw: &Value) -> bool {
    let Some(obj) = raw.as_object() else {
        return false;
    };
    obj.contains_key("googleSearch")
        || obj.contains_key("codeExecution")
        || obj.contains_key("googleSearchRetrieval")
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

fn encode_parts(parts: &[IrPart], report: &mut LossReport) -> Vec<Value> {
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
            IrPart::Raw { raw, .. } => {
                if raw.get("fileData").is_some() || raw.get("fileUri").is_some() {
                    out.push(raw.clone());
                } else {
                    report.record(
                        "part.raw",
                        LossAction::Drop,
                        "raw part has no generateContent slot",
                    );
                }
            }
            IrPart::ImageUrl(url) => {
                if let Some(rest) = url.strip_prefix("data:")
                    && let Some((mime, b64)) = rest.split_once(";base64,")
                {
                    out.push(json!({
                        "inlineData": { "mimeType": mime, "data": b64 }
                    }));
                } else {
                    report.record(
                        "part.image_url",
                        LossAction::Degrade,
                        "url to text placeholder",
                    );
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
    let thinking_budget = s
        .thinking_budget
        .or_else(|| s.max_reasoning_tokens.filter(|&n| n > 0));
    if s.include_thoughts.is_some() || thinking_budget.is_some() {
        let mut tc = json!({});
        if let Some(include) = s.include_thoughts {
            tc["includeThoughts"] = json!(include);
        }
        if let Some(budget) = thinking_budget {
            tc["thinkingBudget"] = json!(budget);
        }
        cfg["thinkingConfig"] = tc;
    }
    if let Some(schema) = &s.json_schema {
        if schema.is_object() {
            cfg["responseMimeType"] = json!("application/json");
            cfg["responseSchema"] = schema.clone();
        } else {
            report.record(
                "sampling.json_schema",
                LossAction::Drop,
                "json_schema requires object schema",
            );
        }
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
    if s.reasoning_effort
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        report.record("sampling.reasoning_effort", LossAction::Drop, "no slot");
    }
    let used_max_as_budget =
        s.thinking_budget.is_none() && s.max_reasoning_tokens.is_some_and(|n| n > 0);
    if s.max_reasoning_tokens.is_some() && !used_max_as_budget {
        report.record("sampling.max_reasoning_tokens", LossAction::Drop, "no slot");
    }
    encode_tool_choice(&s.tool_choice, body);
    if s.parallel_tool_calls.is_some() {
        report.record("sampling.parallel_tool_calls", LossAction::Drop, "no slot");
    }
    if !s.include.is_empty() {
        report.record("sampling.include", LossAction::Drop, "no slot");
    }
}

fn encode_tool_choice(choice: &IrToolChoice, body: &mut Value) {
    let fcc = match choice {
        IrToolChoice::Auto => return,
        IrToolChoice::None => json!({ "mode": "NONE" }),
        IrToolChoice::Required => json!({ "mode": "ANY" }),
        IrToolChoice::Named(name) => json!({
            "mode": "ANY",
            "allowedFunctionNames": [name],
        }),
    };
    body["toolConfig"] = json!({ "functionCallingConfig": fcc });
}

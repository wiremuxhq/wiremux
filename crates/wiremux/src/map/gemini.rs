//! Gemini generateContent request map.

use serde_json::{Value, json};

use super::tools::PreparedTool;
use super::{
    MapError, audio_format_from_mime, audio_mime_from_format, bool_field, document_ref_source,
    f32_field, is_audio_media_type, is_pdf_media_type, stop_values, str_field, u32_field,
};
use crate::ir::{
    IrCache, IrDocumentSource, IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction,
    LossReport,
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
                call_id: crate::stream::gemini_call_id(fc, &name, items.len()),
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
            let call_id = fr
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    items.iter().rev().find_map(|item| match item {
                        IrItem::FunctionCall {
                            call_id,
                            name: call_name,
                            ..
                        } if call_name == &name => Some(call_id.clone()),
                        _ => None,
                    })
                })
                .unwrap_or(name);
            items.push(IrItem::FunctionOutput { call_id, output });
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
            if is_audio_media_type(&media) {
                text_parts.push(IrPart::Audio {
                    data,
                    format: audio_format_from_mime(&media),
                });
            } else if is_pdf_media_type(&media) {
                text_parts.push(IrPart::Document {
                    source: IrDocumentSource::Base64(data),
                    media_type: media,
                    name: None,
                });
            } else {
                text_parts.push(IrPart::ImageBase64 {
                    media_type: media,
                    data,
                });
            }
        }
        if let Some(file) = part.get("fileData") {
            let media = str_field(file, "mimeType").unwrap_or_default();
            let uri = str_field(file, "fileUri").unwrap_or_default();
            if is_pdf_media_type(&media) && !uri.is_empty() {
                text_parts.push(IrPart::Document {
                    source: document_ref_source(uri),
                    media_type: media,
                    name: None,
                });
            } else {
                text_parts.push(IrPart::Raw {
                    type_name: "fileData".into(),
                    raw: part.clone(),
                });
            }
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
    let json_schema_name = json_schema
        .as_ref()
        .is_some()
        .then(|| "response".to_string());
    let json_object = gemini_json_object(cfg, json_schema.is_some());
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
        reasoning_effort: str_field(thinking, "thinkingLevel")
            .or_else(|| str_field(thinking, "thinking_level"))
            .filter(|s| !s.trim().is_empty()),
        max_reasoning_tokens: None,
        json_schema,
        json_schema_name,
        json_object,
        include: Vec::new(),
        prompt_cache_key: None,
        service_tier: None,
        user: None,
    }
}

fn gemini_json_object(cfg: &Value, has_schema: bool) -> Option<bool> {
    if has_schema {
        return None;
    }
    let mime = cfg
        .get("responseMimeType")
        .or_else(|| cfg.get("response_mime_type"))
        .and_then(Value::as_str)?;
    (mime == "application/json").then_some(true)
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

/// Official `thinkingLevel` is `low` | `medium` | `high` | `minimal`.
/// `xhigh` / `x-high` degrade to `high`. Other nonempty values emit lowercase.
fn gemini_thinking_level(effort: &str) -> Option<(String, Option<&'static str>)> {
    let trimmed = effort.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "xhigh" | "x-high" => Some(("high".into(), Some("xhigh maps to high"))),
        _ => Some((lower, None)),
    }
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
                let mut part = json!({
                    "functionCall": { "id": call_id, "name": name, "args": args }
                });
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
                        "functionResponse": {
                            "id": call_id,
                            "name": name,
                            "response": response
                        }
                    }),
                );
            }
            IrItem::Reasoning {
                encrypted: _,
                summary,
                raw,
            } => {
                let text = summary
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| raw.as_ref().and_then(|v| str_field(v, "summary")));
                if let Some(text) = text.filter(|t| !t.is_empty()) {
                    // Do not copy OpenAI encrypted_content onto thoughtSignature.
                    push_role_part(
                        &mut contents,
                        "model",
                        json!({ "text": text, "thought": true }),
                    );
                } else {
                    report.record(
                        "item.reasoning",
                        LossAction::Drop,
                        "reasoning omitted on generateContent",
                    );
                }
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
            IrPart::Document {
                source, media_type, ..
            } => out.push(encode_document(source, media_type)),
            IrPart::Audio { data, format } => {
                out.push(json!({
                    "inlineData": {
                        "mimeType": audio_mime_from_format(format),
                        "data": data
                    }
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

fn encode_document(source: &IrDocumentSource, media_type: &str) -> Value {
    match source {
        IrDocumentSource::Base64(data) => json!({
            "inlineData": { "mimeType": media_type, "data": data }
        }),
        IrDocumentSource::Url(uri) | IrDocumentSource::FileId(uri) => {
            let mut file = json!({ "fileUri": uri });
            if !media_type.is_empty() {
                file["mimeType"] = json!(media_type);
            }
            json!({ "fileData": file })
        }
    }
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
    let thinking_budget = s.thinking_budget.or(s.max_reasoning_tokens);
    let thinking_level = s
        .reasoning_effort
        .as_deref()
        .and_then(gemini_thinking_level);
    if s.include_thoughts.is_some() || thinking_budget.is_some() || thinking_level.is_some() {
        let mut tc = json!({});
        if let Some(include) = s.include_thoughts {
            tc["includeThoughts"] = json!(include);
        }
        if let Some(budget) = thinking_budget {
            tc["thinkingBudget"] = json!(budget);
        }
        if let Some((level, degrade)) = thinking_level {
            tc["thinkingLevel"] = json!(level);
            if let Some(detail) = degrade {
                report.record("sampling.reasoning_effort", LossAction::Degrade, detail);
            }
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
    } else if s.json_object == Some(true) {
        cfg["responseMimeType"] = json!("application/json");
    }
    if cfg.as_object().is_some_and(|o| !o.is_empty()) {
        body["generationConfig"] = cfg;
    }
    if s.store.is_some() {
        report.record("sampling.store", LossAction::Drop, "no slot");
    }
    if s.prompt_cache_key.is_some() {
        report.record("sampling.prompt_cache_key", LossAction::Drop, "no slot");
    }
    if s.user.is_some() {
        report.record("sampling.user", LossAction::Drop, "no slot");
    }
    if s.service_tier.is_some() {
        report.record("sampling.service_tier", LossAction::Drop, "no slot");
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
    let used_max_as_budget = s.thinking_budget.is_none() && s.max_reasoning_tokens.is_some();
    if s.max_reasoning_tokens.is_some() && !used_max_as_budget {
        let detail = if s.thinking_budget.is_some() {
            "thinking_budget sibling won"
        } else {
            "no slot"
        };
        report.record("sampling.max_reasoning_tokens", LossAction::Drop, detail);
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

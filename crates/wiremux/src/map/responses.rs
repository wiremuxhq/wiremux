//! OpenAI Responses request map.

use serde_json::{Value, json};
use wiremux_auth::{ResolvedProfile, ToolTypePolicy};

use super::tools::{PreparedTool, decode_tool, qualify_call_name, split_namespace_name};
use super::{
    MapError, bool_field, decode_input_audio_part, decode_openai_file_part, f32_field,
    off_dialect_raw_path, responses_raw_passthrough, stop_values, str_field, u32_field,
    value_as_string,
};
use crate::ir::{
    IrCache, IrDocumentSource, IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction,
    LossReport,
};

pub(super) fn decode(value: &Value) -> Result<(IrRequest, LossReport), MapError> {
    let report = LossReport::default();
    let mut items = Vec::new();
    if let Some(instructions) = str_field(value, "instructions")
        && !instructions.is_empty()
    {
        items.push(IrItem::System { text: instructions });
    }

    match value.get("input") {
        Some(Value::String(text)) => items.push(IrItem::User {
            parts: vec![IrPart::Text(text.clone())],
        }),
        Some(Value::Array(arr)) => {
            for item in arr {
                items.extend(decode_input_item(item));
            }
        }
        _ => {}
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

fn decode_input_item(item: &Value) -> Vec<IrItem> {
    if let Some(text) = item.as_str() {
        return vec![IrItem::User {
            parts: vec![IrPart::Text(text.to_string())],
        }];
    }
    let ty = item.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "function_call" => vec![IrItem::FunctionCall {
            call_id: str_field(item, "call_id")
                .or_else(|| str_field(item, "id"))
                .unwrap_or_default(),
            name: qualify_call_name(item),
            arguments: item
                .get("arguments")
                .map(value_as_string)
                .unwrap_or_else(|| "{}".into()),
            thought_signature: None,
        }],
        "function_call_output" => vec![IrItem::FunctionOutput {
            call_id: str_field(item, "call_id").unwrap_or_default(),
            output: item.get("output").map(value_as_string).unwrap_or_default(),
        }],
        "reasoning" => vec![IrItem::Reasoning {
            encrypted: str_field(item, "encrypted_content"),
            summary: reasoning_summary(item),
            raw: Some(item.clone()),
        }],
        "message" | "" => decode_message_item(item),
        other if is_hosted_item(other) => vec![IrItem::HostedToolCall {
            kind: other.to_string(),
            raw: item.clone(),
        }],
        other => vec![IrItem::Unknown {
            type_name: other.to_string(),
            raw: item.clone(),
        }],
    }
}

fn is_hosted_item(ty: &str) -> bool {
    ty.ends_with("_call") || ty == "web_search_call" || ty == "file_search_call"
}

fn reasoning_summary(item: &Value) -> Option<String> {
    if let Some(s) = str_field(item, "summary") {
        return Some(s);
    }
    let arr = item.get("summary")?.as_array()?;
    let texts: Vec<_> = arr
        .iter()
        .filter_map(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .or_else(|| part.as_str())
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

fn decode_message_item(item: &Value) -> Vec<IrItem> {
    let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
    let parts = decode_parts(item.get("content"));
    match role {
        "system" => vec![IrItem::System {
            text: parts_text(&parts),
        }],
        "developer" => vec![IrItem::Developer {
            text: parts_text(&parts),
        }],
        "assistant" => vec![IrItem::Assistant { parts }],
        _ => vec![IrItem::User { parts }],
    }
}

fn decode_parts(content: Option<&Value>) -> Vec<IrPart> {
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
    let type_name = part.get("type").and_then(Value::as_str).unwrap_or("text");
    match type_name {
        "input_text" | "output_text" | "text" => part
            .get("text")
            .and_then(Value::as_str)
            .map(|t| IrPart::Text(t.to_string())),
        "input_image" | "image" => decode_image(part),
        "input_file" | "file" => decode_openai_file_part(part),
        "input_audio" => decode_input_audio_part(part),
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
            .map(|t| IrPart::Text(t.to_string()))
            .or_else(|| {
                Some(IrPart::Raw {
                    type_name: type_name.to_string(),
                    raw: part.clone(),
                })
            }),
    }
}

fn decode_image(part: &Value) -> Option<IrPart> {
    if let Some(url) = part
        .get("image_url")
        .and_then(|u| {
            u.as_str()
                .map(str::to_string)
                .or_else(|| str_field(u, "url"))
        })
        .or_else(|| str_field(part, "url"))
    {
        if let Some((media_type, data)) = super::split_data_url(&url) {
            return Some(IrPart::ImageBase64 {
                media_type: media_type.to_string(),
                data: data.to_string(),
            });
        }
        return Some(IrPart::ImageUrl(url));
    }
    if let Some(data) =
        str_field(part, "data").or_else(|| part.get("source").and_then(|s| str_field(s, "data")))
    {
        return Some(IrPart::ImageBase64 {
            media_type: str_field(part, "media_type")
                .or_else(|| part.get("source").and_then(|s| str_field(s, "media_type")))
                .unwrap_or_else(|| "image/png".into()),
            data,
        });
    }
    None
}

fn parts_text(parts: &[IrPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            IrPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn decode_sampling(value: &Value) -> IrSampling {
    let (json_schema, json_schema_name) = responses_json_schema(value);
    let json_object = responses_json_object(value);
    IrSampling {
        temperature: f32_field(value, "temperature"),
        top_p: f32_field(value, "top_p"),
        max_tokens: u32_field(value, "max_output_tokens")
            .or_else(|| u32_field(value, "max_tokens")),
        stop: stop_values(value, &["stop"]),
        tool_choice: decode_tool_choice(value.get("tool_choice")),
        parallel_tool_calls: bool_field(value, "parallel_tool_calls"),
        store: bool_field(value, "store"),
        previous_response_id: str_field(value, "previous_response_id"),
        cache: IrCache::default(),
        stream: bool_field(value, "stream"),
        include_thoughts: value
            .get("reasoning")
            .and_then(|r| str_field(r, "summary"))
            .filter(|s| !s.trim().is_empty())
            .map(|_| true),
        thinking_budget: None,
        reasoning_effort: value
            .get("reasoning")
            .and_then(|r| str_field(r, "effort"))
            .filter(|s| !s.trim().is_empty()),
        max_reasoning_tokens: value
            .get("reasoning")
            .and_then(|r| u32_field(r, "max_tokens")),
        json_schema,
        json_schema_name,
        json_object,
        include: decode_include(value),
        prompt_cache_key: str_field(value, "prompt_cache_key").filter(|s| !s.trim().is_empty()),
        service_tier: str_field(value, "service_tier").filter(|s| !s.trim().is_empty()),
        user: str_field(value, "user").filter(|s| !s.trim().is_empty()),
    }
}

fn decode_include(value: &Value) -> Vec<String> {
    let Some(arr) = value.get("include").and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect()
}

fn encode_include(extras: &[String]) -> Vec<String> {
    const ENCRYPTED: &str = "reasoning.encrypted_content";
    let mut include = Vec::with_capacity(extras.len() + 1);
    if !extras.iter().any(|item| item == ENCRYPTED) {
        include.push(ENCRYPTED.to_string());
    }
    for extra in extras {
        if !include.iter().any(|item| item == extra) {
            include.push(extra.clone());
        }
    }
    include
}

fn responses_json_object(value: &Value) -> Option<bool> {
    let format = value.get("text").and_then(|t| t.get("format"))?;
    (format.get("type").and_then(Value::as_str) == Some("json_object")).then_some(true)
}

fn responses_json_schema(value: &Value) -> (Option<Value>, Option<String>) {
    let format = value.get("text").and_then(|t| t.get("format"));
    let Some(format) = format else {
        return (None, None);
    };
    if format.get("type").and_then(Value::as_str) != Some("json_schema") {
        return (None, None);
    }
    let schema = format.get("schema").cloned();
    let name = format
        .get("name")
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
    if let Some(name) = value.get("name").and_then(Value::as_str).or_else(|| {
        value
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
    }) {
        return IrToolChoice::Named(name.to_string());
    }
    match value.get("type").and_then(Value::as_str) {
        Some("none") => IrToolChoice::None,
        Some("required") => IrToolChoice::Required,
        _ => IrToolChoice::Auto,
    }
}

pub(super) fn encode(
    ir: &IrRequest,
    tools: &[PreparedTool],
    profile: &ResolvedProfile,
    report: &mut LossReport,
) -> Result<Value, MapError> {
    // Split dotted IR names only when keeping native namespace tools (hard-error).
    // flatten-namespace emits dotted function names on the wire, including Responses.
    let restore_calls = matches!(profile.dialect.tool_type_policy, ToolTypePolicy::HardError);
    let (instructions, input) = encode_items(ir, restore_calls, report);
    let mut body = json!({
        "model": ir.model,
        "input": input,
    });
    if let Some(instructions) = instructions {
        body["instructions"] = json!(instructions);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(encode_tool).collect());
    }
    encode_sampling(ir, &mut body, report);
    Ok(body)
}

fn encode_items(
    ir: &IrRequest,
    restore_calls: bool,
    report: &mut LossReport,
) -> (Option<String>, Value) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    for (idx, item) in ir.items.iter().enumerate() {
        match item {
            IrItem::System { text } => instructions.push(text.clone()),
            IrItem::Developer { text } => {
                report.record(
                    format!("items[{idx}]"),
                    LossAction::Degrade,
                    "developer to system",
                );
                instructions.push(text.clone());
            }
            IrItem::User { parts } => input.push(json!({
                "role": "user",
                "content": encode_parts(parts, true, report),
            })),
            IrItem::Assistant { parts } => {
                let rest = peel_signed_thinking(parts, &mut input, report);
                if !rest.is_empty() || !parts.iter().any(is_signed_thinking) {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": encode_parts(&rest, false, report),
                    }));
                }
            }
            IrItem::FunctionCall {
                call_id,
                name,
                arguments,
                thought_signature,
            } => {
                if thought_signature.is_some() {
                    report.record(
                        "item.function_call",
                        LossAction::Drop,
                        "thoughtSignature has no Responses slot",
                    );
                }
                input.push(encode_function_call(
                    call_id,
                    name,
                    arguments,
                    restore_calls,
                ));
            }
            IrItem::FunctionOutput { call_id, output } => input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output,
            })),
            IrItem::Reasoning {
                encrypted,
                summary,
                raw,
            } => input.push(encode_reasoning(
                encrypted.as_deref(),
                summary.as_deref(),
                raw.as_ref(),
            )),
            IrItem::HostedToolCall { raw, .. } | IrItem::Unknown { raw, .. } => {
                input.push(raw.clone());
            }
        }
    }
    let instructions = if instructions.is_empty() {
        None
    } else {
        Some(instructions.join("\n\n"))
    };
    (instructions, Value::Array(input))
}

fn encode_function_call(call_id: &str, name: &str, arguments: &str, restore: bool) -> Value {
    if restore && let Some((ns, leaf)) = split_namespace_name(name) {
        return json!({
            "type": "function_call",
            "call_id": call_id,
            "name": leaf,
            "namespace": ns,
            "arguments": arguments,
        });
    }
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    })
}

fn encode_reasoning(encrypted: Option<&str>, summary: Option<&str>, raw: Option<&Value>) -> Value {
    if let Some(raw) = raw {
        return raw.clone();
    }
    let mut obj = json!({"type": "reasoning"});
    if let Some(enc) = encrypted {
        obj["encrypted_content"] = json!(enc);
    }
    if let Some(summary) = summary {
        obj["summary"] = json!([{"type": "summary_text", "text": summary}]);
    }
    obj
}

fn is_signed_thinking(part: &IrPart) -> bool {
    matches!(
        part,
        IrPart::Thinking {
            signature: Some(sig),
            ..
        } if !sig.is_empty()
    )
}

fn peel_signed_thinking(
    parts: &[IrPart],
    input: &mut Vec<Value>,
    report: &mut LossReport,
) -> Vec<IrPart> {
    let mut rest = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            IrPart::Thinking { text, signature }
                if signature.as_deref().is_some_and(|sig| !sig.is_empty()) =>
            {
                let summary = if text.is_empty() {
                    None
                } else {
                    Some(text.as_str())
                };
                input.push(encode_reasoning(signature.as_deref(), summary, None));
                report.record(
                    "part.thinking",
                    LossAction::Preserve,
                    "signed thinking remapped to reasoning",
                );
            }
            other => rest.push(other.clone()),
        }
    }
    rest
}

fn encode_parts(parts: &[IrPart], input: bool, report: &mut LossReport) -> Value {
    let text_ty = if input { "input_text" } else { "output_text" };
    let visible: Vec<&IrPart> = parts
        .iter()
        .filter(|part| match part {
            IrPart::Thinking { .. } => {
                report.record(
                    "part.thinking",
                    LossAction::Drop,
                    "thinking has no Responses slot",
                );
                false
            }
            IrPart::Raw { raw, .. } => {
                if responses_raw_passthrough(raw) {
                    true
                } else {
                    report.record(
                        off_dialect_raw_path(raw),
                        LossAction::Drop,
                        "raw part is not Responses-shaped",
                    );
                    false
                }
            }
            _ => true,
        })
        .collect();
    if visible.len() == 1
        && let IrPart::Text(text) = visible[0]
        && input
    {
        return Value::Array(vec![json!({"type": text_ty, "text": text})]);
    }
    Value::Array(
        visible
            .iter()
            .map(|part| match part {
                IrPart::Text(text) => json!({"type": text_ty, "text": text}),
                IrPart::ImageUrl(url) => json!({"type": "input_image", "image_url": url}),
                IrPart::ImageBase64 { media_type, data } => json!({
                    "type": "input_image",
                    "image_url": format!("data:{media_type};base64,{data}")
                }),
                IrPart::Document {
                    source,
                    media_type,
                    name,
                } => encode_document(source, media_type, name.as_deref()),
                IrPart::Audio { data, format } => json!({
                    "type": "input_audio",
                    "input_audio": { "data": data, "format": format }
                }),
                IrPart::Raw { raw, .. } => raw.clone(),
                IrPart::Thinking { .. } => unreachable!("filtered"),
            })
            .collect(),
    )
}

fn encode_document(source: &IrDocumentSource, media_type: &str, name: Option<&str>) -> Value {
    let mut obj = json!({ "type": "input_file" });
    if let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) {
        obj["filename"] = json!(name);
    }
    match source {
        IrDocumentSource::Base64(data) => {
            let media_type = if media_type.is_empty() {
                "application/pdf"
            } else {
                media_type
            };
            if obj.get("filename").and_then(Value::as_str).is_none() {
                obj["filename"] = json!(super::document_filename(None, media_type));
            }
            obj["file_data"] = json!(format!("data:{media_type};base64,{data}"));
        }
        IrDocumentSource::Url(url) => {
            obj["file_url"] = json!(url);
        }
        IrDocumentSource::FileId(id) => {
            obj["file_id"] = json!(id);
        }
    }
    obj
}

fn encode_tool(tool: &PreparedTool) -> Value {
    match tool {
        PreparedTool::Function {
            name,
            description,
            parameters,
        } => json!({
            "type": "function",
            "name": name,
            "description": description,
            "parameters": parameters,
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
        body["max_output_tokens"] = json!(max);
    }
    if !s.stop.is_empty() {
        report.record("sampling.stop", LossAction::Drop, "no slot");
    }
    encode_tool_choice(&s.tool_choice, body);
    if let Some(parallel) = s.parallel_tool_calls {
        body["parallel_tool_calls"] = json!(parallel);
        report.record(
            "sampling.parallel_tool_calls",
            LossAction::Preserve,
            "responses parallel_tool_calls",
        );
    }
    if let Some(store) = s.store {
        body["store"] = json!(store);
        report.record("sampling.store", LossAction::Preserve, "responses store");
    }
    if let Some(id) = &s.previous_response_id {
        body["previous_response_id"] = json!(id);
        report.record(
            "sampling.previous_response_id",
            LossAction::Preserve,
            "responses previous_response_id",
        );
    }
    if s.cache.enabled {
        report.record("sampling.cache", LossAction::Drop, "no slot");
    }
    if let Some(key) = s
        .prompt_cache_key
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        body["prompt_cache_key"] = json!(key);
        report.record(
            "sampling.prompt_cache_key",
            LossAction::Preserve,
            "responses prompt_cache_key",
        );
    }
    if let Some(user) = s.user.as_deref().filter(|s| !s.trim().is_empty()) {
        body["user"] = json!(user);
    }
    if let Some(tier) = s.service_tier.as_deref().filter(|s| !s.trim().is_empty()) {
        body["service_tier"] = json!(tier);
        report.record(
            "sampling.service_tier",
            LossAction::Preserve,
            "responses service_tier",
        );
    }
    if let Some(stream) = s.stream {
        body["stream"] = json!(stream);
    }
    body["include"] = json!(encode_include(&s.include));
    if let Some(effort) = s
        .reasoning_effort
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        body["reasoning"] = json!({ "effort": effort });
    }
    if s.include_thoughts == Some(true) {
        if !body.get("reasoning").is_some_and(Value::is_object) {
            body["reasoning"] = json!({});
        }
        body["reasoning"]["summary"] = json!("auto");
        report.record(
            "sampling.include_thoughts",
            LossAction::Preserve,
            "responses reasoning.summary",
        );
    } else if s.include_thoughts == Some(false) {
        report.record("sampling.include_thoughts", LossAction::Drop, "no slot");
    }
    if s.max_reasoning_tokens.is_some() {
        report.record("sampling.max_reasoning_tokens", LossAction::Drop, "no slot");
    }
    if s.thinking_budget.is_some() {
        report.record("sampling.thinking_budget", LossAction::Drop, "no slot");
    }
    if let Some(schema) = &s.json_schema
        && let Some((schema, name)) =
            super::official_json_schema(schema, s.json_schema_name.as_deref(), report)
    {
        body["text"] = json!({
            "format": {
                "type": "json_schema",
                "name": name,
                "schema": schema,
            }
        });
    } else if s.json_object == Some(true) {
        body["text"] = json!({ "format": { "type": "json_object" } });
    }
}

fn encode_tool_choice(choice: &IrToolChoice, body: &mut Value) {
    match choice {
        IrToolChoice::Auto => {}
        IrToolChoice::None => body["tool_choice"] = json!("none"),
        IrToolChoice::Required => body["tool_choice"] = json!("required"),
        IrToolChoice::Named(name) => {
            body["tool_choice"] = json!({"type": "function", "name": name});
        }
    }
}

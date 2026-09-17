//! Anthropic Messages request map.

use serde_json::{Value, json};

use super::tools::{PreparedTool, decode_tool};
use super::{
    MapError, bool_field, f32_field, messages_raw_passthrough, off_dialect_raw_path, stop_values,
    str_field, u32_field, value_as_string,
};
use crate::ir::{
    IrCache, IrDocumentSource, IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction,
    LossReport,
};

pub(super) fn decode(value: &Value) -> Result<(IrRequest, LossReport), MapError> {
    let mut report = LossReport::default();
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

    let mut sampling = decode_sampling(value, &mut report);
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
            "redacted_thinking" => parts.push(IrPart::Raw {
                type_name: "redacted_thinking".into(),
                raw: block.clone(),
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
    let type_name = block.get("type").and_then(Value::as_str).unwrap_or("text");
    match type_name {
        "text" => block
            .get("text")
            .and_then(Value::as_str)
            .map(|t| IrPart::Text(t.to_string())),
        "image" => decode_image(block),
        "document" => decode_document(block).or_else(|| {
            Some(IrPart::Raw {
                type_name: "document".into(),
                raw: block.clone(),
            })
        }),
        _ => block
            .get("text")
            .and_then(Value::as_str)
            .map(|t| IrPart::Text(t.to_string()))
            .or_else(|| {
                Some(IrPart::Raw {
                    type_name: type_name.to_string(),
                    raw: block.clone(),
                })
            }),
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

fn decode_document(block: &Value) -> Option<IrPart> {
    let source = block.get("source")?;
    let media_type = str_field(source, "media_type")
        .or_else(|| str_field(block, "media_type"))
        .unwrap_or_default();
    let name = str_field(block, "title")
        .or_else(|| str_field(block, "name"))
        .filter(|s| !s.is_empty());
    let src = match source.get("type").and_then(Value::as_str) {
        Some("url") => IrDocumentSource::Url(str_field(source, "url")?),
        Some("file") => IrDocumentSource::FileId(
            str_field(source, "file_id").or_else(|| str_field(source, "id"))?,
        ),
        Some("base64") => IrDocumentSource::Base64(str_field(source, "data")?),
        _ => {
            if let Some(data) = str_field(source, "data") {
                IrDocumentSource::Base64(data)
            } else if let Some(url) = str_field(source, "url") {
                IrDocumentSource::Url(url)
            } else if let Some(id) = str_field(source, "file_id") {
                IrDocumentSource::FileId(id)
            } else {
                return None;
            }
        }
    };
    Some(IrPart::Document {
        source: src,
        media_type: if media_type.is_empty() {
            "application/pdf".into()
        } else {
            media_type
        },
        name,
    })
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

fn decode_sampling(value: &Value, report: &mut LossReport) -> IrSampling {
    if messages_source_has_json_schema(value) {
        report.record("sampling.json_schema", LossAction::Drop, "no slot");
    }
    let (include_thoughts, max_reasoning_tokens) = decode_thinking(value, report);
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
        include_thoughts,
        thinking_budget: None,
        reasoning_effort: None,
        max_reasoning_tokens,
        json_schema: None,
        json_schema_name: None,
        json_object: None,
        include: Vec::new(),
        prompt_cache_key: None,
        service_tier: None,
        user: None,
    }
}

fn decode_thinking(value: &Value, report: &mut LossReport) -> (Option<bool>, Option<u32>) {
    let Some(thinking) = value.get("thinking") else {
        return (None, None);
    };
    match thinking.get("type").and_then(Value::as_str) {
        Some("disabled") => (Some(false), None),
        Some("enabled") => (Some(true), u32_field(thinking, "budget_tokens")),
        _ => {
            report.record("sampling.thinking", LossAction::Drop, "unknown thinking");
            (None, None)
        }
    }
}

fn messages_source_has_json_schema(value: &Value) -> bool {
    value.get("output_format").is_some()
        || value.get("response_format").is_some()
        || value.get("json_schema").is_some()
        || value.pointer("/text/format/type").and_then(Value::as_str) == Some("json_schema")
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
            min_cacheable_tokens: None,
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
        body["tools"] = Value::Array(tools.iter().map(encode_tool).collect());
    }
    encode_sampling(ir, &mut body, report);
    apply_cache_breakpoints(&mut body, ir, report);
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
                    "content": [tool_use_block(call_id, name, arguments, format!("items[{idx}]"), report)],
                }));
                idx += 1;
            }
            IrItem::FunctionOutput { call_id, output } => {
                messages.push(json!({
                    "role": "user",
                    "content": [tool_result_block(call_id, output, format!("items[{idx}]"), report)],
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

    if messages_need_continue(ir) {
        messages.push(json!({
            "role": "user",
            "content": [text_block("Continue.", false, None)],
        }));
    }

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

fn messages_need_continue(ir: &IrRequest) -> bool {
    matches!(
        ir.items.last(),
        Some(IrItem::Assistant { .. } | IrItem::FunctionCall { .. })
    )
}

fn encode_user(
    ir: &IrRequest,
    start: usize,
    parts: &[IrPart],
    report: &mut LossReport,
) -> (Value, usize) {
    let mut consumed = 1;
    let mut content = encode_user_parts(parts, report);
    while let Some(IrItem::FunctionOutput { call_id, output }) = ir.items.get(start + consumed) {
        content.push(tool_result_block(
            call_id,
            output,
            format!("items[{}]", start + consumed),
            report,
        ));
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
    let mut content = encode_assistant_parts(parts, report);
    loop {
        match ir.items.get(start + consumed) {
            Some(IrItem::FunctionCall {
                call_id,
                name,
                arguments,
                thought_signature,
            }) => {
                if thought_signature.is_some() {
                    report.record(
                        format!("items[{}]", start + consumed),
                        LossAction::Drop,
                        "thoughtSignature has no Messages slot",
                    );
                }
                content.push(tool_use_block(
                    call_id,
                    name,
                    arguments,
                    format!("items[{}]", start + consumed),
                    report,
                ));
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
    if content.is_empty() {
        content.push(json!({"type": "text", "text": "."}));
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

fn encode_user_parts(parts: &[IrPart], report: &mut LossReport) -> Vec<Value> {
    let out: Vec<Value> = parts
        .iter()
        .filter_map(|part| encode_part(part, report))
        .collect();
    if out.is_empty() {
        vec![json!({"type": "text", "text": "."})]
    } else {
        out
    }
}

fn encode_assistant_parts(parts: &[IrPart], report: &mut LossReport) -> Vec<Value> {
    parts
        .iter()
        .filter_map(|part| encode_part(part, report))
        .collect()
}

fn encode_part(part: &IrPart, report: &mut LossReport) -> Option<Value> {
    match part {
        IrPart::Text(text) if text.trim().is_empty() => None,
        IrPart::Text(text) => Some(json!({"type": "text", "text": text})),
        IrPart::ImageUrl(url) => Some(json!({
            "type": "image",
            "source": {"type": "url", "url": url}
        })),
        IrPart::ImageBase64 { media_type, data } => Some(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        })),
        IrPart::Document {
            source,
            media_type,
            name,
        } => Some(encode_document(source, media_type, name.as_deref())),
        IrPart::Audio { .. } => {
            report.record("part.audio", LossAction::Drop, "audio has no Messages slot");
            None
        }
        IrPart::Thinking { text, signature } => {
            let Some(sig) = signature.as_deref().filter(|s| !s.is_empty()) else {
                report.record(
                    "part.thinking",
                    LossAction::Drop,
                    "unsigned thinking is not replayed",
                );
                return None;
            };
            Some(json!({
                "type": "thinking",
                "thinking": text,
                "signature": sig,
            }))
        }
        IrPart::Raw { raw, .. } => {
            if messages_raw_passthrough(raw) {
                Some(raw.clone())
            } else {
                report.record(
                    off_dialect_raw_path(raw),
                    LossAction::Drop,
                    "raw part is not Messages-shaped",
                );
                None
            }
        }
    }
}

fn encode_document(source: &IrDocumentSource, media_type: &str, name: Option<&str>) -> Value {
    let mut block = json!({"type": "document"});
    if let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) {
        block["title"] = json!(name);
    }
    block["source"] = match source {
        IrDocumentSource::Base64(data) => json!({
            "type": "base64",
            "media_type": if media_type.is_empty() {
                "application/pdf"
            } else {
                media_type
            },
            "data": data
        }),
        IrDocumentSource::Url(url) => json!({
            "type": "url",
            "url": url
        }),
        IrDocumentSource::FileId(id) => json!({
            "type": "file",
            "file_id": id
        }),
    };
    block
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

/// Anthropic rejects more than this many `cache_control` blocks.
const ANTHROPIC_MAX_CACHE_CONTROL_BLOCKS: usize = 4;
/// Preferred agent layout. Leaves headroom if a proxy injects more.
const ANTHROPIC_PREFERRED_CACHE_CONTROL_BLOCKS: usize = 2;

fn apply_cache_breakpoints(body: &mut Value, ir: &IrRequest, report: &mut LossReport) {
    let cache = &ir.sampling.cache;
    if !cache.enabled || cache.retention.as_deref() == Some("none") {
        return;
    }
    if let Some(floor) = cache.min_cacheable_tokens
        && floor > 0
    {
        let estimated = crate::ir::estimate_prompt_tokens(ir);
        if estimated < floor {
            report.record(
                "sampling.cache",
                LossAction::Drop,
                format!("below min_cacheable_tokens floor {floor}"),
            );
            return;
        }
    }
    report.record("sampling.cache", LossAction::Preserve, "messages cache");
    let ttl = match cache.retention.as_deref() {
        Some("1h") | Some("long") => Some("1h"),
        Some("5m") | Some("short") | None => None,
        Some(other) => Some(other),
    };
    strip_all_cache_control(body);
    apply_preferred_cache_breakpoints(body, ttl);
    enforce_cache_control_limit(body, ttl);
}

fn apply_preferred_cache_breakpoints(body: &mut Value, ttl: Option<&str>) {
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|t| !t.is_empty());
    if ttl.is_some() {
        // Long TTL must sit early (tools, then first system). A 1h marker
        // after a later resume fragment is rejected by the API.
        if has_tools {
            tag_last_tool(body, ttl);
            if !tag_first_system(body, ttl) {
                tag_first_user_text(body, ttl);
            }
            return;
        }
        if !tag_first_system(body, ttl) {
            tag_first_user_text(body, ttl);
            return;
        }
        tag_first_user_text(body, ttl);
        return;
    }
    if !tag_last_system(body, ttl) {
        tag_first_system(body, ttl);
    }
    tag_first_user_text(body, ttl);
}

fn strip_all_cache_control(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("cache_control");
            for child in map.values_mut() {
                strip_all_cache_control(child);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                strip_all_cache_control(child);
            }
        }
        _ => {}
    }
}

fn enforce_cache_control_limit(body: &mut Value, ttl: Option<&str>) {
    let n = count_cache_control(body);
    if n <= ANTHROPIC_PREFERRED_CACHE_CONTROL_BLOCKS {
        debug_assert!(n <= ANTHROPIC_MAX_CACHE_CONTROL_BLOCKS);
        return;
    }
    strip_all_cache_control(body);
    apply_preferred_cache_breakpoints(body, ttl);
    debug_assert!(count_cache_control(body) <= ANTHROPIC_MAX_CACHE_CONTROL_BLOCKS);
}

fn count_cache_control(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            usize::from(map.contains_key("cache_control"))
                + map.values().map(count_cache_control).sum::<usize>()
        }
        Value::Array(arr) => arr.iter().map(count_cache_control).sum(),
        _ => 0,
    }
}

fn ephemeral_cache_control(ttl: Option<&str>) -> Value {
    let mut cc = json!({"type": "ephemeral"});
    if let Some(ttl) = ttl {
        cc["ttl"] = json!(ttl);
    }
    cc
}

fn tag_last_tool(body: &mut Value, ttl: Option<&str>) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(last) = tools.last_mut() {
        last["cache_control"] = ephemeral_cache_control(ttl);
    }
}

fn tag_first_system(body: &mut Value, ttl: Option<&str>) -> bool {
    tag_system_slot(body, ttl, true)
}

fn tag_last_system(body: &mut Value, ttl: Option<&str>) -> bool {
    tag_system_slot(body, ttl, false)
}

fn tag_system_slot(body: &mut Value, ttl: Option<&str>, first: bool) -> bool {
    match body.get_mut("system") {
        Some(Value::Array(blocks)) => {
            let slot = if first {
                blocks.first_mut()
            } else {
                blocks.last_mut()
            };
            if let Some(block) = slot {
                block["cache_control"] = ephemeral_cache_control(ttl);
                return true;
            }
            false
        }
        Some(Value::String(text)) => {
            let text = text.clone();
            body["system"] = json!([{
                "type": "text",
                "text": text,
                "cache_control": ephemeral_cache_control(ttl),
            }]);
            true
        }
        Some(Value::Object(obj)) => {
            obj.insert("cache_control".into(), ephemeral_cache_control(ttl));
            true
        }
        _ => false,
    }
}

fn tag_first_user_text(body: &mut Value, ttl: Option<&str>) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(user) = messages
        .iter_mut()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
    else {
        return;
    };
    match user.get_mut("content") {
        Some(Value::Array(parts)) => {
            if let Some(part) = parts.iter_mut().find(|p| {
                p.get("type").and_then(Value::as_str) == Some("text")
                    && p.get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|t| !t.trim().is_empty())
            }) {
                part["cache_control"] = ephemeral_cache_control(ttl);
            }
        }
        Some(Value::String(text)) if !text.trim().is_empty() => {
            let text = text.clone();
            user["content"] = json!([{
                "type": "text",
                "text": text,
                "cache_control": ephemeral_cache_control(ttl),
            }]);
        }
        _ => {}
    }
}

/// Anthropic `tool_use.id` must match `^[a-zA-Z0-9_-]+$`.
fn sanitize_messages_tool_use_id(call_id: &str) -> String {
    let mut out = String::with_capacity(call_id.len().max(1));
    let mut last_underscore = false;
    for c in call_id.chars() {
        let legal = c.is_ascii_alphanumeric() || c == '_' || c == '-';
        if legal {
            if c == '_' && last_underscore {
                continue;
            }
            last_underscore = c == '_';
            out.push(c);
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn rewrite_messages_tool_use_id(
    call_id: &str,
    path: impl Into<String>,
    report: &mut LossReport,
) -> String {
    let sanitized = sanitize_messages_tool_use_id(call_id);
    if sanitized != call_id {
        report.record(
            path,
            LossAction::Degrade,
            "sanitized to Anthropic tool_use.id charset",
        );
    }
    sanitized
}

fn tool_use_block(
    call_id: &str,
    name: &str,
    arguments: &str,
    path: impl Into<String>,
    report: &mut LossReport,
) -> Value {
    let id = rewrite_messages_tool_use_id(call_id, path, report);
    let input = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!(arguments));
    json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": input,
    })
}

fn tool_result_block(
    call_id: &str,
    output: &str,
    path: impl Into<String>,
    report: &mut LossReport,
) -> Value {
    let id = rewrite_messages_tool_use_id(call_id, path, report);
    json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": output,
    })
}

fn encode_tool(tool: &PreparedTool) -> Value {
    match tool {
        PreparedTool::Function {
            name,
            description,
            parameters,
        } => {
            let mut schema = parameters.clone();
            normalize_object_schema_required(&mut schema);
            json!({
                "name": name,
                "description": description,
                "input_schema": schema,
            })
        }
        PreparedTool::Raw(raw) => {
            let mut raw = raw.clone();
            if let Some(schema) = raw.get_mut("input_schema") {
                normalize_object_schema_required(schema);
            }
            raw
        }
    }
}

/// Grok Build Messages rejects `required: null` (and treats a missing
/// `required` the same way): HTTP 400 `/required: null is not of type "array"`.
fn normalize_object_schema_required(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    let is_object =
        obj.get("type").and_then(Value::as_str) == Some("object") || obj.contains_key("properties");
    if !is_object {
        if matches!(obj.get("required"), Some(Value::Null)) {
            obj.insert("required".into(), json!([]));
        }
        return;
    }
    match obj.get("required") {
        None | Some(Value::Null) => {
            obj.insert("required".into(), json!([]));
        }
        Some(Value::Array(_)) => {}
        Some(_) => {
            obj.insert("required".into(), json!([]));
        }
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
    if s.prompt_cache_key.is_some() {
        report.record("sampling.prompt_cache_key", LossAction::Drop, "no slot");
    }
    if s.service_tier.is_some() {
        report.record("sampling.service_tier", LossAction::Drop, "no slot");
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
    encode_thinking(s, body, report);
    if body.get("max_tokens").is_none() {
        body["max_tokens"] = json!(MESSAGES_DEFAULT_COMPLETION_TOKENS);
        report.record(
            "sampling.max_tokens",
            LossAction::Preserve,
            "messages requires max_tokens",
        );
    }
    if !s.include.is_empty() {
        report.record("sampling.include", LossAction::Drop, "no slot");
    }
    if s.json_schema.is_some() {
        report.record("sampling.json_schema", LossAction::Drop, "no slot");
    }
    if s.json_object == Some(true) {
        report.record("sampling.json_object", LossAction::Drop, "no slot");
    }
    if s.user.is_some() {
        report.record("sampling.user", LossAction::Drop, "no slot");
    }
}

/// Extra completion tokens when `max_tokens` must exceed `budget_tokens`.
const MESSAGES_DEFAULT_COMPLETION_TOKENS: u32 = 4096;
const MESSAGES_DEFAULT_THINKING_BUDGET: u32 = 10240;

fn encode_thinking(s: &IrSampling, body: &mut Value, report: &mut LossReport) {
    let effort = s
        .reasoning_effort
        .as_deref()
        .filter(|effort| !effort.trim().is_empty());
    if s.include_thoughts == Some(false) {
        body["thinking"] = json!({ "type": "disabled" });
        report.record(
            "sampling.include_thoughts",
            LossAction::Preserve,
            "messages thinking",
        );
        if effort.is_some() {
            report.record(
                "sampling.reasoning_effort",
                LossAction::Drop,
                "thinking disabled",
            );
        }
        if s.max_reasoning_tokens.is_some() {
            report.record(
                "sampling.max_reasoning_tokens",
                LossAction::Drop,
                "thinking disabled",
            );
        }
        if s.thinking_budget.is_some() {
            report.record(
                "sampling.thinking_budget",
                LossAction::Drop,
                "thinking disabled",
            );
        }
        return;
    }

    let want_enable = s.include_thoughts == Some(true)
        || effort.is_some()
        || s.max_reasoning_tokens.is_some()
        || s.thinking_budget.is_some();
    if !want_enable {
        return;
    }

    let budget = s
        .max_reasoning_tokens
        .or(s.thinking_budget)
        .unwrap_or_else(|| {
            effort
                .map(messages_effort_budget)
                .unwrap_or(MESSAGES_DEFAULT_THINKING_BUDGET)
        });
    body["thinking"] = json!({
        "type": "enabled",
        "budget_tokens": budget,
    });
    if s.include_thoughts == Some(true) {
        report.record(
            "sampling.include_thoughts",
            LossAction::Preserve,
            "messages thinking",
        );
    }
    if s.max_reasoning_tokens.is_some() {
        report.record(
            "sampling.max_reasoning_tokens",
            LossAction::Preserve,
            "messages thinking.budget_tokens",
        );
        if s.thinking_budget.is_some_and(|n| n != budget) {
            report.record(
                "sampling.thinking_budget",
                LossAction::Degrade,
                "max_reasoning_tokens wins budget_tokens",
            );
        }
    } else if s.thinking_budget.is_some() {
        report.record(
            "sampling.thinking_budget",
            LossAction::Preserve,
            "messages thinking.budget_tokens",
        );
    }
    if effort.is_some() {
        report.record(
            "sampling.reasoning_effort",
            LossAction::Preserve,
            "messages thinking",
        );
    }

    let current = body.get("max_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
    if current <= budget {
        let raised = budget.saturating_add(MESSAGES_DEFAULT_COMPLETION_TOKENS);
        body["max_tokens"] = json!(raised);
        report.record(
            "sampling.max_tokens",
            LossAction::Preserve,
            "raised above thinking.budget_tokens",
        );
    }
}

fn messages_effort_budget(effort: &str) -> u32 {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => 4096,
        "high" => 32768,
        "xhigh" | "x-high" => 65536,
        _ => MESSAGES_DEFAULT_THINKING_BUDGET,
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

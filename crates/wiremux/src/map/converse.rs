//! Amazon Bedrock Converse request maps.

use serde_json::{Value, json};

use super::MapError;
use super::drop_dest_chat_sampling_extras;
use super::drop_dest_n_and_penalties;
use super::drop_dest_top_logprobs;
use super::str_field;
use super::string_object_field;
use super::tools::PreparedTool;
use crate::ir::{
    IrDocumentSource, IrItem, IrPart, IrRequest, IrSampling, IrToolChoice, LossAction, LossReport,
};

pub(super) fn decode(value: &Value) -> Result<(IrRequest, LossReport), MapError> {
    let mut report = LossReport::default();
    let mut items = Vec::new();
    if let Some(system) = value.get("system") {
        decode_system(system, &mut items);
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for msg in messages {
            decode_message(msg, &mut items, &mut report)?;
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

fn decode_message(
    msg: &Value,
    items: &mut Vec<IrItem>,
    report: &mut LossReport,
) -> Result<(), MapError> {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
    let content = msg.get("content");
    match role {
        "user" => decode_user(content, items, report),
        "assistant" => decode_assistant(content, items, report),
        _ => Ok(()),
    }
}

fn decode_user(
    content: Option<&Value>,
    items: &mut Vec<IrItem>,
    report: &mut LossReport,
) -> Result<(), MapError> {
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
            flush_user(&mut parts, items);
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
        if let Some(part) = decode_part(block, report) {
            parts.push(part);
        }
    }
    flush_user(&mut parts, items);
    Ok(())
}

fn flush_user(parts: &mut Vec<IrPart>, items: &mut Vec<IrItem>) {
    if !parts.is_empty() {
        items.push(IrItem::User {
            parts: std::mem::take(parts),
        });
    }
}

fn decode_assistant(
    content: Option<&Value>,
    items: &mut Vec<IrItem>,
    report: &mut LossReport,
) -> Result<(), MapError> {
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
        if let Some(part) = decode_part(block, report) {
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

fn decode_part(block: &Value, report: &mut LossReport) -> Option<IrPart> {
    if let Some(text) = block.get("text").and_then(Value::as_str) {
        return Some(IrPart::Text(text.to_string()));
    }
    if let Some(reason) = block
        .pointer("/reasoningContent/reasoningText/text")
        .and_then(Value::as_str)
    {
        let signature = block
            .pointer("/reasoningContent/reasoningText/signature")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        return Some(IrPart::Thinking {
            text: reason.to_string(),
            signature,
        });
    }
    if let Some(doc) = block.get("document") {
        return decode_document(doc);
    }
    if let Some(image) = block.get("image") {
        return decode_image(image);
    }
    if let Some(audio) = block.get("audio") {
        return decode_audio(audio, report);
    }
    None
}

fn decode_document(doc: &Value) -> Option<IrPart> {
    let format = str_field(doc, "format").unwrap_or_else(|| "pdf".into());
    let name = str_field(doc, "name").filter(|s| !s.is_empty());
    let source = doc.get("source")?;
    let src = if let Some(bytes) = str_field(source, "bytes") {
        IrDocumentSource::Base64(bytes)
    } else if let Some(uri) = source
        .pointer("/s3Location/uri")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        IrDocumentSource::Url(uri.to_string())
    } else {
        return None;
    };
    Some(IrPart::Document {
        source: src,
        media_type: super::media_type_from_converse_format(&format),
        name,
    })
}

fn decode_image(image: &Value) -> Option<IrPart> {
    let format = str_field(image, "format").unwrap_or_else(|| "png".into());
    let media_type = super::media_type_from_converse_image_format(&format);
    let source = image.get("source")?;
    if let Some(bytes) = str_field(source, "bytes") {
        return Some(IrPart::ImageBase64 {
            media_type,
            data: bytes,
        });
    }
    if let Some(uri) = source
        .pointer("/s3Location/uri")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(IrPart::ImageUrl(uri.to_string()));
    }
    None
}

fn decode_audio(audio: &Value, report: &mut LossReport) -> Option<IrPart> {
    let format = str_field(audio, "format").unwrap_or_else(|| "mp3".into());
    let source = audio.get("source")?;
    if let Some(bytes) = str_field(source, "bytes") {
        return Some(IrPart::Audio {
            data: bytes,
            format,
        });
    }
    if source
        .pointer("/s3Location/uri")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        report.record("part.audio", LossAction::Drop, "audio s3 has no chat slot");
    }
    None
}

fn decode_sampling(value: &Value) -> IrSampling {
    let mut sampling = IrSampling::default();
    if let Some(cfg) = value.get("inferenceConfig") {
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
    }
    sampling.service_tier = value
        .pointer("/serviceTier/type")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    if let Some(out) = value.get("outputConfig") {
        sampling.reasoning_effort = out
            .get("effort")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        let text_format = out.get("textFormat");
        let is_json_schema = text_format
            .and_then(|tf| tf.get("type"))
            .and_then(Value::as_str)
            == Some("json_schema");
        if is_json_schema
            && let Some(js) = text_format.and_then(|tf| tf.pointer("/structure/jsonSchema"))
        {
            sampling.json_schema = converse_json_schema(js.get("schema"));
            sampling.json_schema_name = js
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
        }
    }
    sampling.metadata = string_object_field(value, "requestMetadata");
    sampling
}

fn converse_json_schema(schema: Option<&Value>) -> Option<Value> {
    match schema {
        Some(Value::String(raw)) => serde_json::from_str(raw).ok().filter(Value::is_object),
        Some(obj) if obj.is_object() => Some(obj.clone()),
        _ => None,
    }
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
    encode_sampling(ir, &mut body, report);
    if !prepared.is_empty() {
        if matches!(ir.sampling.tool_choice, IrToolChoice::None) {
            report.record("sampling.tool_choice", LossAction::Degrade, "no none slot");
        } else {
            let tools: Vec<Value> = prepared.iter().filter_map(encode_tool).collect();
            if !tools.is_empty() {
                let mut cfg = json!({ "tools": tools });
                encode_tool_choice(&ir.sampling.tool_choice, &mut cfg);
                body["toolConfig"] = cfg;
            }
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
                if blocks.is_empty() {
                    continue;
                }
                if last_user_has_tool_result(&messages)
                    && let Some(last) = messages.last_mut()
                    && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
                {
                    arr.extend(blocks);
                    continue;
                }
                messages.push(json!({ "role": "user", "content": blocks }));
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
                let text = if output.trim().is_empty() {
                    "."
                } else {
                    output.as_str()
                };
                let block = json!({
                    "toolResult": {
                        "toolUseId": call_id,
                        "content": [{ "text": text }]
                    }
                });
                if last_is_user(&messages)
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

fn last_is_user(messages: &[Value]) -> bool {
    messages
        .last()
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        == Some("user")
}

fn last_user_has_tool_result(messages: &[Value]) -> bool {
    if !last_is_user(messages) {
        return false;
    }
    messages
        .last()
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(|b| b.get("toolResult").is_some()))
}

fn encode_part(part: &IrPart, report: &mut LossReport) -> Option<Value> {
    match part {
        IrPart::Text(text) if text.trim().is_empty() => None,
        IrPart::Text(text) => Some(json!({ "text": text })),
        IrPart::Thinking { text, signature } => {
            let mut reasoning_text = json!({ "text": text });
            if let Some(sig) = signature.as_deref().filter(|s| !s.is_empty()) {
                reasoning_text["signature"] = json!(sig);
            }
            Some(json!({
                "reasoningContent": { "reasoningText": reasoning_text }
            }))
        }
        IrPart::Document {
            source,
            media_type,
            name,
        } => encode_document(source, media_type, name.as_deref(), report),
        IrPart::Audio { data, format } => encode_audio(data, format, report),
        IrPart::ImageUrl(url) => encode_image_url(url, report),
        IrPart::ImageBase64 { media_type, data } => encode_image_bytes(media_type, data, report),
        IrPart::Raw { .. } => {
            report.record("content", LossAction::Drop, "converse image/raw dropped");
            None
        }
    }
}

fn encode_image_bytes(media_type: &str, data: &str, report: &mut LossReport) -> Option<Value> {
    let Some(format) = super::converse_image_format(media_type) else {
        report.record(
            "part.image",
            LossAction::Drop,
            "image format has no converse slot",
        );
        return None;
    };
    Some(json!({
        "image": {
            "format": format,
            "source": { "bytes": data }
        }
    }))
}

fn encode_image_url(url: &str, report: &mut LossReport) -> Option<Value> {
    if !url.to_ascii_lowercase().starts_with("s3://") {
        report.record(
            "part.image",
            LossAction::Drop,
            "image url has no converse slot",
        );
        return None;
    }
    let format = super::converse_image_format_from_url(url);
    Some(json!({
        "image": {
            "format": format,
            "source": { "s3Location": { "uri": url } }
        }
    }))
}

fn encode_audio(data: &str, format: &str, report: &mut LossReport) -> Option<Value> {
    let Some(format) = super::converse_audio_format(format) else {
        report.record(
            "part.audio",
            LossAction::Drop,
            "audio format has no converse slot",
        );
        return None;
    };
    Some(json!({
        "audio": {
            "format": format,
            "source": { "bytes": data }
        }
    }))
}

fn encode_document(
    source: &IrDocumentSource,
    media_type: &str,
    name: Option<&str>,
    report: &mut LossReport,
) -> Option<Value> {
    if matches!(source, IrDocumentSource::FileId(_)) {
        report.record(
            "part.document",
            LossAction::Drop,
            "document file_id has no converse slot",
        );
        return None;
    }
    let Some(format) = super::converse_document_format(media_type) else {
        report.record(
            "part.document",
            LossAction::Drop,
            "document format has no converse slot",
        );
        return None;
    };
    let name = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("document");
    match source {
        IrDocumentSource::Base64(data) => Some(json!({
            "document": {
                "format": format,
                "name": name,
                "source": { "bytes": data }
            }
        })),
        IrDocumentSource::Url(url) if url.starts_with("s3://") => Some(json!({
            "document": {
                "format": format,
                "name": name,
                "source": { "s3Location": { "uri": url } }
            }
        })),
        IrDocumentSource::Url(_) => {
            report.record(
                "part.document",
                LossAction::Drop,
                "document url has no converse slot",
            );
            None
        }
        IrDocumentSource::FileId(_) => unreachable!("recorded above"),
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

fn encode_sampling(ir: &IrRequest, body: &mut Value, report: &mut LossReport) {
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
    if s.store.is_some() {
        report.record("sampling.store", LossAction::Drop, "no slot");
    }
    if s.prompt_cache_key.is_some() {
        report.record("sampling.prompt_cache_key", LossAction::Drop, "no slot");
    }
    if s.prompt_cache_retention.is_some() {
        report.record(
            "sampling.prompt_cache_retention",
            LossAction::Drop,
            "no slot",
        );
    }
    drop_dest_chat_sampling_extras(s, report);
    drop_dest_top_logprobs(s, report);
    drop_dest_n_and_penalties(s, report);
    if let Some(tier) = s.service_tier.as_deref() {
        match converse_service_tier(tier) {
            Some((mapped, degrade)) => {
                body["serviceTier"] = json!({ "type": mapped });
                if let Some(detail) = degrade {
                    report.record("sampling.service_tier", LossAction::Degrade, detail);
                }
            }
            None => {
                report.record(
                    "sampling.service_tier",
                    LossAction::Drop,
                    "unmapped service_tier",
                );
            }
        }
    }
    if s.previous_response_id.is_some() {
        report.record("sampling.previous_response_id", LossAction::Drop, "no slot");
    }
    if s.cache.enabled {
        report.record("sampling.cache", LossAction::Drop, "no slot");
    }
    let mut output = serde_json::Map::new();
    match &s.json_schema {
        Some(schema) if schema.is_object() => match serde_json::to_string(schema) {
            Ok(schema_str) => {
                let mut json_schema = serde_json::Map::new();
                json_schema.insert("schema".into(), json!(schema_str));
                if let Some(name) = &s.json_schema_name {
                    json_schema.insert("name".into(), json!(name));
                }
                output.insert(
                    "textFormat".into(),
                    json!({
                        "type": "json_schema",
                        "structure": { "jsonSchema": json_schema },
                    }),
                );
            }
            Err(_) => {
                report.record(
                    "sampling.json_schema",
                    LossAction::Drop,
                    "json_schema requires object schema",
                );
                if s.json_schema_name.is_some() {
                    report.record("sampling.json_schema_name", LossAction::Drop, "no slot");
                }
            }
        },
        Some(_) => {
            report.record(
                "sampling.json_schema",
                LossAction::Drop,
                "json_schema requires object schema",
            );
            if s.json_schema_name.is_some() {
                report.record("sampling.json_schema_name", LossAction::Drop, "no slot");
            }
        }
        None => {
            if s.json_schema_name.is_some() {
                report.record("sampling.json_schema_name", LossAction::Drop, "no slot");
            }
        }
    }
    if let Some(effort) = s.reasoning_effort.as_deref() {
        match converse_effort(effort) {
            Some((mapped, degrade)) => {
                output.insert("effort".into(), json!(mapped));
                if let Some(detail) = degrade {
                    report.record("sampling.reasoning_effort", LossAction::Degrade, detail);
                }
            }
            None => {
                report.record(
                    "sampling.reasoning_effort",
                    LossAction::Drop,
                    "unmapped effort",
                );
            }
        }
    }
    if !output.is_empty() {
        body["outputConfig"] = Value::Object(output);
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
    if s.json_object == Some(true) && s.json_schema.is_none() {
        report.record("sampling.json_object", LossAction::Drop, "no slot");
    }
    if s.user.is_some() {
        report.record("sampling.user", LossAction::Drop, "no slot");
    }
    if s.verbosity.is_some() {
        report.record("sampling.verbosity", LossAction::Drop, "no slot");
    }
    if s.safety_identifier.is_some() {
        report.record("sampling.safety_identifier", LossAction::Drop, "no slot");
    }
    if !s.metadata.is_empty() {
        body["requestMetadata"] = json!(s.metadata);
        report.record(
            "sampling.metadata",
            LossAction::Preserve,
            "converse requestMetadata",
        );
    }
    if !s.include.is_empty() {
        report.record("sampling.include", LossAction::Drop, "no slot");
    }
    if s.parallel_tool_calls.is_some() {
        report.record("sampling.parallel_tool_calls", LossAction::Drop, "no slot");
    }
}

fn converse_service_tier(tier: &str) -> Option<(String, Option<&'static str>)> {
    let trimmed = tier.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "flex" | "priority" | "reserved" | "default" => Some((lower, None)),
        "auto" => Some(("default".into(), Some("auto maps to default"))),
        _ => None,
    }
}

fn converse_effort(effort: &str) -> Option<(String, Option<&'static str>)> {
    let trimmed = effort.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "low" | "medium" | "high" | "xhigh" | "max" => Some((lower, None)),
        "x-high" => Some(("xhigh".into(), Some("x-high maps to xhigh"))),
        _ => None,
    }
}

fn encode_tool_choice(choice: &IrToolChoice, cfg: &mut Value) {
    match choice {
        IrToolChoice::Auto => {
            cfg["toolChoice"] = json!({ "auto": {} });
        }
        IrToolChoice::None => {}
        IrToolChoice::Required => {
            cfg["toolChoice"] = json!({ "any": {} });
        }
        IrToolChoice::Named(name) => {
            cfg["toolChoice"] = json!({ "tool": { "name": name } });
        }
    }
}

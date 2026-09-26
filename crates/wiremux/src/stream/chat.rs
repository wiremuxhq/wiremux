//! Chat Completions SSE (`data:` chunks, no `event:` field).

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

use super::usage;
use super::{MAX_TOOL_CALL_INDEX, RawSse, check_index, str_field};

pub(super) fn decode(value: &Value) -> Result<Option<IrStreamEvent>, MapError> {
    let choices = value.get("choices").and_then(Value::as_array);
    let empty_choices = choices.map(|c| c.is_empty()).unwrap_or(true);
    if empty_choices && let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
        return Ok(Some(usage::from_chat(usage)));
    }

    let Some(choice) = choices.and_then(|c| c.first()) else {
        if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
            return Ok(Some(usage::from_chat(usage)));
        }
        if let Some(ev) = super::complete::moderation_event(value) {
            return Ok(Some(ev));
        }
        return Ok(None);
    };

    if let Some(calls) = choice
        .pointer("/delta/tool_calls")
        .and_then(Value::as_array)
    {
        for call in calls {
            check_index(call, "index", MAX_TOOL_CALL_INDEX, "tool call")?;
            super::json_text_field(function_body(call), "arguments")?;
        }
        // One event cannot carry two calls, or a start plus its arguments.
        // `decode_stream_events` keeps both. An arguments-only delta stays.
        if calls.len() > 1 || calls.first().is_some_and(call_bundles_arguments) {
            return Err(singular_tool_limit());
        }
        if let Some(call) = calls.first() {
            return Ok(Some(decode_tool_call(call, value)));
        }
    }
    if let Some(fc) = choice
        .pointer("/delta/function_call")
        .filter(|v| v.is_object())
    {
        super::json_text_field(function_body(fc), "arguments")?;
        if call_bundles_arguments(fc) {
            return Err(singular_tool_limit());
        }
        return Ok(Some(decode_tool_call(fc, value)));
    }

    let delta = choice.get("delta");
    if let Some(text) = delta
        .and_then(|d| d.get("content"))
        .and_then(flatten_content)
    {
        return Ok(Some(IrStreamEvent::TextDelta { text }));
    }
    if let Some(text) = delta
        .and_then(|d| d.get("refusal"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(IrStreamEvent::RefusalDelta {
            text: text.to_string(),
        }));
    }
    if let Some(content) = logprobs_content(choice) {
        return Ok(Some(IrStreamEvent::Logprobs { content }));
    }
    if let Some(text) = delta
        .and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(IrStreamEvent::ReasoningDelta {
            text: text.to_string(),
        }));
    }
    if let Some(signature) = delta
        .and_then(|d| d.get("reasoning_signature"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(IrStreamEvent::ReasoningSignature {
            signature: signature.to_string(),
        }));
    }
    let mut media = Vec::new();
    if let Some(content) = delta.and_then(|d| d.get("content")) {
        media.extend(content_events(content));
    }
    if let Some(audio) = delta.and_then(|d| d.get("audio")).filter(|v| v.is_object()) {
        if let Some(data) = str_field(audio, "data").filter(|s| !s.is_empty()) {
            media.push(IrStreamEvent::AudioDelta { data });
        }
        if let Some(text) = str_field(audio, "transcript").filter(|s| !s.is_empty()) {
            media.push(IrStreamEvent::AudioTranscriptDelta { text });
        }
    }
    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|reason| IrStreamEvent::FinishReason {
            reason: map_finish(reason).to_string(),
        });
    let usage = value
        .get("usage")
        .filter(|v| v.is_object())
        .map(usage::from_chat);
    let signals = media.len() + usize::from(finish.is_some()) + usize::from(usage.is_some());
    if signals > 1 {
        return Err(singular_media_limit());
    }
    if let Some(ev) = media.into_iter().next() {
        return Ok(Some(ev));
    }
    if let Some(ev) = finish {
        return Ok(Some(ev));
    }
    if let Some(ev) = usage {
        return Ok(Some(ev));
    }
    Ok(None)
}

/// Every signal on one Chat chunk. First-wins lives in [`decode`].
pub(super) fn decode_all(value: &Value) -> Result<Vec<IrStreamEvent>, MapError> {
    let choices = value.get("choices").and_then(Value::as_array);
    let empty_choices = choices.map(|c| c.is_empty()).unwrap_or(true);
    if empty_choices && let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
        return Ok(vec![usage::from_chat(usage)]);
    }

    let Some(choice) = choices.and_then(|c| c.first()) else {
        let mut out = Vec::new();
        if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
            out.push(usage::from_chat(usage));
        }
        if let Some(ev) = super::complete::moderation_event(value) {
            out.push(ev);
        }
        return Ok(out);
    };

    let mut out = Vec::new();
    if let Some(tier) = value
        .get("service_tier")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::ServiceTier {
            tier: tier.to_string(),
        });
    }
    if let Some(unix) = value.get("created").and_then(Value::as_i64) {
        out.push(IrStreamEvent::Created { unix });
    }
    if let Some(obj) = value.get("metadata").and_then(Value::as_object) {
        let metadata: std::collections::BTreeMap<String, String> = obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
        if !metadata.is_empty() {
            out.push(IrStreamEvent::Metadata { metadata });
        }
    }
    if let Some(ev) = super::complete::moderation_event(value) {
        out.push(ev);
    }
    if let Some(calls) = choice
        .pointer("/delta/tool_calls")
        .and_then(Value::as_array)
    {
        for call in calls {
            check_index(call, "index", MAX_TOOL_CALL_INDEX, "tool call")?;
            out.extend(expand_tool_call(call, value)?);
        }
    } else if let Some(fc) = choice
        .pointer("/delta/function_call")
        .filter(|v| v.is_object())
    {
        out.extend(expand_tool_call(fc, value)?);
    }

    let delta = choice.get("delta");
    if let Some(text) = delta
        .and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::ReasoningDelta {
            text: text.to_string(),
        });
    }
    if let Some(signature) = delta
        .and_then(|d| d.get("reasoning_signature"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::ReasoningSignature {
            signature: signature.to_string(),
        });
    }
    if let Some(content) = delta.and_then(|d| d.get("content")) {
        out.extend(content_events(content));
    }
    if let Some(anns) = delta
        .and_then(|d| d.get("annotations"))
        .and_then(Value::as_array)
    {
        for ann in anns {
            if ann.is_object() {
                out.push(IrStreamEvent::AnnotationAdded {
                    annotation: annotation_from_chat(ann),
                });
            }
        }
    }
    if let Some(text) = delta
        .and_then(|d| d.get("refusal"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::RefusalDelta {
            text: text.to_string(),
        });
    }
    if let Some(audio) = delta.and_then(|d| d.get("audio")).filter(|v| v.is_object()) {
        if let Some(data) = str_field(audio, "data").filter(|s| !s.is_empty()) {
            out.push(IrStreamEvent::AudioDelta { data });
        }
        if let Some(text) = str_field(audio, "transcript").filter(|s| !s.is_empty()) {
            out.push(IrStreamEvent::AudioTranscriptDelta { text });
        }
    }
    if let Some(content) = logprobs_content(choice) {
        out.push(IrStreamEvent::Logprobs { content });
    }
    if let Some(reason) = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::FinishReason {
            reason: map_finish(reason).to_string(),
        });
    }
    if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
        out.push(usage::from_chat(usage));
    }
    Ok(out)
}

pub(super) fn logprobs_content(choice: &Value) -> Option<Value> {
    let logprobs = choice.get("logprobs")?;
    let mut out = Vec::new();
    for key in ["content", "refusal"] {
        if let Some(arr) = logprobs.get(key).and_then(Value::as_array) {
            out.extend(arr.iter().cloned());
        }
    }
    (!out.is_empty()).then_some(Value::Array(out))
}

pub(super) fn chat_image_url_part(media_type: &str, data: &str) -> Value {
    json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{media_type};base64,{data}") }
    })
}

pub(super) fn content_events(content: &Value) -> Vec<IrStreamEvent> {
    if let Some(text) = content.as_str().filter(|s| !s.is_empty()) {
        return vec![IrStreamEvent::TextDelta {
            text: text.to_string(),
        }];
    }
    let Some(parts) = content.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut text = String::new();
    for part in parts {
        if let Some(piece) = part.as_str() {
            text.push_str(piece);
            continue;
        }
        if part.get("type").and_then(Value::as_str) == Some("image_url") {
            if !text.is_empty() {
                out.push(IrStreamEvent::TextDelta {
                    text: std::mem::take(&mut text),
                });
            }
            out.push(
                image_event_from_part(part).unwrap_or_else(|| super::protocol("image_url", part)),
            );
            continue;
        }
        let ty = part.get("type").and_then(Value::as_str);
        if (ty.is_none() || ty == Some("text"))
            && let Some(piece) = part.get("text").and_then(Value::as_str)
        {
            text.push_str(piece);
        }
    }
    if !text.is_empty() {
        out.push(IrStreamEvent::TextDelta { text });
    }
    out
}

fn image_event_from_part(part: &Value) -> Option<IrStreamEvent> {
    if part.get("type").and_then(Value::as_str) != Some("image_url") {
        return None;
    }
    let url = part.pointer("/image_url/url").and_then(Value::as_str)?;
    let rest = url.strip_prefix("data:")?;
    let (media_type, data) = rest.split_once(";base64,")?;
    if data.is_empty() || !media_type.to_ascii_lowercase().starts_with("image/") {
        return None;
    }
    Some(IrStreamEvent::ImageDelta {
        media_type: media_type.to_string(),
        data: data.to_string(),
    })
}

pub(super) fn flatten_content(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    let arr = content.as_array()?;
    let mut out = String::new();
    for part in arr {
        if let Some(s) = part.as_str() {
            out.push_str(s);
            continue;
        }
        let ty = part.get("type").and_then(Value::as_str);
        if (ty.is_none() || ty == Some("text"))
            && let Some(s) = part.get("text").and_then(Value::as_str)
        {
            out.push_str(s);
        }
    }
    (!out.is_empty()).then_some(out)
}

pub(crate) fn map_finish(reason: &str) -> &str {
    match reason {
        "eos" => "stop",
        "function_call" => "tool_calls",
        "content_filter" | "content-filter" => "content_filter",
        other => other,
    }
}

pub(super) fn encode_finish(reason: &str) -> &str {
    match reason {
        "max_tokens" | "model_context_window_exceeded" => "length",
        "failed" | "cancelled" | "canceled" | "end_turn" | "stop_sequence" | "pause_turn" => "stop",
        other => other,
    }
}

/// Dest Chat finish once a tool call has been opened.
///
/// Only a real `stop` or Messages `end_turn` becomes `tool_calls`.
/// `failed`, `cancelled`, `canceled`, `pause_turn`, and `stop_sequence`
/// stay on [`encode_finish`], so a cut-off call is not executed.
pub(super) fn finish_after_tool(reason: &str, saw_tool: bool) -> String {
    if saw_tool && matches!(reason, "stop" | "end_turn") {
        "tool_calls".to_string()
    } else {
        encode_finish(reason).to_string()
    }
}

pub(super) fn annotation_url(ann: &Value) -> Option<&str> {
    ann.pointer("/url_citation/url")
        .or_else(|| ann.get("url"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub(super) fn annotation_title(ann: &Value) -> Option<&str> {
    ann.pointer("/url_citation/title")
        .or_else(|| ann.get("title"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub(super) fn annotation_from_chat(ann: &Value) -> Value {
    if ann.get("type").and_then(Value::as_str) != Some("url_citation") {
        return ann.clone();
    }
    let Some(cit) = ann.get("url_citation") else {
        return ann.clone();
    };
    let mut out = json!({ "type": "url_citation" });
    for key in ["start_index", "end_index", "title", "url"] {
        if let Some(value) = cit.get(key) {
            out[key] = value.clone();
        }
    }
    out
}

pub(super) fn annotation_to_chat(ann: &Value) -> Value {
    if ann.get("type").and_then(Value::as_str) != Some("url_citation")
        || ann.get("url_citation").is_some()
    {
        return ann.clone();
    }
    json!({
        "type": "url_citation",
        "url_citation": {
            "start_index": ann.get("start_index").cloned().unwrap_or(json!(0)),
            "end_index": ann.get("end_index").cloned().unwrap_or(json!(0)),
            "title": ann.get("title").cloned().unwrap_or(json!("")),
            "url": ann.get("url").cloned().unwrap_or(json!("")),
        }
    })
}

fn function_body(call: &Value) -> &Value {
    call.get("function").unwrap_or(call)
}

pub(super) fn tool_call_index(call: &Value) -> u32 {
    call.get("index")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

fn protocol_chunk(chunk: &Value) -> Vec<IrStreamEvent> {
    vec![IrStreamEvent::Protocol {
        item_type: "chunk".into(),
        payload: chunk.clone(),
    }]
}

/// `type: custom`, or a continuation that only carries `custom`.
fn is_custom_tool_call(call: &Value) -> bool {
    match call.get("type").and_then(Value::as_str) {
        Some("custom") => true,
        Some(_) => false,
        None => call.get("custom").is_some() && call.get("function").is_none(),
    }
}

fn expand_custom_tool_call(call: &Value, chunk: &Value) -> Result<Vec<IrStreamEvent>, MapError> {
    let custom = call.get("custom").unwrap_or(call);
    let id = str_field(call, "id").unwrap_or_default();
    let name = str_field(custom, "name").unwrap_or_default();
    let input = super::json_text_field(custom, "input")?;
    let index = tool_call_index(call);
    let mut out = Vec::new();
    if !id.is_empty() || !name.is_empty() {
        out.push(IrStreamEvent::CustomToolCallStart { id, name, index });
    }
    if let Some(delta) = input {
        out.push(IrStreamEvent::CustomToolCallInputDelta { delta, index });
    }
    if out.is_empty() {
        Ok(protocol_chunk(chunk))
    } else {
        Ok(out)
    }
}

/// Expand one Chat `tool_calls[]` entry into Start and/or ArgDelta.
///
/// Number, boolean, and null `arguments` / `custom.input` are
/// [`MapError::Invalid`]. An unknown `type` stays `Protocol`.
pub(super) fn expand_tool_call(
    call: &Value,
    chunk: &Value,
) -> Result<Vec<IrStreamEvent>, MapError> {
    if is_custom_tool_call(call) {
        return expand_custom_tool_call(call, chunk);
    }
    if let Some(ty) = call.get("type").and_then(Value::as_str)
        && ty != "function"
    {
        return Ok(protocol_chunk(chunk));
    }
    let func = function_body(call);
    let id = str_field(call, "id").unwrap_or_default();
    let name = str_field(func, "name").unwrap_or_default();
    let args = super::json_text_field(func, "arguments")?;
    let index = tool_call_index(call);
    let mut out = Vec::new();
    if !id.is_empty() || !name.is_empty() {
        out.push(IrStreamEvent::ToolCallStart {
            id,
            name,
            thought_signature: None,
            index,
        });
    }
    if let Some(delta) = args {
        out.push(IrStreamEvent::ToolCallArgDelta { delta, index });
    }
    if out.is_empty() {
        Ok(protocol_chunk(chunk))
    } else {
        Ok(out)
    }
}

/// `id` or `name` together with a non-empty `arguments` string.
///
/// Non-function types stay on the Protocol path. Empty `arguments` is a start.
fn call_bundles_arguments(call: &Value) -> bool {
    if let Some(ty) = call.get("type").and_then(Value::as_str)
        && ty != "function"
    {
        return false;
    }
    let func = function_body(call);
    let started = str_field(call, "id").is_some() || str_field(func, "name").is_some();
    let args = super::json_text_field(func, "arguments")
        .ok()
        .flatten()
        .is_some();
    started && args
}

fn singular_media_limit() -> MapError {
    MapError::Invalid(
        "decode_stream_event cannot represent every event in this Chat chunk; use decode_stream_events"
            .into(),
    )
}

pub(super) fn singular_tool_limit() -> MapError {
    MapError::Invalid(
        "decode_stream_event cannot represent multiple Chat tool calls or a call that includes arguments; use decode_stream_events"
            .into(),
    )
}

fn decode_tool_call(call: &Value, chunk: &Value) -> IrStreamEvent {
    let keep = || IrStreamEvent::Protocol {
        item_type: "chunk".into(),
        payload: chunk.clone(),
    };
    if let Some(ty) = call.get("type").and_then(Value::as_str)
        && ty != "function"
    {
        return keep();
    }
    let func = function_body(call);
    let id = str_field(call, "id");
    let name = str_field(func, "name");
    let args = super::json_text_field(func, "arguments").ok().flatten();
    let index = tool_call_index(call);
    if id.is_some() || name.is_some() {
        return IrStreamEvent::ToolCallStart {
            id: id.unwrap_or_default(),
            name: name.unwrap_or_default(),
            thought_signature: None,
            index,
        };
    }
    match args {
        Some(delta) => IrStreamEvent::ToolCallArgDelta { delta, index },
        None => keep(),
    }
}

pub(super) fn encode(ev: &IrStreamEvent) -> Result<RawSse, MapError> {
    let data = match ev {
        IrStreamEvent::TextDelta { text } => json!({
            "choices": [{ "index": 0, "delta": { "content": text } }]
        }),
        IrStreamEvent::RefusalDelta { text } => json!({
            "choices": [{ "index": 0, "delta": { "refusal": text } }]
        }),
        IrStreamEvent::ReasoningDelta { text } => json!({
            "choices": [{ "index": 0, "delta": { "reasoning_content": text } }]
        }),
        IrStreamEvent::ReasoningSignature { signature } => json!({
            "choices": [{ "index": 0, "delta": { "reasoning_signature": signature } }]
        }),
        IrStreamEvent::ToolCallStart {
            id, name, index, ..
        } => json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": "" }
                    }]
                }
            }]
        }),
        IrStreamEvent::ToolCallArgDelta { delta, index } => json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "function": { "arguments": delta }
                    }]
                }
            }]
        }),
        IrStreamEvent::ToolCallEnd => json!({
            "choices": [{ "index": 0, "delta": {} }]
        }),
        IrStreamEvent::AnnotationAdded { annotation } => json!({
            "choices": [{
                "index": 0,
                "delta": { "annotations": [annotation_to_chat(annotation)] }
            }]
        }),
        IrStreamEvent::AudioDelta { data } => json!({
            "choices": [{ "index": 0, "delta": { "audio": { "data": data } } }]
        }),
        IrStreamEvent::ImageDelta { media_type, data } => json!({
            "choices": [{
                "index": 0,
                "delta": { "content": [chat_image_url_part(media_type, data)] }
            }]
        }),
        IrStreamEvent::AudioTranscriptDelta { text } => json!({
            "choices": [{ "index": 0, "delta": { "audio": { "transcript": text } } }]
        }),
        IrStreamEvent::CustomToolCallStart { id, name, index } => json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "custom",
                        "custom": { "name": name, "input": "" }
                    }]
                }
            }]
        }),
        IrStreamEvent::CustomToolCallInputDelta { delta, index } => json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "type": "custom",
                        "custom": { "input": delta }
                    }]
                }
            }]
        }),
        IrStreamEvent::Logprobs { content } => json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "logprobs": { "content": content }
            }]
        }),
        IrStreamEvent::Created { unix } => json!({
            "created": unix,
            "choices": []
        }),
        IrStreamEvent::ServiceTier { tier } => json!({
            "service_tier": tier,
            "choices": []
        }),
        IrStreamEvent::Metadata { metadata } => json!({
            "metadata": metadata,
            "choices": []
        }),
        IrStreamEvent::Moderation { input, output } => {
            let mut moderation = serde_json::Map::new();
            if let Some(input) = input {
                moderation.insert("input".into(), input.clone());
            }
            if let Some(output) = output {
                moderation.insert("output".into(), output.clone());
            }
            json!({
                "moderation": moderation,
                "choices": []
            })
        }
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            audio_tokens,
            completion_audio_tokens,
        } => usage::encode_chat(
            *prompt_tokens,
            *completion_tokens,
            *cache_read_tokens,
            *cache_write_tokens,
            *reasoning_tokens,
            *audio_tokens,
            *completion_audio_tokens,
        ),
        IrStreamEvent::FinishReason { reason } => json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": encode_finish(reason) }]
        }),
        IrStreamEvent::Done => {
            return Ok(RawSse {
                event: None,
                data: "[DONE]".into(),
            });
        }
        IrStreamEvent::Protocol { .. } | IrStreamEvent::Unknown { .. } => {
            return Err(MapError::Invalid(
                "protocol/unknown events are encoded at the stream root".into(),
            ));
        }
    };
    Ok(RawSse {
        event: None,
        data: data.to_string(),
    })
}

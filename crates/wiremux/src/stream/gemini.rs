//! Gemini generateContent SSE (`data:` JSON chunks).

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

use super::usage;
use super::{RawSse, str_field};

pub(super) fn decode(value: &Value) -> Result<Option<IrStreamEvent>, MapError> {
    if value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .is_none()
        && let Some(reason) = value
            .pointer("/promptFeedback/blockReason")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    {
        return Ok(Some(IrStreamEvent::FinishReason {
            reason: map_block(reason).to_string(),
        }));
    }

    if let Some(usage) = value.get("usageMetadata").filter(|v| v.is_object())
        && value
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
            .is_none_or(|p| p.is_empty())
        && value
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
            .is_none()
    {
        return Ok(Some(usage::from_gemini(usage)));
    }

    let Some(candidate) = value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
    else {
        if let Some(error) = value.get("error").filter(|v| v.is_object()) {
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("vendor error")
                .to_string();
            return Err(MapError::HardError {
                path: "error".into(),
                detail,
            });
        }
        if let Some(usage) = value.get("usageMetadata").filter(|v| v.is_object()) {
            return Ok(Some(usage::from_gemini(usage)));
        }
        return Ok(None);
    };

    if let Some(parts) = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
    {
        for part in parts {
            if part.get("thought").and_then(Value::as_bool) == Some(true)
                && let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            {
                return Ok(Some(IrStreamEvent::ReasoningDelta {
                    text: text.to_string(),
                }));
            }
            if let Some(sig) = part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                && part.get("functionCall").is_none()
            {
                return Ok(Some(IrStreamEvent::ReasoningSignature {
                    signature: sig.to_string(),
                }));
            }
            if let Some(fc) = part.get("functionCall") {
                let name = str_field(fc, "name").unwrap_or_default();
                let thought_signature = part
                    .get("thoughtSignature")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let args = fc.get("args");
                if args.is_some_and(|a| !a.as_object().is_some_and(serde_json::Map::is_empty)) {
                    return Ok(Some(IrStreamEvent::Protocol {
                        item_type: "chunk".into(),
                        payload: value.clone(),
                    }));
                }
                return Ok(Some(IrStreamEvent::ToolCallStart {
                    id: gemini_call_id(fc, &name, 0),
                    name,
                    thought_signature,
                    index: 0,
                }));
            }
            if let Some(text) = part
                .get("text")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Ok(Some(IrStreamEvent::TextDelta {
                    text: text.to_string(),
                }));
            }
        }
    }

    if let Some(chunks) = candidate
        .pointer("/groundingMetadata/groundingChunks")
        .and_then(Value::as_array)
    {
        for chunk in chunks {
            if let Some(annotation) = annotation_from_grounding_chunk(chunk) {
                return Ok(Some(IrStreamEvent::AnnotationAdded { annotation }));
            }
        }
    }

    if let Some(reason) = candidate
        .get("finishReason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(IrStreamEvent::FinishReason {
            reason: map_finish(reason, candidate_has_function_call(candidate)).to_string(),
        }));
    }

    if let Some(usage) = value.get("usageMetadata").filter(|v| v.is_object()) {
        return Ok(Some(usage::from_gemini(usage)));
    }
    Ok(None)
}

fn candidate_has_function_call(candidate: &Value) -> bool {
    candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(|p| p.get("functionCall").is_some()))
}

pub(crate) fn gemini_call_id(fc: &Value, name: &str, seq: usize) -> String {
    fc.get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{name}#{seq}"))
}

pub(super) fn map_finish(reason: &str, has_function_call: bool) -> &'static str {
    match reason {
        "STOP" if has_function_call => "tool_calls",
        "STOP" => "stop",
        "MAX_TOKENS" => "max_tokens",
        "SAFETY" | "RECITATION" | "OTHER" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
        | "IMAGE_SAFETY" | "LANGUAGE" => "content_filter",
        "MALFORMED_FUNCTION_CALL" => "malformed_function_call",
        other if other.eq_ignore_ascii_case("stop") => "stop",
        _ => "stop",
    }
}

fn map_block(reason: &str) -> &'static str {
    match map_finish(reason, false) {
        "stop" if !reason.eq_ignore_ascii_case("stop") => "content_filter",
        mapped => mapped,
    }
}

pub(super) fn encode(ev: &IrStreamEvent) -> Result<RawSse, MapError> {
    let data = match ev {
        IrStreamEvent::TextDelta { text }
        | IrStreamEvent::RefusalDelta { text }
        | IrStreamEvent::AudioTranscriptDelta { text } => json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": text }] }
            }]
        }),
        IrStreamEvent::ReasoningDelta { text } => json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": text, "thought": true }] }
            }]
        }),
        IrStreamEvent::ReasoningSignature { signature } => json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "thoughtSignature": signature }] }
            }]
        }),
        IrStreamEvent::ToolCallStart {
            id,
            name,
            thought_signature,
            ..
        } => {
            let n = if name.is_empty() { id } else { name };
            let mut part = json!({ "functionCall": { "name": n, "args": {} } });
            if let Some(sig) = thought_signature.as_deref().filter(|s| !s.is_empty()) {
                part["thoughtSignature"] = json!(sig);
            }
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [part]
                    }
                }]
            })
        }
        IrStreamEvent::CustomToolCallStart { id, name, .. } => {
            let n = if name.is_empty() { id } else { name };
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{ "functionCall": { "name": n, "args": {} } }]
                    }
                }]
            })
        }
        IrStreamEvent::AnnotationAdded { annotation } => json!({
            "candidates": [{
                "groundingMetadata": {
                    "groundingChunks": [grounding_chunk_from_annotation(annotation)]
                }
            }]
        }),
        IrStreamEvent::AudioDelta { data } => json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "inlineData": { "mimeType": "audio/mpeg", "data": data }
                    }]
                }
            }]
        }),
        IrStreamEvent::ToolCallArgDelta { delta, .. }
        | IrStreamEvent::CustomToolCallInputDelta { delta, .. } => {
            let args: Value = serde_json::from_str(delta).unwrap_or_else(|_| json!({}));
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{ "functionCall": { "name": "", "args": args } }]
                    }
                }]
            })
        }
        IrStreamEvent::ToolCallEnd => json!({
            "candidates": [{ "content": { "role": "model", "parts": [] } }]
        }),
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            reasoning_tokens,
            ..
        } => usage::encode_gemini(
            *prompt_tokens,
            *completion_tokens,
            *cache_read_tokens,
            *reasoning_tokens,
        ),
        IrStreamEvent::FinishReason { reason } => json!({
            "candidates": [{ "finishReason": encode_finish(reason) }]
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

pub(super) fn annotation_from_grounding_chunk(chunk: &Value) -> Option<Value> {
    let url = chunk
        .pointer("/web/uri")
        .or_else(|| chunk.pointer("/web/url"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let mut out = json!({ "type": "url_citation", "url": url });
    if let Some(title) = chunk
        .pointer("/web/title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out["title"] = json!(title);
    }
    Some(out)
}

pub(super) fn grounding_chunk_from_annotation(annotation: &Value) -> Value {
    let url = super::chat::annotation_url(annotation).unwrap_or("");
    let mut web = json!({ "uri": url });
    if let Some(title) = super::chat::annotation_title(annotation) {
        web["title"] = json!(title);
    }
    json!({ "web": web })
}

pub(super) fn encode_finish(reason: &str) -> &'static str {
    match reason {
        "max_tokens" | "length" => "MAX_TOKENS",
        "content_filter" => "SAFETY",
        "tool_calls" => "STOP",
        _ => "STOP",
    }
}

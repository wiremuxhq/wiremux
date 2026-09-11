//! Gemini generateContent SSE (`data:` JSON chunks).

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

use super::usage;
use super::{RawSse, str_field};

pub(super) fn decode(value: &Value) -> Result<Option<IrStreamEvent>, MapError> {
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
                if args.is_some_and(|a| !a.as_object().is_some_and(serde_json::Map::is_empty))
                    && thought_signature.is_none()
                {
                    return Ok(Some(IrStreamEvent::Protocol {
                        item_type: "chunk".into(),
                        payload: value.clone(),
                    }));
                }
                return Ok(Some(IrStreamEvent::ToolCallStart {
                    id: name.clone(),
                    name,
                    thought_signature,
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

    if let Some(reason) = candidate
        .get("finishReason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(IrStreamEvent::FinishReason {
            reason: map_finish(reason).to_string(),
        }));
    }

    if let Some(usage) = value.get("usageMetadata").filter(|v| v.is_object()) {
        return Ok(Some(usage::from_gemini(usage)));
    }
    Ok(None)
}

fn map_finish(reason: &str) -> &'static str {
    match reason {
        "STOP" => "stop",
        "MAX_TOKENS" => "max_tokens",
        "SAFETY" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "IMAGE_SAFETY" | "LANGUAGE" => {
            "content_filter"
        }
        "MALFORMED_FUNCTION_CALL" => "tool_calls",
        other if other.eq_ignore_ascii_case("stop") => "stop",
        _ => "stop",
    }
}

pub(super) fn encode(ev: &IrStreamEvent) -> Result<RawSse, MapError> {
    let data = match ev {
        IrStreamEvent::TextDelta { text } => json!({
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
        IrStreamEvent::ToolCallArgDelta { delta } => {
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

fn encode_finish(reason: &str) -> &'static str {
    match reason {
        "max_tokens" => "MAX_TOKENS",
        "content_filter" => "SAFETY",
        "tool_calls" => "STOP",
        _ => "STOP",
    }
}

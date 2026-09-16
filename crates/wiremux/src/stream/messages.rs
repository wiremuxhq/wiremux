//! Anthropic Messages SSE (`event:` + `data:`).

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

use super::usage;
use super::{MAX_CONTENT_BLOCK_INDEX, RawSse, check_index, protocol, str_field};

pub(super) fn decode(name: &str, value: &Value) -> Result<Option<IrStreamEvent>, MapError> {
    match name {
        "ping" => Ok(None),
        "error" => {
            let err = value.get("error").unwrap_or(value);
            let ty = str_field(err, "type").unwrap_or_else(|| "error".into());
            let msg = str_field(err, "message").unwrap_or_default();
            Err(MapError::Invalid(format!("{ty}: {msg}")))
        }
        "message_stop" => Ok(Some(IrStreamEvent::Done)),
        "message_start" => {
            if let Some(usage) = value.pointer("/message/usage") {
                Ok(Some(usage::from_anthropic(usage)))
            } else {
                Ok(Some(protocol(name, value)))
            }
        }
        "content_block_start" => {
            check_index(value, "index", MAX_CONTENT_BLOCK_INDEX, "content block")?;
            let block = value.get("content_block").unwrap_or(value);
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => Ok(Some(IrStreamEvent::ToolCallStart {
                    id: str_field(block, "id").unwrap_or_default(),
                    name: str_field(block, "name").unwrap_or_default(),
                    thought_signature: None,
                    index: block_index(value),
                })),
                _ => Ok(Some(protocol(name, value))),
            }
        }
        "content_block_delta" => {
            check_index(value, "index", MAX_CONTENT_BLOCK_INDEX, "content block")?;
            let delta = value.get("delta").unwrap_or(value);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => nonempty_text(str_field(delta, "text"), |text| {
                    IrStreamEvent::TextDelta { text }
                }),
                Some("thinking_delta") => nonempty_text(str_field(delta, "thinking"), |text| {
                    IrStreamEvent::ReasoningDelta { text }
                }),
                Some("signature_delta") => {
                    nonempty_text(str_field(delta, "signature"), |signature| {
                        IrStreamEvent::ReasoningSignature { signature }
                    })
                }
                Some("input_json_delta") => Ok(Some(IrStreamEvent::ToolCallArgDelta {
                    delta: str_field(delta, "partial_json").unwrap_or_default(),
                    index: block_index(value),
                })),
                _ => Ok(Some(protocol(name, value))),
            }
        }
        "content_block_stop" => {
            check_index(value, "index", MAX_CONTENT_BLOCK_INDEX, "content block")?;
            Ok(Some(protocol(name, value)))
        }
        "message_delta" => {
            // stop_reason only appears here; usage already arrived on message_start.
            if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                Ok(Some(IrStreamEvent::FinishReason {
                    reason: map_stop_reason(reason).to_string(),
                }))
            } else if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
                Ok(Some(usage::from_anthropic(usage)))
            } else {
                Ok(Some(protocol(name, value)))
            }
        }
        other => Ok(Some(protocol(other, value))),
    }
}

fn block_index(value: &Value) -> u32 {
    value
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

fn nonempty_text(
    text: Option<String>,
    wrap: impl FnOnce(String) -> IrStreamEvent,
) -> Result<Option<IrStreamEvent>, MapError> {
    Ok(text.filter(|s| !s.is_empty()).map(wrap))
}

pub(super) fn encode(ev: &IrStreamEvent) -> Result<RawSse, MapError> {
    let (event, data) = match ev {
        IrStreamEvent::TextDelta { text } => (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": text }
            }),
        ),
        IrStreamEvent::ReasoningDelta { text } => (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "thinking_delta", "thinking": text }
            }),
        ),
        IrStreamEvent::ReasoningSignature { signature } => (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "signature_delta", "signature": signature }
            }),
        ),
        IrStreamEvent::ToolCallStart {
            id, name, index, ..
        } => (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} }
            }),
        ),
        IrStreamEvent::ToolCallArgDelta { delta, index } => (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": delta }
            }),
        ),
        IrStreamEvent::ToolCallEnd => (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        } => (
            "message_delta",
            usage::encode_anthropic(
                *prompt_tokens,
                *completion_tokens,
                *cache_read_tokens,
                *cache_write_tokens,
                *reasoning_tokens,
            ),
        ),
        IrStreamEvent::FinishReason { reason } => (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": encode_stop_reason(reason), "stop_sequence": null }
            }),
        ),
        IrStreamEvent::Done => ("message_stop", json!({ "type": "message_stop" })),
        IrStreamEvent::Protocol { .. } | IrStreamEvent::Unknown { .. } => {
            return Err(MapError::Invalid(
                "protocol/unknown events are encoded at the stream root".into(),
            ));
        }
    };
    Ok(RawSse {
        event: Some(event.into()),
        data: data.to_string(),
    })
}

pub(super) fn map_stop_reason(reason: &str) -> &str {
    match reason {
        "refusal" => "content_filter",
        "max_tokens" => "max_tokens",
        "tool_use" => "tool_calls",
        "end_turn" => "stop",
        other => other,
    }
}

pub(super) fn encode_stop_reason(reason: &str) -> &str {
    match reason {
        "stop" | "end_turn" => "end_turn",
        "length" | "max_tokens" => "max_tokens",
        "tool_calls" | "tool_use" => "tool_use",
        other => other,
    }
}

//! OpenAI Responses SSE.

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

use super::usage;
use super::{MAX_TOOL_CALL_INDEX, RawSse, check_index, protocol, str_field};

pub(super) fn decode(name: &str, value: &Value) -> Result<Option<IrStreamEvent>, MapError> {
    check_responses_indexes(value)?;
    match name {
        "response.output_text.delta" => {
            nonempty_delta(value, |text| IrStreamEvent::TextDelta { text })
        }
        "response.function_call_arguments.delta" => Ok(Some(IrStreamEvent::ToolCallArgDelta {
            delta: str_field(value, "delta").unwrap_or_default(),
        })),
        "response.output_item.added" => match item_type(value) {
            Some("function_call") => {
                let item = value.get("item").unwrap_or(value);
                Ok(Some(IrStreamEvent::ToolCallStart {
                    id: str_field(item, "call_id")
                        .or_else(|| str_field(item, "id"))
                        .unwrap_or_default(),
                    name: str_field(item, "name").unwrap_or_default(),
                }))
            }
            _ => Ok(Some(protocol(name, value))),
        },
        "response.output_item.done" => match item_type(value) {
            Some("function_call") => Ok(Some(IrStreamEvent::ToolCallEnd)),
            _ => Ok(Some(protocol(name, value))),
        },
        "response.completed" => {
            if let Some(usage) = value.pointer("/response/usage") {
                Ok(Some(usage::from_responses(usage)))
            } else {
                Ok(Some(IrStreamEvent::Done))
            }
        }
        "response.failed" => Ok(Some(IrStreamEvent::FinishReason {
            reason: "failed".into(),
        })),
        "response.incomplete" => Ok(Some(IrStreamEvent::FinishReason {
            reason: "incomplete".into(),
        })),
        other => Ok(Some(protocol(other, value))),
    }
}

fn item_type(value: &Value) -> Option<&str> {
    value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
}

fn nonempty_delta(
    value: &Value,
    wrap: impl FnOnce(String) -> IrStreamEvent,
) -> Result<Option<IrStreamEvent>, MapError> {
    Ok(str_field(value, "delta")
        .filter(|s| !s.is_empty())
        .map(wrap))
}

fn check_responses_indexes(value: &Value) -> Result<(), MapError> {
    check_index(value, "output_index", MAX_TOOL_CALL_INDEX, "output")?;
    check_index(value, "content_index", MAX_TOOL_CALL_INDEX, "content")?;
    Ok(())
}

pub(super) fn encode(ev: &IrStreamEvent) -> Result<RawSse, MapError> {
    let (event, data) = match ev {
        IrStreamEvent::TextDelta { text } => (
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "delta": text
            }),
        ),
        IrStreamEvent::ReasoningDelta { text } => (
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "delta": text
            }),
        ),
        IrStreamEvent::ReasoningSignature { signature } => (
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "reasoning", "signature": signature }
            }),
        ),
        IrStreamEvent::ToolCallStart { id, name } => (
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": id,
                    "call_id": id,
                    "name": name,
                    "arguments": ""
                }
            }),
        ),
        IrStreamEvent::ToolCallArgDelta { delta } => (
            "response.function_call_arguments.delta",
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": delta
            }),
        ),
        IrStreamEvent::ToolCallEnd => (
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": { "type": "function_call" }
            }),
        ),
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            reasoning_tokens,
            ..
        } => (
            "response.completed",
            usage::encode_responses(
                *prompt_tokens,
                *completion_tokens,
                *cache_read_tokens,
                *reasoning_tokens,
            ),
        ),
        IrStreamEvent::FinishReason { reason } => {
            let event = if reason == "failed" {
                "response.failed"
            } else if reason == "incomplete" {
                "response.incomplete"
            } else {
                "response.completed"
            };
            (
                event,
                json!({
                    "type": event,
                    "response": { "status": reason }
                }),
            )
        }
        IrStreamEvent::Done => (
            "response.completed",
            json!({
                "type": "response.completed",
                "response": { "status": "completed" }
            }),
        ),
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

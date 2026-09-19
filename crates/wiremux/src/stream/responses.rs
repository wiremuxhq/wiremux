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
            if let Some(text) = str_field(value, "delta").filter(|s| !s.is_empty()) {
                return Ok(Some(IrStreamEvent::TextDelta { text }));
            }
            if let Some(content) = logprobs_array(value) {
                return Ok(Some(IrStreamEvent::Logprobs { content }));
            }
            Ok(None)
        }
        "response.reasoning_summary_text.delta" | "response.reasoning.delta" => {
            nonempty_delta(value, |text| IrStreamEvent::ReasoningDelta { text })
        }
        "response.refusal.delta" => {
            nonempty_delta(value, |text| IrStreamEvent::RefusalDelta { text })
        }
        "response.function_call_arguments.delta" => Ok(Some(IrStreamEvent::ToolCallArgDelta {
            delta: str_field(value, "delta").unwrap_or_default(),
            index: output_index(value),
        })),
        "response.output_text.annotation.added" => match value.get("annotation") {
            Some(annotation) if annotation.is_object() => {
                Ok(Some(IrStreamEvent::AnnotationAdded {
                    annotation: annotation.clone(),
                }))
            }
            _ => Ok(Some(protocol(name, value))),
        },
        "response.audio.delta" => nonempty_delta(value, |data| IrStreamEvent::AudioDelta { data }),
        "response.audio.transcript.delta" => {
            nonempty_delta(value, |text| IrStreamEvent::AudioTranscriptDelta { text })
        }
        "response.custom_tool_call_input.delta" => {
            Ok(Some(IrStreamEvent::CustomToolCallInputDelta {
                delta: str_field(value, "delta").unwrap_or_default(),
                index: output_index(value),
            }))
        }
        "response.output_item.added" => match item_type(value) {
            Some("function_call") => {
                let item = value.get("item").unwrap_or(value);
                Ok(Some(IrStreamEvent::ToolCallStart {
                    id: str_field(item, "call_id")
                        .or_else(|| str_field(item, "id"))
                        .unwrap_or_default(),
                    name: str_field(item, "name").unwrap_or_default(),
                    thought_signature: None,
                    index: output_index(value),
                }))
            }
            Some("custom_tool_call") => {
                let item = value.get("item").unwrap_or(value);
                Ok(Some(IrStreamEvent::CustomToolCallStart {
                    id: str_field(item, "call_id")
                        .or_else(|| str_field(item, "id"))
                        .unwrap_or_default(),
                    name: str_field(item, "name").unwrap_or_default(),
                    index: output_index(value),
                }))
            }
            Some("reasoning") => Ok(Some(decode_reasoning_item(name, value))),
            _ => Ok(Some(protocol(name, value))),
        },
        "response.output_item.done" => match item_type(value) {
            Some("function_call") | Some("custom_tool_call") => {
                Ok(Some(IrStreamEvent::ToolCallEnd))
            }
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
            reason: decode_incomplete_reason(value, "incomplete"),
        })),
        other => Ok(Some(protocol(other, value))),
    }
}

/// Text and logprobs on one `response.output_text.delta` frame.
pub(super) fn decode_all(name: &str, value: &Value) -> Result<Vec<IrStreamEvent>, MapError> {
    check_responses_indexes(value)?;
    if name != "response.output_text.delta" {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    if let Some(text) = str_field(value, "delta").filter(|s| !s.is_empty()) {
        out.push(IrStreamEvent::TextDelta { text });
    }
    if let Some(content) = logprobs_array(value) {
        out.push(IrStreamEvent::Logprobs { content });
    }
    Ok(out)
}

fn logprobs_array(value: &Value) -> Option<Value> {
    value
        .get("logprobs")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .map(|a| Value::Array(a.clone()))
}

/// Fan-out for `response.completed` / `response.incomplete`.
///
/// 1:1 [`decode`] keeps Usage-or-Done / FinishReason. This walk emits
/// Protocol (encrypted reasoning), FinishReason from `response.status`,
/// then Usage when more than one signal is present.
pub(super) fn decode_terminal_events(name: &str, value: &Value) -> Option<Vec<IrStreamEvent>> {
    if name != "response.completed" && name != "response.incomplete" {
        return None;
    }
    let mut out = Vec::new();
    if let Some(items) = value.pointer("/response/output").and_then(Value::as_array) {
        for item in items {
            if item
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_none_or(|s| s.is_empty())
            {
                continue;
            }
            out.push(IrStreamEvent::Protocol {
                item_type: item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("reasoning")
                    .to_string(),
                payload: item.clone(),
            });
        }
    }
    if let Some(reason) = terminal_finish_reason(name, value) {
        out.push(IrStreamEvent::FinishReason { reason });
    }
    if let Some(usage) = value.pointer("/response/usage").filter(|v| v.is_object()) {
        out.push(usage::from_responses(usage));
    }
    (out.len() >= 2).then_some(out)
}

fn terminal_finish_reason(name: &str, value: &Value) -> Option<String> {
    match value.pointer("/response/status").and_then(Value::as_str) {
        Some("completed") => Some("stop".into()),
        Some("incomplete") => Some(decode_incomplete_reason(value, "length")),
        Some("failed") => Some("failed".into()),
        Some(other) if !other.is_empty() => Some(other.to_string()),
        None if name == "response.incomplete" => {
            Some(decode_incomplete_reason(value, "incomplete"))
        }
        None if name == "response.completed"
            && value
                .pointer("/response/usage")
                .is_some_and(Value::is_object) =>
        {
            Some("stop".into())
        }
        _ => None,
    }
}

pub(super) fn incomplete_details_reason(ir_reason: &str) -> Option<&'static str> {
    match ir_reason {
        "content_filter" => Some("content_filter"),
        "length" | "max_tokens" => Some("max_output_tokens"),
        _ => None,
    }
}

fn decode_incomplete_reason(value: &Value, fallback: &str) -> String {
    match value
        .pointer("/response/incomplete_details/reason")
        .and_then(Value::as_str)
    {
        Some("content_filter") => "content_filter".into(),
        Some("max_output_tokens") => "length".into(),
        _ => fallback.into(),
    }
}

fn output_index(value: &Value) -> u32 {
    value
        .get("output_index")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

fn item_type(value: &Value) -> Option<&str> {
    value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
}

fn decode_reasoning_item(name: &str, value: &Value) -> IrStreamEvent {
    let item = value.get("item").unwrap_or(value);
    if let Some(signature) = str_field(item, "signature").filter(|s| !s.is_empty()) {
        return IrStreamEvent::ReasoningSignature { signature };
    }
    if let Some(text) = str_field(item, "text").filter(|s| !s.is_empty()) {
        return IrStreamEvent::ReasoningDelta { text };
    }
    protocol(name, value)
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
        IrStreamEvent::Logprobs { content } => (
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "delta": "",
                "logprobs": content
            }),
        ),
        IrStreamEvent::RefusalDelta { text } => (
            "response.refusal.delta",
            json!({
                "type": "response.refusal.delta",
                "output_index": 0,
                "delta": text
            }),
        ),
        IrStreamEvent::ReasoningDelta { text } => (
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "reasoning", "text": text }
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
        IrStreamEvent::ToolCallStart {
            id, name, index, ..
        } => (
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": index,
                "item": {
                    "type": "function_call",
                    "id": id,
                    "call_id": id,
                    "name": name,
                    "arguments": ""
                }
            }),
        ),
        IrStreamEvent::AnnotationAdded { annotation } => (
            "response.output_text.annotation.added",
            json!({
                "type": "response.output_text.annotation.added",
                "output_index": 0,
                "content_index": 0,
                "annotation_index": 0,
                "annotation": annotation
            }),
        ),
        IrStreamEvent::AudioDelta { data } => (
            "response.audio.delta",
            json!({
                "type": "response.audio.delta",
                "delta": data
            }),
        ),
        IrStreamEvent::AudioTranscriptDelta { text } => (
            "response.audio.transcript.delta",
            json!({
                "type": "response.audio.transcript.delta",
                "delta": text
            }),
        ),
        IrStreamEvent::CustomToolCallStart { id, name, index } => (
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": index,
                "item": {
                    "type": "custom_tool_call",
                    "id": id,
                    "call_id": id,
                    "name": name,
                    "input": ""
                }
            }),
        ),
        IrStreamEvent::CustomToolCallInputDelta { delta, index } => (
            "response.custom_tool_call_input.delta",
            json!({
                "type": "response.custom_tool_call_input.delta",
                "output_index": index,
                "delta": delta
            }),
        ),
        IrStreamEvent::ToolCallArgDelta { delta, index } => (
            "response.function_call_arguments.delta",
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": index,
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
            cache_write_tokens,
            reasoning_tokens,
        } => (
            "response.completed",
            usage::encode_responses(
                *prompt_tokens,
                *completion_tokens,
                *cache_read_tokens,
                *cache_write_tokens,
                *reasoning_tokens,
            ),
        ),
        IrStreamEvent::FinishReason { reason } => {
            let (event, status) = match reason.as_str() {
                "failed" => ("response.failed", "failed"),
                "incomplete" | "length" | "max_tokens" | "content_filter" => {
                    ("response.incomplete", "incomplete")
                }
                "stop" => ("response.completed", "completed"),
                other => ("response.completed", other),
            };
            let mut response = json!({ "status": status });
            if let Some(detail) = incomplete_details_reason(reason) {
                response["incomplete_details"] = json!({ "reason": detail });
            }
            (
                event,
                json!({
                    "type": event,
                    "response": response
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

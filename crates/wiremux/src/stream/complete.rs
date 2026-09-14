//! Complete JSON bodies (non-SSE). Chat stream maps stay delta-only.

use serde_json::{Value, json};
use wiremux_auth::{ResolvedProfile, Wire};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

use super::usage::{from_anthropic, from_chat};
use super::{MAX_TOOL_CALL_INDEX, RawSse, check_index, decode_stream_events, str_field};

/// Decode a complete (non-SSE) vendor body into IR stream events.
///
/// Chat Completions reads `choices[0].message`, not `delta`. A delta-only
/// body does not invent a complete message.
pub fn decode_response(
    wire: Wire,
    bytes: &[u8],
    profile: &ResolvedProfile,
) -> Result<Vec<IrStreamEvent>, MapError> {
    let value: Value = serde_json::from_slice(bytes)?;
    match wire {
        Wire::ChatCompletions => decode_chat_complete(&value),
        Wire::Messages => decode_messages_complete(&value),
        Wire::Responses => decode_responses_complete(&value, profile),
        Wire::Gemini => decode_gemini_complete(&value, profile),
    }
}

fn decode_chat_complete(value: &Value) -> Result<Vec<IrStreamEvent>, MapError> {
    let mut out = Vec::new();
    if let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
    {
        if let Some(message) = choice.get("message") {
            if let Some(text) = message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.push(IrStreamEvent::ReasoningDelta {
                    text: text.to_string(),
                });
            }
            if let Some(signature) = message
                .get("reasoning_signature")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.push(IrStreamEvent::ReasoningSignature {
                    signature: signature.to_string(),
                });
            }
            if let Some(text) = message
                .get("content")
                .and_then(super::chat::flatten_content)
            {
                out.push(IrStreamEvent::TextDelta { text });
            }
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    check_index(call, "index", MAX_TOOL_CALL_INDEX, "tool call")?;
                    out.extend(complete_chat_tool_call(call));
                }
            }
        }
        if let Some(reason) = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            out.push(IrStreamEvent::FinishReason {
                reason: super::chat::map_finish(reason).to_string(),
            });
        }
    }
    if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
        out.push(from_chat(usage));
    }
    Ok(out)
}

fn complete_chat_tool_call(call: &Value) -> Vec<IrStreamEvent> {
    if let Some(ty) = call.get("type").and_then(Value::as_str)
        && ty != "function"
    {
        return vec![IrStreamEvent::Protocol {
            item_type: "chunk".into(),
            payload: call.clone(),
        }];
    }
    let func = call.get("function").unwrap_or(call);
    let id = str_field(call, "id").unwrap_or_default();
    let name = str_field(func, "name").unwrap_or_default();
    let args = str_field(func, "arguments");
    let mut out = vec![IrStreamEvent::ToolCallStart {
        id,
        name,
        thought_signature: None,
    }];
    if let Some(delta) = args {
        out.push(IrStreamEvent::ToolCallArgDelta { delta });
    }
    out.push(IrStreamEvent::ToolCallEnd);
    out
}

fn decode_messages_complete(value: &Value) -> Result<Vec<IrStreamEvent>, MapError> {
    let mut out = Vec::new();
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = str_field(block, "text").filter(|s| !s.is_empty()) {
                        out.push(IrStreamEvent::TextDelta { text });
                    }
                }
                Some("thinking") => {
                    if let Some(text) = str_field(block, "thinking").filter(|s| !s.is_empty()) {
                        out.push(IrStreamEvent::ReasoningDelta { text });
                    }
                    if let Some(signature) = str_field(block, "signature").filter(|s| !s.is_empty())
                    {
                        out.push(IrStreamEvent::ReasoningSignature { signature });
                    }
                }
                Some("tool_use") => {
                    out.push(IrStreamEvent::ToolCallStart {
                        id: str_field(block, "id").unwrap_or_default(),
                        name: str_field(block, "name").unwrap_or_default(),
                        thought_signature: None,
                    });
                    if let Some(input) = block.get("input").filter(|v| !v.is_null()) {
                        out.push(IrStreamEvent::ToolCallArgDelta {
                            delta: input.to_string(),
                        });
                    }
                    out.push(IrStreamEvent::ToolCallEnd);
                }
                Some(ty) => {
                    out.push(IrStreamEvent::Protocol {
                        item_type: ty.to_string(),
                        payload: block.clone(),
                    });
                }
                None => {}
            }
        }
    }
    if let Some(reason) = value.get("stop_reason").and_then(Value::as_str) {
        out.push(IrStreamEvent::FinishReason {
            reason: super::messages::map_stop_reason(reason).to_string(),
        });
    }
    if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
        out.push(from_anthropic(usage));
    }
    Ok(out)
}

fn decode_responses_complete(
    value: &Value,
    profile: &ResolvedProfile,
) -> Result<Vec<IrStreamEvent>, MapError> {
    let name = value.get("type").and_then(Value::as_str).unwrap_or("");
    let (event, data) = if name == "response.completed" || name == "response.incomplete" {
        (name.to_string(), value.to_string())
    } else {
        let status = value.get("status").and_then(Value::as_str).unwrap_or("");
        let event = match status {
            "incomplete" => "response.incomplete",
            "failed" => "response.failed",
            _ => "response.completed",
        };
        (
            event.to_string(),
            json!({ "type": event, "response": value }).to_string(),
        )
    };
    let mut events = decode_stream_events(
        Wire::Responses,
        &RawSse {
            event: Some(event),
            data,
        },
        profile,
    )?;
    let extra = complete_responses_output_events(value);
    if extra.is_empty() {
        return Ok(events);
    }
    let insert_at = events
        .iter()
        .position(|ev| {
            matches!(
                ev,
                IrStreamEvent::FinishReason { .. }
                    | IrStreamEvent::Usage { .. }
                    | IrStreamEvent::Done
            )
        })
        .unwrap_or(events.len());
    events.splice(insert_at..insert_at, extra);
    Ok(events)
}

fn complete_responses_output_events(value: &Value) -> Vec<IrStreamEvent> {
    let Some(items) = value
        .get("output")
        .and_then(Value::as_array)
        .or_else(|| value.pointer("/response/output").and_then(Value::as_array))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let Some(content) = item.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for part in content {
                    let ty = part.get("type").and_then(Value::as_str);
                    match ty {
                        Some("output_text") | Some("text") => {
                            if let Some(text) = str_field(part, "text").filter(|s| !s.is_empty()) {
                                out.push(IrStreamEvent::TextDelta { text });
                            }
                        }
                        Some("refusal") => {
                            if let Some(text) = str_field(part, "refusal")
                                .or_else(|| str_field(part, "text"))
                                .filter(|s| !s.is_empty())
                            {
                                out.push(IrStreamEvent::TextDelta { text });
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("function_call") => {
                out.push(IrStreamEvent::ToolCallStart {
                    id: str_field(item, "call_id")
                        .or_else(|| str_field(item, "id"))
                        .unwrap_or_default(),
                    name: str_field(item, "name").unwrap_or_default(),
                    thought_signature: None,
                });
                if let Some(delta) = str_field(item, "arguments").filter(|s| !s.is_empty()) {
                    out.push(IrStreamEvent::ToolCallArgDelta { delta });
                }
                out.push(IrStreamEvent::ToolCallEnd);
            }
            Some("reasoning") => {
                if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                    for part in parts {
                        let ty = part.get("type").and_then(Value::as_str);
                        if matches!(ty, Some("summary_text") | Some("text"))
                            && let Some(text) = str_field(part, "text").filter(|s| !s.is_empty())
                        {
                            out.push(IrStreamEvent::ReasoningDelta { text });
                        }
                    }
                }
                if item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                {
                    out.push(IrStreamEvent::Protocol {
                        item_type: "reasoning".into(),
                        payload: item.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn decode_gemini_complete(
    value: &Value,
    profile: &ResolvedProfile,
) -> Result<Vec<IrStreamEvent>, MapError> {
    decode_stream_events(
        Wire::Gemini,
        &RawSse {
            event: None,
            data: value.to_string(),
        },
        profile,
    )
}

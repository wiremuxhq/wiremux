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
        return Ok(None);
    };

    if let Some(calls) = choice
        .pointer("/delta/tool_calls")
        .and_then(Value::as_array)
    {
        for call in calls {
            check_index(call, "index", MAX_TOOL_CALL_INDEX, "tool call")?;
        }
        if calls.len() > 1 {
            return Ok(Some(IrStreamEvent::Protocol {
                item_type: "chunk".into(),
                payload: value.clone(),
            }));
        }
        if let Some(call) = calls.first() {
            return Ok(Some(decode_tool_call(call, value)));
        }
    }

    let delta = choice.get("delta");
    if let Some(text) = delta
        .and_then(|d| d.get("content"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(IrStreamEvent::TextDelta {
            text: text.to_string(),
        }));
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
    if let Some(reason) = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(IrStreamEvent::FinishReason {
            reason: map_finish(reason).to_string(),
        }));
    }
    if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
        return Ok(Some(usage::from_chat(usage)));
    }
    Ok(None)
}

fn map_finish(reason: &str) -> &str {
    match reason {
        "eos" => "stop",
        "function_call" => "tool_calls",
        "content_filter" | "content-filter" => "content_filter",
        other => other,
    }
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
    let func = call.get("function").unwrap_or(call);
    let id = str_field(call, "id");
    let name = str_field(func, "name");
    let args = str_field(func, "arguments").filter(|s| !s.is_empty());
    // 1:1 API cannot emit Start and ArgDelta together; keep the whole chunk.
    if (id.is_some() || name.is_some()) && args.is_some() {
        return keep();
    }
    if id.is_some() || name.is_some() {
        return IrStreamEvent::ToolCallStart {
            id: id.unwrap_or_default(),
            name: name.unwrap_or_default(),
            thought_signature: None,
        };
    }
    match args {
        Some(delta) => IrStreamEvent::ToolCallArgDelta { delta },
        None => keep(),
    }
}

pub(super) fn encode(ev: &IrStreamEvent) -> Result<RawSse, MapError> {
    let data = match ev {
        IrStreamEvent::TextDelta { text } => json!({
            "choices": [{ "index": 0, "delta": { "content": text } }]
        }),
        IrStreamEvent::ReasoningDelta { text } => json!({
            "choices": [{ "index": 0, "delta": { "reasoning_content": text } }]
        }),
        IrStreamEvent::ReasoningSignature { signature } => json!({
            "choices": [{ "index": 0, "delta": { "reasoning_signature": signature } }]
        }),
        IrStreamEvent::ToolCallStart { id, name, .. } => json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": "" }
                    }]
                }
            }]
        }),
        IrStreamEvent::ToolCallArgDelta { delta } => json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": delta }
                    }]
                }
            }]
        }),
        IrStreamEvent::ToolCallEnd => json!({
            "choices": [{ "index": 0, "delta": {} }]
        }),
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        } => usage::encode_chat(
            *prompt_tokens,
            *completion_tokens,
            *cache_read_tokens,
            *cache_write_tokens,
            *reasoning_tokens,
        ),
        IrStreamEvent::FinishReason { reason } => json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }]
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

//! Bedrock Converse stream and complete JSON (not AWS eventstream bytes).

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

pub(super) fn decode(value: &Value) -> Result<Option<IrStreamEvent>, MapError> {
    if let Some(delta) = value.pointer("/contentBlockDelta/delta") {
        if let Some(text) = delta.get("text").and_then(Value::as_str) {
            return Ok(Some(IrStreamEvent::TextDelta {
                text: text.to_string(),
            }));
        }
        if let Some(text) = delta
            .pointer("/reasoningContent/text")
            .and_then(Value::as_str)
        {
            return Ok(Some(IrStreamEvent::ReasoningDelta {
                text: text.to_string(),
            }));
        }
        if let Some(input) = delta.pointer("/toolUse/input").and_then(Value::as_str) {
            return Ok(Some(IrStreamEvent::ToolCallArgDelta {
                delta: input.to_string(),
            }));
        }
    }
    if let Some(start) = value.get("contentBlockStart")
        && let Some(tool) = start.pointer("/start/toolUse")
    {
        let id = tool
            .get("toolUseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Ok(Some(IrStreamEvent::ToolCallStart {
            id,
            name,
            thought_signature: None,
        }));
    }
    if value.get("contentBlockStop").is_some() {
        // AWS also emits this for text/reasoning blocks.
        return Ok(None);
    }
    if let Some(stop) = value
        .pointer("/messageStop/stopReason")
        .and_then(Value::as_str)
    {
        return Ok(Some(IrStreamEvent::FinishReason {
            reason: stop.to_string(),
        }));
    }
    if let Some(usage) = value.pointer("/metadata/usage") {
        return Ok(Some(IrStreamEvent::Usage {
            prompt_tokens: usage
                .get("inputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            completion_tokens: usage
                .get("outputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        }));
    }
    if value.get("messageStart").is_some() {
        return Ok(None);
    }
    Ok(Some(IrStreamEvent::Unknown {
        event: "converse".into(),
        raw: value.clone(),
    }))
}

pub(super) fn encode(ev: &IrStreamEvent) -> Result<Value, MapError> {
    match ev {
        IrStreamEvent::TextDelta { text } => Ok(json!({
            "contentBlockDelta": { "delta": { "text": text } }
        })),
        IrStreamEvent::ReasoningDelta { text } => Ok(json!({
            "contentBlockDelta": { "delta": { "reasoningContent": { "text": text } } }
        })),
        IrStreamEvent::ToolCallStart { id, name, .. } => Ok(json!({
            "contentBlockStart": {
                "start": { "toolUse": { "toolUseId": id, "name": name } }
            }
        })),
        IrStreamEvent::ToolCallArgDelta { delta } => Ok(json!({
            "contentBlockDelta": { "delta": { "toolUse": { "input": delta } } }
        })),
        IrStreamEvent::ToolCallEnd => Ok(json!({ "contentBlockStop": {} })),
        IrStreamEvent::FinishReason { reason } => Ok(json!({
            "messageStop": { "stopReason": reason }
        })),
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            ..
        } => Ok(json!({
            "metadata": {
                "usage": {
                    "inputTokens": prompt_tokens,
                    "outputTokens": completion_tokens
                }
            }
        })),
        IrStreamEvent::Done => Ok(json!({ "messageStop": { "stopReason": "end_turn" } })),
        other => Err(MapError::Invalid(format!(
            "converse stream cannot encode {other:?}"
        ))),
    }
}

pub(super) fn encode_complete(events: &[IrStreamEvent]) -> Value {
    let mut text = String::new();
    let mut content = Vec::new();
    let mut current_tool: Option<(String, String, String)> = None;
    let mut stop = "end_turn";
    let mut usage = None;
    for ev in events {
        match ev {
            IrStreamEvent::TextDelta { text: delta } => text.push_str(delta),
            IrStreamEvent::ToolCallStart { id, name, .. } => {
                flush_text(&mut text, &mut content);
                if let Some((id, name, args)) = current_tool.take() {
                    content.push(tool_use(&id, &name, &args));
                }
                current_tool = Some((id.clone(), name.clone(), String::new()));
            }
            IrStreamEvent::ToolCallArgDelta { delta } => {
                if let Some((_, _, args)) = current_tool.as_mut() {
                    args.push_str(delta);
                }
            }
            IrStreamEvent::ToolCallEnd => {
                if let Some((id, name, args)) = current_tool.take() {
                    content.push(tool_use(&id, &name, &args));
                    stop = "tool_use";
                }
            }
            IrStreamEvent::FinishReason { reason } => stop = finish_reason(reason),
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                ..
            } => {
                usage = Some((*prompt_tokens, *completion_tokens));
            }
            _ => {}
        }
    }
    flush_text(&mut text, &mut content);
    if let Some((id, name, args)) = current_tool.take() {
        content.push(tool_use(&id, &name, &args));
        if stop == "end_turn" {
            stop = "tool_use";
        }
    }
    let mut body = json!({
        "output": { "message": { "role": "assistant", "content": content } },
        "stopReason": stop
    });
    if let Some((input, output)) = usage {
        body["usage"] = json!({
            "inputTokens": input,
            "outputTokens": output
        });
    }
    body
}

pub(super) fn decode_complete(value: &Value) -> Result<Vec<IrStreamEvent>, MapError> {
    let mut out = Vec::new();
    let blocks = value
        .pointer("/output/message/content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for block in &blocks {
        if let Some(text) = block.get("text").and_then(Value::as_str)
            && !text.is_empty()
        {
            out.push(IrStreamEvent::TextDelta {
                text: text.to_string(),
            });
        }
        if let Some(tool) = block.get("toolUse") {
            let id = tool
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let args = tool
                .get("input")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "{}".into());
            out.push(IrStreamEvent::ToolCallStart {
                id,
                name,
                thought_signature: None,
            });
            out.push(IrStreamEvent::ToolCallArgDelta { delta: args });
            out.push(IrStreamEvent::ToolCallEnd);
        }
    }
    if let Some(reason) = value.get("stopReason").and_then(Value::as_str) {
        out.push(IrStreamEvent::FinishReason {
            reason: reason.to_string(),
        });
    }
    if let Some(usage) = value.get("usage") {
        out.push(IrStreamEvent::Usage {
            prompt_tokens: usage
                .get("inputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            completion_tokens: usage
                .get("outputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        });
    }
    out.push(IrStreamEvent::Done);
    Ok(out)
}

fn flush_text(text: &mut String, content: &mut Vec<Value>) {
    if !text.is_empty() {
        content.push(json!({ "text": text.as_str() }));
        text.clear();
    }
}

fn tool_use(id: &str, name: &str, args: &str) -> Value {
    let input: Value = serde_json::from_str(args).unwrap_or_else(|_| json!(args));
    json!({
        "toolUse": {
            "toolUseId": id,
            "name": name,
            "input": input
        }
    })
}

fn finish_reason(reason: &str) -> &'static str {
    match reason {
        "tool_use" | "tool-use" | "tool_calls" => "tool_use",
        "max_tokens" | "length" => "max_tokens",
        "content_filtered" => "content_filtered",
        "stop_sequence" => "stop_sequence",
        "guardrail_intervened" => "guardrail_intervened",
        _ => "end_turn",
    }
}

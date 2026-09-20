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
                index: 0,
            }));
        }
        if let Some(citation) = delta.get("citation")
            && let Some(annotation) = annotation_from_converse_citation(citation)
        {
            return Ok(Some(IrStreamEvent::AnnotationAdded { annotation }));
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
            index: 0,
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
        return Ok(Some(usage_from_converse(usage)));
    }
    if let Some(tier) = service_tier_from(value) {
        return Ok(Some(IrStreamEvent::ServiceTier { tier }));
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
        IrStreamEvent::TextDelta { text }
        | IrStreamEvent::RefusalDelta { text }
        | IrStreamEvent::AudioTranscriptDelta { text } => Ok(json!({
            "contentBlockDelta": { "delta": { "text": text } }
        })),
        IrStreamEvent::ReasoningDelta { text } => Ok(json!({
            "contentBlockDelta": { "delta": { "reasoningContent": { "text": text } } }
        })),
        IrStreamEvent::ReasoningSignature { signature } => Ok(json!({
            "contentBlockDelta": { "delta": { "reasoningContent": { "signature": signature } } }
        })),
        IrStreamEvent::ToolCallStart { id, name, .. } => Ok(json!({
            "contentBlockStart": {
                "start": { "toolUse": { "toolUseId": id, "name": name } }
            }
        })),
        IrStreamEvent::CustomToolCallStart { id, name, .. } => Ok(json!({
            "contentBlockStart": {
                "start": { "toolUse": { "toolUseId": id, "name": name } }
            }
        })),
        IrStreamEvent::AnnotationAdded { annotation } => Ok(json!({
            "contentBlockDelta": { "delta": { "citation": citation_from_annotation(annotation) } }
        })),
        IrStreamEvent::AudioDelta { .. }
        | IrStreamEvent::Logprobs { .. }
        | IrStreamEvent::Created { .. }
        | IrStreamEvent::Metadata { .. }
        | IrStreamEvent::Moderation { .. } => Ok(json!({
            "contentBlockDelta": { "delta": { "text": "" } }
        })),
        IrStreamEvent::ServiceTier { tier } => {
            let mut metadata = serde_json::Map::new();
            if let Some((mapped, _)) = crate::map::converse_service_tier(tier) {
                metadata.insert("serviceTier".into(), json!({ "type": mapped }));
            }
            Ok(json!({ "metadata": metadata }))
        }
        IrStreamEvent::ToolCallArgDelta { delta, .. }
        | IrStreamEvent::CustomToolCallInputDelta { delta, .. } => Ok(json!({
            "contentBlockDelta": { "delta": { "toolUse": { "input": delta } } }
        })),
        IrStreamEvent::ToolCallEnd => Ok(json!({ "contentBlockStop": {} })),
        IrStreamEvent::FinishReason { reason } => Ok(json!({
            "messageStop": { "stopReason": finish_reason(reason) }
        })),
        IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            ..
        } => Ok(json!({
            "metadata": {
                "usage": encode_converse_usage(
                    *prompt_tokens,
                    *completion_tokens,
                    *cache_read_tokens,
                    *cache_write_tokens,
                )
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
    let mut citations = Vec::new();
    let mut stop = "end_turn";
    let mut usage = None;
    let mut service_tier = None;
    for ev in events {
        match ev {
            IrStreamEvent::TextDelta { text: delta }
            | IrStreamEvent::AudioTranscriptDelta { text: delta } => text.push_str(delta),
            IrStreamEvent::AnnotationAdded { annotation } => {
                citations.push(citation_from_annotation(annotation));
            }
            IrStreamEvent::ToolCallStart { id, name, .. } => {
                flush_text(&mut text, &mut content);
                if let Some((id, name, args)) = current_tool.take() {
                    content.push(tool_use(&id, &name, &args));
                }
                current_tool = Some((id.clone(), name.clone(), String::new()));
            }
            IrStreamEvent::ToolCallArgDelta { delta, .. } => {
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
            IrStreamEvent::ServiceTier { tier } => {
                if let Some((mapped, _)) = crate::map::converse_service_tier(tier) {
                    service_tier = Some(mapped);
                }
            }
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                ..
            } => {
                usage = Some((
                    *prompt_tokens,
                    *completion_tokens,
                    *cache_read_tokens,
                    *cache_write_tokens,
                ));
            }
            _ => {}
        }
    }
    if !citations.is_empty() {
        let mut generated = Vec::new();
        if !text.is_empty() {
            generated.push(json!({ "text": text.as_str() }));
            text.clear();
        }
        content.push(json!({
            "citationsContent": {
                "content": generated,
                "citations": citations
            }
        }));
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
    if let Some(tier) = service_tier {
        body["serviceTier"] = json!({ "type": tier });
    }
    if let Some((input, output, cache_read, cache_write)) = usage {
        body["usage"] = encode_converse_usage(input, output, cache_read, cache_write);
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
        if let Some(citations) = block
            .pointer("/citationsContent/citations")
            .and_then(Value::as_array)
        {
            for citation in citations {
                if let Some(annotation) = annotation_from_converse_citation(citation) {
                    out.push(IrStreamEvent::AnnotationAdded { annotation });
                }
            }
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
                index: 0,
            });
            out.push(IrStreamEvent::ToolCallArgDelta {
                delta: args,
                index: 0,
            });
            out.push(IrStreamEvent::ToolCallEnd);
        }
    }
    if let Some(reason) = value.get("stopReason").and_then(Value::as_str) {
        out.push(IrStreamEvent::FinishReason {
            reason: reason.to_string(),
        });
    }
    if let Some(tier) = service_tier_from(value) {
        out.push(IrStreamEvent::ServiceTier { tier });
    }
    if let Some(usage) = value.get("usage") {
        out.push(usage_from_converse(usage));
    }
    out.push(IrStreamEvent::Done);
    Ok(out)
}

pub(super) fn annotation_from_converse_citation(citation: &Value) -> Option<Value> {
    let url = citation
        .pointer("/location/web/url")
        .or_else(|| citation.get("source"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let mut out = json!({ "type": "url_citation", "url": url });
    if let Some(title) = citation
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out["title"] = json!(title);
    }
    Some(out)
}

pub(super) fn citation_from_annotation(annotation: &Value) -> Value {
    let url = super::chat::annotation_url(annotation).unwrap_or("");
    let mut citation = json!({
        "source": url,
        "location": { "web": { "url": url } }
    });
    if let Some(title) = super::chat::annotation_title(annotation) {
        citation["title"] = json!(title);
    }
    citation
}

pub(super) fn decode_metadata_events(value: &Value) -> Vec<IrStreamEvent> {
    let mut out = Vec::new();
    if let Some(tier) = service_tier_from(value) {
        out.push(IrStreamEvent::ServiceTier { tier });
    }
    if let Some(usage) = value.pointer("/metadata/usage") {
        out.push(usage_from_converse(usage));
    }
    out
}

fn service_tier_from(value: &Value) -> Option<String> {
    value
        .pointer("/metadata/serviceTier/type")
        .or_else(|| value.pointer("/serviceTier/type"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn usage_from_converse(usage: &Value) -> IrStreamEvent {
    IrStreamEvent::Usage {
        prompt_tokens: usage_u32(usage, "inputTokens"),
        completion_tokens: usage_u32(usage, "outputTokens"),
        cache_read_tokens: usage_u32(usage, "cacheReadInputTokens"),
        cache_write_tokens: usage_u32(usage, "cacheWriteInputTokens"),
        reasoning_tokens: 0,
    }
}

fn encode_converse_usage(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
) -> Value {
    let mut usage = json!({
        "inputTokens": prompt_tokens,
        "outputTokens": completion_tokens,
    });
    if cache_read_tokens > 0 {
        usage["cacheReadInputTokens"] = json!(cache_read_tokens);
    }
    if cache_write_tokens > 0 {
        usage["cacheWriteInputTokens"] = json!(cache_write_tokens);
    }
    usage
}

fn usage_u32(usage: &Value, key: &str) -> u32 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0) as u32
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
        "content_filtered" | "content_filter" | "content-filter" => "content_filtered",
        "stop_sequence" => "stop_sequence",
        "guardrail_intervened" => "guardrail_intervened",
        _ => "end_turn",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converse_encode_finish_maps_chat_stop_and_tool_calls() {
        let stop = encode(&IrStreamEvent::FinishReason {
            reason: "stop".into(),
        })
        .expect("encode stop");
        assert_eq!(
            stop.pointer("/messageStop/stopReason")
                .and_then(Value::as_str),
            Some("end_turn"),
            "Chat stop must become AWS end_turn, got {stop}"
        );
        let tools = encode(&IrStreamEvent::FinishReason {
            reason: "tool_calls".into(),
        })
        .expect("encode tool_calls");
        assert_eq!(
            tools
                .pointer("/messageStop/stopReason")
                .and_then(Value::as_str),
            Some("tool_use"),
            "Chat tool_calls must become AWS tool_use, got {tools}"
        );
        let done = encode(&IrStreamEvent::Done).expect("encode done");
        assert_eq!(
            done.pointer("/messageStop/stopReason")
                .and_then(Value::as_str),
            Some("end_turn"),
            "Done stays end_turn, got {done}"
        );
    }

    #[test]
    fn converse_encode_finish_maps_content_filter() {
        let stop = encode(&IrStreamEvent::FinishReason {
            reason: "content_filter".into(),
        })
        .expect("encode content_filter");
        assert_eq!(
            stop.pointer("/messageStop/stopReason")
                .and_then(Value::as_str),
            Some("content_filtered"),
            "IR content_filter must become AWS content_filtered, got {stop}"
        );
        let complete = encode_complete(&[IrStreamEvent::FinishReason {
            reason: "content_filter".into(),
        }]);
        assert_eq!(
            complete.get("stopReason").and_then(Value::as_str),
            Some("content_filtered"),
            "complete JSON must map content_filter, got {complete}"
        );
    }

    #[test]
    fn converse_encode_reasoning_signature() {
        let value = encode(&IrStreamEvent::ReasoningSignature {
            signature: "sig-1".into(),
        })
        .expect("encode signature");
        assert_eq!(
            value
                .pointer("/contentBlockDelta/delta/reasoningContent/signature")
                .and_then(Value::as_str),
            Some("sig-1"),
            "dest Converse must emit reasoningContent.signature, got {value}"
        );
    }
}

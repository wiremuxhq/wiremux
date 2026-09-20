//! Complete JSON bodies (non-SSE). Chat stream maps stay delta-only.

use serde_json::{Value, json};
use wiremux_auth::{ResolvedProfile, Wire};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

use super::usage::{from_anthropic, from_chat};
use super::{MAX_TOOL_CALL_INDEX, RawSse, check_index, decode_stream_events, str_field};

/// Encode IR stream events as a complete (non-SSE) client body.
pub fn encode_response(wire: Wire, events: &[IrStreamEvent]) -> Result<Value, MapError> {
    encode_response_with_model(wire, events, "")
}

/// Same as [`encode_response`], with dest `model` for Chat, Messages, Gemini, and Responses.
pub fn encode_response_with_model(
    wire: Wire,
    events: &[IrStreamEvent],
    model: &str,
) -> Result<Value, MapError> {
    match wire {
        Wire::ChatCompletions => Ok(encode_chat_complete(events, model)),
        Wire::Messages => Ok(encode_messages_complete(events, model)),
        Wire::Gemini => Ok(encode_gemini_complete(events, model)),
        Wire::Responses => Ok(encode_responses_complete(events, model)),
        Wire::Converse => Ok(super::converse::encode_complete(events)),
        _ => Err(MapError::Invalid(format!(
            "unsupported wire `{}`",
            wire.as_str()
        ))),
    }
}

fn encode_chat_complete(events: &[IrStreamEvent], model: &str) -> Value {
    let mut text = String::new();
    let mut refusal = String::new();
    let mut reasoning = String::new();
    let mut reasoning_signature = None;
    let mut finish = None;
    let mut usage = None;
    let mut annotations = Vec::new();
    let mut audio_data = String::new();
    let mut audio_transcript = String::new();
    let mut tool_calls = Vec::new();
    let mut logprobs_content = Vec::new();
    let mut created = None;
    let mut service_tier = None;
    let mut current: Option<(String, String, String, bool)> = None;
    for ev in events {
        match ev {
            IrStreamEvent::TextDelta { text: delta } => text.push_str(delta),
            IrStreamEvent::RefusalDelta { text: delta } => refusal.push_str(delta),
            IrStreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(delta),
            IrStreamEvent::ReasoningSignature { signature } => {
                reasoning_signature = Some(signature.clone());
            }
            IrStreamEvent::AnnotationAdded { annotation } => {
                annotations.push(super::chat::annotation_to_chat(annotation));
            }
            IrStreamEvent::AudioDelta { data } => audio_data.push_str(data),
            IrStreamEvent::AudioTranscriptDelta { text } => audio_transcript.push_str(text),
            IrStreamEvent::Logprobs { content } => {
                extend_logprobs_content(&mut logprobs_content, content);
            }
            IrStreamEvent::Created { unix } => created = Some(*unix),
            IrStreamEvent::ServiceTier { tier } => service_tier = Some(tier.clone()),
            IrStreamEvent::FinishReason { reason } => {
                finish = Some(super::chat::encode_finish(reason).to_string());
            }
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
            } => {
                usage = Some((
                    *prompt_tokens,
                    *completion_tokens,
                    *cache_read_tokens,
                    *cache_write_tokens,
                    *reasoning_tokens,
                ));
            }
            IrStreamEvent::ToolCallStart { id, name, .. } => {
                if let Some((id, name, args, custom)) = current.take() {
                    tool_calls.push(chat_tool_call_value(&id, &name, &args, custom));
                }
                current = Some((id.clone(), name.clone(), String::new(), false));
            }
            IrStreamEvent::CustomToolCallStart { id, name, .. } => {
                if let Some((id, name, args, custom)) = current.take() {
                    tool_calls.push(chat_tool_call_value(&id, &name, &args, custom));
                }
                current = Some((id.clone(), name.clone(), String::new(), true));
            }
            IrStreamEvent::ToolCallArgDelta { delta, .. }
            | IrStreamEvent::CustomToolCallInputDelta { delta, .. } => {
                if let Some((_, _, args, _)) = current.as_mut() {
                    args.push_str(delta);
                }
            }
            IrStreamEvent::ToolCallEnd => {
                if let Some((id, name, args, custom)) = current.take() {
                    tool_calls.push(chat_tool_call_value(&id, &name, &args, custom));
                }
            }
            _ => {}
        }
    }
    if let Some((id, name, args, custom)) = current.take() {
        tool_calls.push(chat_tool_call_value(&id, &name, &args, custom));
    }

    let mut message = json!({ "role": "assistant" });
    if tool_calls.is_empty() {
        message["content"] = if text.is_empty() && !refusal.is_empty() {
            Value::Null
        } else {
            json!(text)
        };
    } else {
        message["content"] = if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        };
        message["tool_calls"] = Value::Array(tool_calls);
    }
    if !refusal.is_empty() {
        message["refusal"] = json!(refusal);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if let Some(signature) = reasoning_signature {
        message["reasoning_signature"] = json!(signature);
    }
    if !annotations.is_empty() {
        message["annotations"] = Value::Array(annotations);
    }
    if !audio_data.is_empty() || !audio_transcript.is_empty() {
        let mut audio = serde_json::Map::new();
        if !audio_data.is_empty() {
            audio.insert("data".into(), json!(audio_data));
        }
        if !audio_transcript.is_empty() {
            audio.insert("transcript".into(), json!(audio_transcript));
        }
        message["audio"] = Value::Object(audio);
    }

    let mut choice = json!({
        "index": 0,
        "message": message,
    });
    if !logprobs_content.is_empty() {
        choice["logprobs"] = json!({ "content": logprobs_content });
    }
    if let Some(reason) = finish {
        choice["finish_reason"] = json!(reason);
    }

    let mut out = json!({
        "id": "chatcmpl-wiremux",
        "object": "chat.completion",
        "choices": [choice],
    });
    if !model.is_empty() {
        out["model"] = json!(model);
    }
    if let Some(unix) = created {
        out["created"] = json!(unix);
    }
    if let Some(tier) = service_tier {
        out["service_tier"] = json!(tier);
    }
    if let Some((prompt, completion, cache_read, cache_write, reasoning_tokens)) = usage {
        let encoded = super::usage::encode_chat(
            prompt,
            completion,
            cache_read,
            cache_write,
            reasoning_tokens,
        );
        if let Some(u) = encoded.get("usage") {
            out["usage"] = u.clone();
        }
    }
    out
}

fn extend_logprobs_content(dst: &mut Vec<Value>, content: &Value) {
    match content {
        Value::Array(arr) => dst.extend(arr.iter().cloned()),
        other if !other.is_null() => dst.push(other.clone()),
        _ => {}
    }
}

fn chat_tool_call_value(id: &str, name: &str, args: &str, custom: bool) -> Value {
    if custom {
        json!({
            "id": id,
            "type": "custom",
            "custom": { "name": name, "input": args },
        })
    } else {
        json!({
            "id": id,
            "type": "function",
            "function": { "name": name, "arguments": args },
        })
    }
}

fn encode_messages_complete(events: &[IrStreamEvent], model: &str) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut reasoning_signature = None;
    let mut finish = None;
    let mut usage = None;
    let mut tool_calls = Vec::new();
    let mut citations = Vec::new();
    let mut current: Option<(String, String, String)> = None;
    for ev in events {
        match ev {
            IrStreamEvent::TextDelta { text: delta }
            | IrStreamEvent::AudioTranscriptDelta { text: delta } => text.push_str(delta),
            IrStreamEvent::AnnotationAdded { annotation } => {
                citations.push(super::messages::citation_from_annotation(annotation));
            }
            IrStreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(delta),
            IrStreamEvent::ReasoningSignature { signature } => {
                reasoning_signature = Some(signature.clone());
            }
            IrStreamEvent::FinishReason { reason } => {
                finish = Some(super::messages::encode_stop_reason(reason).to_string());
            }
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
            } => {
                usage = Some((
                    *prompt_tokens,
                    *completion_tokens,
                    *cache_read_tokens,
                    *cache_write_tokens,
                    *reasoning_tokens,
                ));
            }
            IrStreamEvent::ToolCallStart { id, name, .. } => {
                if let Some((id, name, args)) = current.take() {
                    tool_calls.push(messages_tool_use_value(&id, &name, &args));
                }
                current = Some((id.clone(), name.clone(), String::new()));
            }
            IrStreamEvent::ToolCallArgDelta { delta, .. } => {
                if let Some((_, _, args)) = current.as_mut() {
                    args.push_str(delta);
                }
            }
            IrStreamEvent::ToolCallEnd => {
                if let Some((id, name, args)) = current.take() {
                    tool_calls.push(messages_tool_use_value(&id, &name, &args));
                }
            }
            _ => {}
        }
    }
    if let Some((id, name, args)) = current.take() {
        tool_calls.push(messages_tool_use_value(&id, &name, &args));
    }

    let mut content = Vec::new();
    if !reasoning.is_empty() || reasoning_signature.is_some() {
        let mut block = json!({ "type": "thinking", "thinking": reasoning });
        if let Some(signature) = reasoning_signature {
            block["signature"] = json!(signature);
        }
        content.push(block);
    }
    if !text.is_empty() || !citations.is_empty() {
        let mut block = json!({ "type": "text", "text": text });
        if !citations.is_empty() {
            block["citations"] = json!(citations);
        }
        content.push(block);
    }
    content.extend(tool_calls);

    let mut out = json!({
        "id": "msg_wiremux",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
    });
    if let Some(reason) = finish {
        out["stop_reason"] = json!(reason);
    }
    if let Some((prompt, completion, cache_read, cache_write, reasoning_tokens)) = usage {
        let encoded = super::usage::encode_anthropic(
            prompt,
            completion,
            cache_read,
            cache_write,
            reasoning_tokens,
        );
        if let Some(u) = encoded.get("usage") {
            out["usage"] = u.clone();
        }
    }
    out
}

fn messages_tool_use_value(id: &str, name: &str, args: &str) -> Value {
    json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
    })
}

fn encode_gemini_complete(events: &[IrStreamEvent], model: &str) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut reasoning_signature = None;
    let mut finish = None;
    let mut usage = None;
    let mut tool_calls = Vec::new();
    let mut grounding_chunks = Vec::new();
    let mut grounding_supports = Vec::new();
    let mut audio_parts = Vec::new();
    let mut logprobs_content = Vec::new();
    let mut current: Option<(String, String, String)> = None;
    for ev in events {
        match ev {
            IrStreamEvent::TextDelta { text: delta }
            | IrStreamEvent::AudioTranscriptDelta { text: delta } => text.push_str(delta),
            IrStreamEvent::AnnotationAdded { annotation } => {
                let idx = grounding_chunks.len();
                grounding_chunks.push(super::gemini::grounding_chunk_from_annotation(annotation));
                grounding_supports.push(super::gemini::grounding_support_from_annotation(
                    annotation, idx,
                ));
            }
            IrStreamEvent::AudioDelta { data } => {
                audio_parts.push(json!({
                    "inlineData": { "mimeType": "audio/mpeg", "data": data }
                }));
            }
            IrStreamEvent::Logprobs { content } => {
                extend_logprobs_content(&mut logprobs_content, content);
            }
            IrStreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(delta),
            IrStreamEvent::ReasoningSignature { signature } => {
                reasoning_signature = Some(signature.clone());
            }
            IrStreamEvent::FinishReason { reason } => {
                finish = Some(super::gemini::encode_finish(reason).to_string());
            }
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                reasoning_tokens,
                ..
            } => {
                usage = Some((
                    *prompt_tokens,
                    *completion_tokens,
                    *cache_read_tokens,
                    *reasoning_tokens,
                ));
            }
            IrStreamEvent::ToolCallStart { id, name, .. } => {
                if let Some((id, name, args)) = current.take() {
                    tool_calls.push(gemini_function_call_value(&id, &name, &args));
                }
                current = Some((id.clone(), name.clone(), String::new()));
            }
            IrStreamEvent::ToolCallArgDelta { delta, .. } => {
                if let Some((_, _, args)) = current.as_mut() {
                    args.push_str(delta);
                }
            }
            IrStreamEvent::ToolCallEnd => {
                if let Some((id, name, args)) = current.take() {
                    tool_calls.push(gemini_function_call_value(&id, &name, &args));
                }
            }
            _ => {}
        }
    }
    if let Some((id, name, args)) = current.take() {
        tool_calls.push(gemini_function_call_value(&id, &name, &args));
    }

    let mut parts = Vec::new();
    if !reasoning.is_empty() || reasoning_signature.is_some() {
        let mut part = json!({ "text": reasoning, "thought": true });
        if let Some(signature) = reasoning_signature {
            part["thoughtSignature"] = json!(signature);
        }
        parts.push(part);
    }
    if !text.is_empty() {
        parts.push(json!({ "text": text }));
    }
    parts.extend(audio_parts);
    parts.extend(tool_calls);

    let mut candidate = json!({
        "content": {
            "role": "model",
            "parts": parts,
        }
    });
    if let Some(reason) = finish {
        candidate["finishReason"] = json!(reason);
    }
    if !grounding_chunks.is_empty() {
        candidate["groundingMetadata"] = json!({
            "groundingChunks": grounding_chunks,
            "groundingSupports": grounding_supports,
        });
    }
    if !logprobs_content.is_empty() {
        candidate["logprobsResult"] =
            super::gemini::logprobs_to_result(&Value::Array(logprobs_content));
    }

    let mut out = json!({
        "candidates": [candidate],
        "responseId": "gemini-wiremux",
    });
    if !model.is_empty() {
        out["modelVersion"] = json!(model);
    }
    if let Some((prompt, completion, cache_read, reasoning_tokens)) = usage {
        let encoded = super::usage::encode_gemini(prompt, completion, cache_read, reasoning_tokens);
        if let Some(meta) = encoded.get("usageMetadata") {
            out["usageMetadata"] = meta.clone();
        }
    }
    out
}

fn gemini_function_call_value(id: &str, name: &str, args: &str) -> Value {
    let n = if name.is_empty() { id } else { name };
    json!({
        "functionCall": {
            "name": n,
            "args": serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
        }
    })
}

fn encode_responses_complete(events: &[IrStreamEvent], model: &str) -> Value {
    let mut text = String::new();
    let mut refusal = String::new();
    let mut reasoning = String::new();
    let mut reasoning_signature = None;
    let mut finish = None;
    let mut usage = None;
    let mut annotations = Vec::new();
    let mut logprobs_content = Vec::new();
    let mut created = None;
    let mut service_tier = None;
    let mut tool_calls = Vec::new();
    let mut current: Option<(String, String, String, bool)> = None;
    for ev in events {
        match ev {
            IrStreamEvent::TextDelta { text: delta } => text.push_str(delta),
            IrStreamEvent::RefusalDelta { text: delta } => refusal.push_str(delta),
            IrStreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(delta),
            IrStreamEvent::ReasoningSignature { signature } => {
                reasoning_signature = Some(signature.clone());
            }
            IrStreamEvent::AnnotationAdded { annotation } => {
                annotations.push(annotation.clone());
            }
            IrStreamEvent::Logprobs { content } => {
                extend_logprobs_content(&mut logprobs_content, content);
            }
            IrStreamEvent::Created { unix } => created = Some(*unix),
            IrStreamEvent::ServiceTier { tier } => service_tier = Some(tier.clone()),
            IrStreamEvent::FinishReason { reason } => {
                finish = Some(reason.clone());
            }
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
            } => {
                usage = Some((
                    *prompt_tokens,
                    *completion_tokens,
                    *cache_read_tokens,
                    *cache_write_tokens,
                    *reasoning_tokens,
                ));
            }
            IrStreamEvent::ToolCallStart { id, name, .. } => {
                if let Some((id, name, args, custom)) = current.take() {
                    tool_calls.push(responses_tool_call_value(&id, &name, &args, custom));
                }
                current = Some((id.clone(), name.clone(), String::new(), false));
            }
            IrStreamEvent::CustomToolCallStart { id, name, .. } => {
                if let Some((id, name, args, custom)) = current.take() {
                    tool_calls.push(responses_tool_call_value(&id, &name, &args, custom));
                }
                current = Some((id.clone(), name.clone(), String::new(), true));
            }
            IrStreamEvent::ToolCallArgDelta { delta, .. }
            | IrStreamEvent::CustomToolCallInputDelta { delta, .. } => {
                if let Some((_, _, args, _)) = current.as_mut() {
                    args.push_str(delta);
                }
            }
            IrStreamEvent::ToolCallEnd => {
                if let Some((id, name, args, custom)) = current.take() {
                    tool_calls.push(responses_tool_call_value(&id, &name, &args, custom));
                }
            }
            _ => {}
        }
    }
    if let Some((id, name, args, custom)) = current.take() {
        tool_calls.push(responses_tool_call_value(&id, &name, &args, custom));
    }

    let mut output = Vec::new();
    if !reasoning.is_empty() || reasoning_signature.is_some() {
        let mut item = json!({ "type": "reasoning" });
        if !reasoning.is_empty() {
            item["summary"] = json!([{ "type": "summary_text", "text": reasoning }]);
        }
        if let Some(signature) = reasoning_signature {
            item["signature"] = json!(signature);
        }
        output.push(item);
    }
    if !text.is_empty()
        || !refusal.is_empty()
        || !annotations.is_empty()
        || !logprobs_content.is_empty()
    {
        let mut content = Vec::new();
        if !text.is_empty() || !annotations.is_empty() || !logprobs_content.is_empty() {
            let mut part = json!({ "type": "output_text", "text": text });
            if !annotations.is_empty() {
                part["annotations"] = json!(annotations);
            }
            if !logprobs_content.is_empty() {
                part["logprobs"] = json!(logprobs_content);
            }
            content.push(part);
        }
        if !refusal.is_empty() {
            content.push(json!({ "type": "refusal", "refusal": refusal }));
        }
        output.push(json!({
            "type": "message",
            "role": "assistant",
            "content": content,
        }));
    }
    output.extend(tool_calls);

    let status = finish
        .as_deref()
        .map(responses_complete_status)
        .unwrap_or("completed");
    let mut out = json!({
        "id": "resp_wiremux",
        "object": "response",
        "status": status,
        "output": output,
    });
    if let Some(detail) = finish
        .as_deref()
        .and_then(super::responses::incomplete_details_reason)
    {
        out["incomplete_details"] = json!({ "reason": detail });
    }
    if !model.is_empty() {
        out["model"] = json!(model);
    }
    if let Some(unix) = created {
        out["created_at"] = json!(unix);
    }
    if let Some(tier) = service_tier {
        out["service_tier"] = json!(tier);
    }
    if !text.is_empty() {
        out["output_text"] = json!(text);
    }
    if let Some((prompt, completion, cache_read, cache_write, reasoning_tokens)) = usage {
        let encoded = super::usage::encode_responses(
            prompt,
            completion,
            cache_read,
            cache_write,
            reasoning_tokens,
        );
        if let Some(u) = encoded.pointer("/response/usage") {
            out["usage"] = u.clone();
        }
    }
    out
}

fn responses_complete_status(reason: &str) -> &str {
    match reason {
        "failed" => "failed",
        "incomplete" | "length" | "max_tokens" | "content_filter" => "incomplete",
        // Chat `tool_calls` is a completed Responses turn with function_call output.
        _ => "completed",
    }
}

fn responses_tool_call_value(id: &str, name: &str, args: &str, custom: bool) -> Value {
    if custom {
        json!({
            "type": "custom_tool_call",
            "id": id,
            "call_id": id,
            "name": name,
            "input": args,
        })
    } else {
        json!({
            "type": "function_call",
            "id": id,
            "call_id": id,
            "name": name,
            "arguments": args,
        })
    }
}

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
        Wire::Converse => super::converse::decode_complete(&value),
        _ => Err(MapError::Invalid(format!(
            "unsupported wire `{}`",
            wire.as_str()
        ))),
    }
}

fn decode_chat_complete(value: &Value) -> Result<Vec<IrStreamEvent>, MapError> {
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
            if let Some(text) = message
                .get("refusal")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.push(IrStreamEvent::RefusalDelta {
                    text: text.to_string(),
                });
            }
            if let Some(anns) = message.get("annotations").and_then(Value::as_array) {
                for ann in anns {
                    if ann.is_object() {
                        out.push(IrStreamEvent::AnnotationAdded {
                            annotation: super::chat::annotation_from_chat(ann),
                        });
                    }
                }
            }
            if let Some(audio) = message.get("audio").filter(|v| v.is_object()) {
                if let Some(data) = str_field(audio, "data").filter(|s| !s.is_empty()) {
                    out.push(IrStreamEvent::AudioDelta { data });
                }
                if let Some(text) = str_field(audio, "transcript").filter(|s| !s.is_empty()) {
                    out.push(IrStreamEvent::AudioTranscriptDelta { text });
                }
            }
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    check_index(call, "index", MAX_TOOL_CALL_INDEX, "tool call")?;
                    out.extend(complete_chat_tool_call(call));
                }
            } else if let Some(fc) = message.get("function_call").filter(|v| v.is_object()) {
                out.extend(complete_chat_tool_call(fc));
            }
        }
        if let Some(content) = super::chat::logprobs_content(choice) {
            out.push(IrStreamEvent::Logprobs { content });
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
        && ty == "custom"
    {
        let custom = call.get("custom").unwrap_or(call);
        let id = str_field(call, "id").unwrap_or_default();
        let name = str_field(custom, "name").unwrap_or_default();
        let input = str_field(custom, "input");
        let index = super::chat::tool_call_index(call);
        let mut out = vec![IrStreamEvent::CustomToolCallStart { id, name, index }];
        if let Some(delta) = input {
            out.push(IrStreamEvent::CustomToolCallInputDelta { delta, index });
        }
        out.push(IrStreamEvent::ToolCallEnd);
        return out;
    }
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
    let index = super::chat::tool_call_index(call);
    let mut out = vec![IrStreamEvent::ToolCallStart {
        id,
        name,
        thought_signature: None,
        index,
    }];
    if let Some(delta) = args {
        out.push(IrStreamEvent::ToolCallArgDelta { delta, index });
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
                    if let Some(citations) = block.get("citations").and_then(Value::as_array) {
                        for citation in citations {
                            if let Some(annotation) =
                                super::messages::annotation_from_messages_citation(citation)
                            {
                                out.push(IrStreamEvent::AnnotationAdded { annotation });
                            }
                        }
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
                        index: 0,
                    });
                    if let Some(input) = block.get("input").filter(|v| !v.is_null()) {
                        out.push(IrStreamEvent::ToolCallArgDelta {
                            delta: input.to_string(),
                            index: 0,
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
        if let Some(tier) = super::messages::service_tier_from_usage(usage) {
            out.push(IrStreamEvent::ServiceTier { tier });
        }
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
    if let Some(unix) = value
        .get("created_at")
        .or_else(|| value.pointer("/response/created_at"))
        .and_then(Value::as_i64)
    {
        events.push(IrStreamEvent::Created { unix });
    }
    if let Some(tier) = value
        .get("service_tier")
        .or_else(|| value.pointer("/response/service_tier"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        events.push(IrStreamEvent::ServiceTier {
            tier: tier.to_string(),
        });
    }
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
                            if let Some(anns) = part.get("annotations").and_then(Value::as_array) {
                                for ann in anns {
                                    if ann.is_object() {
                                        out.push(IrStreamEvent::AnnotationAdded {
                                            annotation: ann.clone(),
                                        });
                                    }
                                }
                            }
                            if let Some(content) = super::responses::logprobs_array(part) {
                                out.push(IrStreamEvent::Logprobs { content });
                            }
                        }
                        Some("refusal") => {
                            if let Some(text) = str_field(part, "refusal")
                                .or_else(|| str_field(part, "text"))
                                .filter(|s| !s.is_empty())
                            {
                                out.push(IrStreamEvent::RefusalDelta { text });
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
                    index: 0,
                });
                if let Some(delta) = str_field(item, "arguments").filter(|s| !s.is_empty()) {
                    out.push(IrStreamEvent::ToolCallArgDelta { delta, index: 0 });
                }
                out.push(IrStreamEvent::ToolCallEnd);
            }
            Some("custom_tool_call") => {
                out.push(IrStreamEvent::CustomToolCallStart {
                    id: str_field(item, "call_id")
                        .or_else(|| str_field(item, "id"))
                        .unwrap_or_default(),
                    name: str_field(item, "name").unwrap_or_default(),
                    index: 0,
                });
                if let Some(delta) = str_field(item, "input").filter(|s| !s.is_empty()) {
                    out.push(IrStreamEvent::CustomToolCallInputDelta { delta, index: 0 });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrStreamEvent;

    #[test]
    fn dest_chat_complete_uses_dest_model() {
        let events = [IrStreamEvent::TextDelta {
            text: "pong".into(),
        }];
        let mapped = encode_response_with_model(Wire::ChatCompletions, &events, "claude-haiku-4-5")
            .expect("encode Chat");
        assert_eq!(
            mapped.get("model").and_then(Value::as_str),
            Some("claude-haiku-4-5"),
            "dest Chat complete must keep dest model, got {mapped}"
        );
    }

    #[test]
    fn dest_chat_complete_url_citation_reaches_messages_citations() {
        let events = [
            IrStreamEvent::TextDelta {
                text: "See https://example.com for more.".into(),
            },
            IrStreamEvent::AnnotationAdded {
                annotation: json!({
                    "type": "url_citation",
                    "url_citation": {
                        "title": "Example Domain",
                        "url": "https://example.com"
                    }
                }),
            },
        ];
        let mapped = encode_response(Wire::Messages, &events).expect("encode dest Messages");
        assert_eq!(
            mapped
                .pointer("/content/0/citations/0/url")
                .and_then(Value::as_str),
            Some("https://example.com"),
            "dest Messages complete encode must write text citations url, got {mapped}"
        );
        assert_eq!(
            mapped
                .pointer("/content/0/citations/0/type")
                .and_then(Value::as_str),
            Some("web_search_result_location"),
            "dest Messages complete citations must be web_search_result_location, got {mapped}"
        );
    }

    #[test]
    fn dest_chat_complete_url_citation_reaches_converse_citations_content() {
        let events = [
            IrStreamEvent::TextDelta {
                text: "See https://example.com for more.".into(),
            },
            IrStreamEvent::AnnotationAdded {
                annotation: json!({
                    "type": "url_citation",
                    "url": "https://example.com",
                    "title": "Example Domain"
                }),
            },
        ];
        let mapped = encode_response(Wire::Converse, &events).expect("encode dest Converse");
        assert_eq!(
            mapped
                .pointer("/output/message/content/0/citationsContent/citations/0/location/web/url")
                .and_then(Value::as_str),
            Some("https://example.com"),
            "dest Converse complete encode must write citationsContent.citations location.web.url, got {mapped}"
        );
    }

    #[test]
    fn dest_chat_complete_url_citation_reaches_gemini_grounding() {
        let events = [IrStreamEvent::AnnotationAdded {
            annotation: json!({
                "type": "url_citation",
                "url": "https://example.com",
                "title": "Example Domain"
            }),
        }];
        let mapped = encode_response(Wire::Gemini, &events).expect("encode dest Gemini");
        assert_eq!(
            mapped
                .pointer("/candidates/0/groundingMetadata/groundingChunks/0/web/uri")
                .and_then(Value::as_str),
            Some("https://example.com"),
            "dest Gemini complete encode must write groundingChunks.web.uri, got {mapped}"
        );
    }

    #[test]
    fn dest_chat_complete_refusal_stays_message_refusal() {
        let events = [IrStreamEvent::RefusalDelta {
            text: "nope".into(),
        }];
        let mapped = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat");
        assert_eq!(
            mapped
                .pointer("/choices/0/message/refusal")
                .and_then(Value::as_str),
            Some("nope"),
            "dest Chat complete encode must write message.refusal, got {mapped}"
        );
        let content = mapped.pointer("/choices/0/message/content");
        assert!(
            content.is_none()
                || content.and_then(Value::as_str).is_none_or(str::is_empty)
                || content.is_some_and(Value::is_null),
            "dest Chat complete refusal must keep content empty or null, got {mapped}"
        );
    }

    #[test]
    fn dest_chat_complete_audio_deltas_concatenate() {
        let events = [
            IrStreamEvent::AudioDelta {
                data: "YQ==".into(),
            },
            IrStreamEvent::AudioDelta {
                data: "Yg==".into(),
            },
            IrStreamEvent::AudioTranscriptDelta { text: "hel".into() },
            IrStreamEvent::AudioTranscriptDelta { text: "lo".into() },
        ];
        let mapped = encode_response(Wire::ChatCompletions, &events).expect("encode dest Chat");
        assert_eq!(
            mapped
                .pointer("/choices/0/message/audio/data")
                .and_then(Value::as_str),
            Some("YQ==Yg=="),
            "dest Chat complete encode must concatenate audio data deltas, got {mapped}"
        );
        assert_eq!(
            mapped
                .pointer("/choices/0/message/audio/transcript")
                .and_then(Value::as_str),
            Some("hello"),
            "dest Chat complete encode must concatenate audio transcript deltas, got {mapped}"
        );
    }

    #[test]
    fn dest_responses_complete_refusal_is_refusal_part() {
        let events = [IrStreamEvent::RefusalDelta {
            text: "nope".into(),
        }];
        let mapped = encode_response(Wire::Responses, &events).expect("encode dest Responses");
        let refusal = mapped.pointer("/output/0/content/0");
        assert_eq!(
            refusal.and_then(|p| p.get("type")).and_then(Value::as_str),
            Some("refusal"),
            "dest Responses complete encode must emit type refusal, got {mapped}"
        );
        assert_eq!(
            refusal
                .and_then(|p| p.get("refusal"))
                .and_then(Value::as_str),
            Some("nope"),
            "dest Responses complete encode must emit refusal text, got {mapped}"
        );
    }

    #[test]
    fn dest_responses_complete_usage_maps_cache_write_and_total() {
        let events = [IrStreamEvent::Usage {
            prompt_tokens: 80,
            completion_tokens: 12,
            cache_read_tokens: 25,
            cache_write_tokens: 9,
            reasoning_tokens: 3,
        }];
        let mapped = encode_response(Wire::Responses, &events).expect("encode dest Responses");
        assert_eq!(
            mapped
                .pointer("/usage/input_tokens_details/cache_write_tokens")
                .and_then(Value::as_u64),
            Some(9),
            "dest Responses complete usage must emit cache_write_tokens, got {mapped}"
        );
        assert_eq!(
            mapped
                .pointer("/usage/total_tokens")
                .and_then(Value::as_u64),
            Some(120),
            "dest Responses complete usage must emit total_tokens, got {mapped}"
        );
    }
}

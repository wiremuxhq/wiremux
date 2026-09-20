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

    if let Some(ev) = usage_from_chunk(value)
        && value
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
            .is_none_or(|p| p.is_empty())
        && value
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
            .is_none()
    {
        return Ok(Some(ev));
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
        if let Some(ev) = usage_from_chunk(value) {
            return Ok(Some(ev));
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
        let supports = candidate
            .pointer("/groundingMetadata/groundingSupports")
            .and_then(Value::as_array);
        for (idx, chunk) in chunks.iter().enumerate() {
            if let Some(mut annotation) = annotation_from_grounding_chunk(chunk) {
                if let Some(supports) = supports {
                    apply_grounding_support(&mut annotation, idx, supports);
                }
                return Ok(Some(IrStreamEvent::AnnotationAdded { annotation }));
            }
        }
    }

    if let Some(cites) = candidate
        .pointer("/citationMetadata/citations")
        .and_then(Value::as_array)
    {
        for cite in cites {
            if let Some(annotation) = annotation_from_citation(cite) {
                return Ok(Some(IrStreamEvent::AnnotationAdded { annotation }));
            }
        }
    }

    if let Some(attrs) = candidate
        .get("groundingAttributions")
        .and_then(Value::as_array)
    {
        for attr in attrs {
            if let Some(annotation) = annotation_from_grounding_chunk(attr) {
                return Ok(Some(IrStreamEvent::AnnotationAdded { annotation }));
            }
        }
    }

    if let Some(content) = candidate
        .get("logprobsResult")
        .and_then(logprobs_from_result)
    {
        return Ok(Some(IrStreamEvent::Logprobs { content }));
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

    if let Some(ev) = usage_from_chunk(value) {
        return Ok(Some(ev));
    }
    Ok(None)
}

pub(super) fn service_tier_from_chunk(value: &Value) -> Option<IrStreamEvent> {
    let usage = value.get("usageMetadata").filter(|v| v.is_object())?;
    crate::map::gemini_decode_service_tier(usage).map(|tier| IrStreamEvent::ServiceTier { tier })
}

pub(super) fn usage_from_chunk(value: &Value) -> Option<IrStreamEvent> {
    if let Some(usage) = value.get("usageMetadata").filter(|v| v.is_object()) {
        return Some(usage::from_gemini(usage));
    }
    let n = value
        .pointer("/candidates/0/tokenCount")
        .and_then(Value::as_u64)
        .filter(|&n| n > 0)?;
    let completion = u32::try_from(n).unwrap_or(u32::MAX);
    Some(IrStreamEvent::Usage {
        prompt_tokens: 0,
        completion_tokens: completion,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        audio_tokens: 0,
    })
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
                    "groundingChunks": [grounding_chunk_from_annotation(annotation)],
                    "groundingSupports": [grounding_support_from_annotation(annotation, 0)]
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
        IrStreamEvent::Logprobs { content } => json!({
            "candidates": [{ "logprobsResult": logprobs_to_result(content) }]
        }),
        IrStreamEvent::Created { .. }
        | IrStreamEvent::ServiceTier { .. }
        | IrStreamEvent::Metadata { .. }
        | IrStreamEvent::Moderation { .. } => {
            json!({ "candidates": [] })
        }
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
            audio_tokens,
            ..
        } => usage::encode_gemini(
            *prompt_tokens,
            *completion_tokens,
            *cache_read_tokens,
            *reasoning_tokens,
            *audio_tokens,
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

pub(super) fn logprobs_from_result(result: &Value) -> Option<Value> {
    let chosen = result.get("chosenCandidates").and_then(Value::as_array)?;
    if chosen.is_empty() {
        return None;
    }
    let tops = result.get("topCandidates").and_then(Value::as_array);
    let mut content = Vec::with_capacity(chosen.len());
    for (i, cand) in chosen.iter().enumerate() {
        let mut item = chat_token_from_gemini(cand);
        if let Some(top) = tops.and_then(|t| t.get(i)) {
            let alts = top
                .get("candidates")
                .and_then(Value::as_array)
                .map(|arr| Value::Array(arr.iter().map(chat_token_from_gemini).collect()))
                .unwrap_or_else(|| json!([]));
            item["top_logprobs"] = alts;
        }
        content.push(item);
    }
    Some(Value::Array(content))
}

pub(super) fn logprobs_to_result(content: &Value) -> Value {
    let mut chosen = Vec::new();
    let mut tops = Vec::new();
    if let Some(arr) = content.as_array() {
        for item in arr {
            chosen.push(gemini_token_from_chat(item));
            let candidates = item
                .get("top_logprobs")
                .and_then(Value::as_array)
                .map(|alts| alts.iter().map(gemini_token_from_chat).collect::<Vec<_>>())
                .unwrap_or_default();
            tops.push(json!({ "candidates": candidates }));
        }
    }
    json!({
        "chosenCandidates": chosen,
        "topCandidates": tops,
    })
}

fn chat_token_from_gemini(cand: &Value) -> Value {
    let token = cand.get("token").and_then(Value::as_str).unwrap_or("");
    let mut item = json!({
        "token": token,
        "bytes": token.as_bytes(),
    });
    if let Some(lp) = cand.get("logProbability") {
        item["logprob"] = lp.clone();
    }
    item
}

fn gemini_token_from_chat(item: &Value) -> Value {
    let mut cand = json!({});
    if let Some(token) = item.get("token") {
        cand["token"] = token.clone();
    }
    if let Some(lp) = item.get("logprob") {
        cand["logProbability"] = lp.clone();
    }
    cand
}

pub(super) fn annotation_from_citation(cite: &Value) -> Option<Value> {
    let url = cite
        .get("uri")
        .or_else(|| cite.get("url"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let mut out = json!({ "type": "url_citation", "url": url });
    if let Some(title) = cite
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out["title"] = json!(title);
    }
    apply_citation_span(&mut out, cite.get("startIndex"), cite.get("endIndex"));
    Some(out)
}

fn apply_citation_span(out: &mut Value, start: Option<&Value>, end: Option<&Value>) {
    if let Some(start) = start.filter(|v| v.is_number()) {
        out["start_index"] = start.clone();
    }
    if let Some(end) = end.filter(|v| v.is_number()) {
        out["end_index"] = end.clone();
    }
}

pub(super) fn apply_grounding_support(
    annotation: &mut Value,
    chunk_index: usize,
    supports: &[Value],
) {
    let idx = u64::try_from(chunk_index).ok();
    for support in supports {
        let matches = support
            .get("groundingChunkIndices")
            .and_then(Value::as_array)
            .is_some_and(|indices| {
                indices
                    .iter()
                    .any(|v| idx.is_some_and(|i| v.as_u64() == Some(i)))
            });
        if !matches {
            continue;
        }
        apply_citation_span(
            annotation,
            support.pointer("/segment/startIndex"),
            support.pointer("/segment/endIndex"),
        );
        break;
    }
}

pub(super) fn annotation_from_grounding_chunk(chunk: &Value) -> Option<Value> {
    let url = chunk
        .pointer("/web/uri")
        .or_else(|| chunk.pointer("/web/url"))
        .or_else(|| chunk.pointer("/image/sourceUri"))
        .or_else(|| chunk.pointer("/retrievedContext/uri"))
        .or_else(|| chunk.pointer("/maps/uri"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let mut out = json!({ "type": "url_citation", "url": url });
    if let Some(title) = chunk
        .pointer("/web/title")
        .or_else(|| chunk.pointer("/image/title"))
        .or_else(|| chunk.pointer("/retrievedContext/title"))
        .or_else(|| chunk.pointer("/maps/title"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out["title"] = json!(title);
    }
    Some(out)
}

pub(super) fn grounding_support_from_annotation(annotation: &Value, chunk_index: usize) -> Value {
    json!({
        "segment": {
            "startIndex": annotation.get("start_index").cloned().unwrap_or(json!(0)),
            "endIndex": annotation.get("end_index").cloned().unwrap_or(json!(0))
        },
        "groundingChunkIndices": [chunk_index]
    })
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

//! Usage field mapping. Zero cache and audio counts omit those keys on encode.
//! Messages response `usage.inference_geo` round-trips. Other wires omit it.

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;

use super::u32_field;

pub(super) fn chat_prediction_token_events(usage: &Value) -> Vec<IrStreamEvent> {
    let mut out = Vec::new();
    for (key, item_type) in [
        (
            "accepted_prediction_tokens",
            "chat_accepted_prediction_tokens",
        ),
        (
            "rejected_prediction_tokens",
            "chat_rejected_prediction_tokens",
        ),
    ] {
        if let Some(count) =
            nested_u32(usage, "completion_tokens_details", key).filter(|count| *count > 0)
        {
            out.push(IrStreamEvent::Protocol {
                item_type: item_type.into(),
                payload: json!(count),
            });
        }
    }
    out
}

pub(super) fn insert_chat_prediction_tokens(
    usage: &mut Value,
    accepted: Option<u32>,
    rejected: Option<u32>,
) {
    if accepted.is_none() && rejected.is_none() {
        return;
    }
    let Some(obj) = usage.as_object_mut() else {
        return;
    };
    let details = obj
        .entry("completion_tokens_details")
        .or_insert_with(|| json!({}));
    let Some(details) = details.as_object_mut() else {
        return;
    };
    if let Some(count) = accepted {
        details.insert("accepted_prediction_tokens".into(), json!(count));
    }
    if let Some(count) = rejected {
        details.insert("rejected_prediction_tokens".into(), json!(count));
    }
}

pub(crate) fn from_chat(usage: &Value) -> IrStreamEvent {
    let cache_read = nested_u32(usage, "prompt_tokens_details", "cached_tokens")
        .filter(|&n| n > 0)
        .or_else(|| u32_field(usage, "cached_tokens").filter(|&n| n > 0))
        .or_else(|| u32_field(usage, "prompt_cache_hit_tokens"))
        .unwrap_or(0);
    let cache_write = nested_u32(usage, "prompt_tokens_details", "cache_write_tokens").unwrap_or(0);
    let reasoning = nested_u32(usage, "completion_tokens_details", "reasoning_tokens").unwrap_or(0);
    let audio_tokens = nested_u32(usage, "prompt_tokens_details", "audio_tokens").unwrap_or(0);
    let completion_audio_tokens =
        nested_u32(usage, "completion_tokens_details", "audio_tokens").unwrap_or(0);
    IrStreamEvent::Usage {
        prompt_tokens: u32_field(usage, "prompt_tokens")
            .unwrap_or(0)
            .saturating_sub(cache_read),
        completion_tokens: u32_field(usage, "completion_tokens")
            .unwrap_or(0)
            .saturating_sub(reasoning),
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens: reasoning,
        audio_tokens,
        completion_audio_tokens,
        inference_geo: None,
    }
}

pub(super) fn messages_web_search_events(usage: &Value) -> Vec<IrStreamEvent> {
    let mut out = Vec::new();
    for (key, item_type) in [
        ("web_search_requests", "messages_web_search_requests"),
        ("web_fetch_requests", "messages_web_fetch_requests"),
    ] {
        let Some(count) = usage
            .get("server_tool_use")
            .and_then(|tools| tools.get(key))
            .and_then(Value::as_u64)
            .filter(|count| *count > 0)
            .and_then(|count| u32::try_from(count).ok())
        else {
            continue;
        };
        out.push(IrStreamEvent::Protocol {
            item_type: item_type.into(),
            payload: json!(count),
        });
    }
    out
}

pub(super) fn insert_messages_server_tool_counts(
    usage: &mut Value,
    search: Option<u32>,
    fetch: Option<u32>,
) {
    if search.is_none() && fetch.is_none() {
        return;
    }
    let mut tools = serde_json::Map::new();
    if let Some(count) = search {
        tools.insert("web_search_requests".into(), json!(count));
    }
    if let Some(count) = fetch {
        tools.insert("web_fetch_requests".into(), json!(count));
    }
    let Some(obj) = usage.as_object_mut() else {
        return;
    };
    obj.insert("server_tool_use".into(), Value::Object(tools));
}

pub(super) fn messages_cache_creation_events(usage: &Value) -> Vec<IrStreamEvent> {
    let Some(creation) = usage
        .get("cache_creation")
        .filter(|value| value.is_object())
    else {
        return Vec::new();
    };
    let mut kept = serde_json::Map::new();
    for key in ["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"] {
        if let Some(count) = creation
            .get(key)
            .and_then(Value::as_u64)
            .filter(|count| *count > 0)
        {
            kept.insert(key.into(), json!(count));
        }
    }
    if kept.is_empty() {
        return Vec::new();
    }
    vec![IrStreamEvent::Protocol {
        item_type: "messages_cache_creation".into(),
        payload: Value::Object(kept),
    }]
}

pub(super) fn insert_messages_cache_creation(usage: &mut Value, creation: Option<&Value>) {
    let Some(creation) = creation.filter(|value| value.is_object()) else {
        return;
    };
    let Some(obj) = usage.as_object_mut() else {
        return;
    };
    obj.insert("cache_creation".into(), creation.clone());
}

pub(super) fn from_anthropic(usage: &Value) -> IrStreamEvent {
    let reasoning = nested_u32(usage, "output_tokens_details", "thinking_tokens").unwrap_or(0);
    IrStreamEvent::Usage {
        prompt_tokens: u32_field(usage, "input_tokens").unwrap_or(0),
        completion_tokens: u32_field(usage, "output_tokens")
            .unwrap_or(0)
            .saturating_sub(reasoning),
        cache_read_tokens: u32_field(usage, "cache_read_input_tokens").unwrap_or(0),
        cache_write_tokens: u32_field(usage, "cache_creation_input_tokens").unwrap_or(0),
        reasoning_tokens: reasoning,
        audio_tokens: 0,
        completion_audio_tokens: 0,
        inference_geo: usage
            .get("inference_geo")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|geo| !geo.is_empty())
            .map(str::to_string),
    }
}

pub(super) fn from_responses(usage: &Value) -> IrStreamEvent {
    let cache_read = nested_u32(usage, "input_tokens_details", "cached_tokens").unwrap_or(0);
    let cache_write = nested_u32(usage, "input_tokens_details", "cache_write_tokens").unwrap_or(0);
    let reasoning = nested_u32(usage, "output_tokens_details", "reasoning_tokens").unwrap_or(0);
    IrStreamEvent::Usage {
        prompt_tokens: u32_field(usage, "input_tokens")
            .unwrap_or(0)
            .saturating_sub(cache_read),
        completion_tokens: u32_field(usage, "output_tokens")
            .unwrap_or(0)
            .saturating_sub(reasoning),
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens: reasoning,
        audio_tokens: 0,
        completion_audio_tokens: 0,
        inference_geo: None,
    }
}

pub(super) fn from_gemini(usage: &Value) -> IrStreamEvent {
    let cache_read = u32_field(usage, "cachedContentTokenCount")
        .or_else(|| u32_field(usage, "cached_content_token_count"))
        .unwrap_or(0);
    let reasoning = u32_field(usage, "thoughtsTokenCount")
        .or_else(|| u32_field(usage, "thoughts_token_count"))
        .unwrap_or(0);
    let prompt = u32_field(usage, "promptTokenCount")
        .or_else(|| u32_field(usage, "prompt_token_count"))
        .unwrap_or(0);
    let completion = u32_field(usage, "candidatesTokenCount")
        .or_else(|| u32_field(usage, "candidates_token_count"))
        .unwrap_or(0);
    IrStreamEvent::Usage {
        prompt_tokens: prompt.saturating_sub(cache_read),
        completion_tokens: completion,
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
        reasoning_tokens: reasoning,
        audio_tokens: gemini_audio_tokens(usage, "promptTokensDetails", "prompt_tokens_details"),
        completion_audio_tokens: gemini_audio_tokens(
            usage,
            "candidatesTokensDetails",
            "candidates_tokens_details",
        ),
        inference_geo: None,
    }
}

pub(super) fn encode_chat(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    reasoning_tokens: u32,
    audio_tokens: u32,
    completion_audio_tokens: u32,
) -> Value {
    let prompt_wire = prompt_tokens.saturating_add(cache_read_tokens);
    let completion_wire = completion_tokens.saturating_add(reasoning_tokens);
    let mut usage = json!({
        "prompt_tokens": prompt_wire,
        "completion_tokens": completion_wire,
        "total_tokens": prompt_wire.saturating_add(completion_wire),
    });
    if cache_read_tokens > 0 || cache_write_tokens > 0 || audio_tokens > 0 {
        let mut details = json!({});
        if cache_read_tokens > 0 {
            details["cached_tokens"] = json!(cache_read_tokens);
        }
        if cache_write_tokens > 0 {
            details["cache_write_tokens"] = json!(cache_write_tokens);
        }
        if audio_tokens > 0 {
            details["audio_tokens"] = json!(audio_tokens);
        }
        usage["prompt_tokens_details"] = details;
    }
    if reasoning_tokens > 0 || completion_audio_tokens > 0 {
        let mut details = json!({});
        if reasoning_tokens > 0 {
            details["reasoning_tokens"] = json!(reasoning_tokens);
        }
        if completion_audio_tokens > 0 {
            details["audio_tokens"] = json!(completion_audio_tokens);
        }
        usage["completion_tokens_details"] = details;
    }
    json!({ "choices": [], "usage": usage })
}

pub(super) fn encode_anthropic(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    reasoning_tokens: u32,
    inference_geo: Option<&str>,
) -> Value {
    let mut usage = json!({
        "input_tokens": prompt_tokens,
        "output_tokens": completion_tokens.saturating_add(reasoning_tokens),
    });
    if cache_read_tokens > 0 {
        usage["cache_read_input_tokens"] = json!(cache_read_tokens);
    }
    if cache_write_tokens > 0 {
        usage["cache_creation_input_tokens"] = json!(cache_write_tokens);
    }
    if reasoning_tokens > 0 {
        usage["output_tokens_details"] = json!({ "thinking_tokens": reasoning_tokens });
    }
    if let Some(geo) = inference_geo {
        usage["inference_geo"] = json!(geo);
    }
    json!({
        "type": "message_delta",
        "delta": {},
        "usage": usage,
    })
}

pub(super) fn encode_responses(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    reasoning_tokens: u32,
) -> Value {
    let input_tokens = prompt_tokens.saturating_add(cache_read_tokens);
    let output_tokens = completion_tokens.saturating_add(reasoning_tokens);
    let mut usage = json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": input_tokens.saturating_add(output_tokens),
    });
    if cache_read_tokens > 0 || cache_write_tokens > 0 {
        let mut details = json!({});
        if cache_read_tokens > 0 {
            details["cached_tokens"] = json!(cache_read_tokens);
        }
        if cache_write_tokens > 0 {
            details["cache_write_tokens"] = json!(cache_write_tokens);
        }
        usage["input_tokens_details"] = details;
    }
    if reasoning_tokens > 0 {
        usage["output_tokens_details"] = json!({ "reasoning_tokens": reasoning_tokens });
    }
    json!({
        "type": "response.completed",
        "response": { "status": "completed", "usage": usage },
    })
}

pub(super) fn gemini_traffic_type_events(value: &Value) -> Vec<IrStreamEvent> {
    let Some(kind) = value
        .pointer("/usageMetadata/trafficType")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
    else {
        return Vec::new();
    };
    vec![IrStreamEvent::Protocol {
        item_type: "gemini_traffic_type".into(),
        payload: json!(kind),
    }]
}

pub(super) fn insert_gemini_traffic_type(usage: &mut Value, kind: Option<&str>) {
    let Some(kind) = kind.filter(|kind| !kind.is_empty()) else {
        return;
    };
    let Some(obj) = usage.as_object_mut() else {
        return;
    };
    obj.insert("trafficType".into(), json!(kind));
}

pub(super) fn encode_gemini(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    reasoning_tokens: u32,
    audio_tokens: u32,
    completion_audio_tokens: u32,
) -> Value {
    let prompt_wire = prompt_tokens.saturating_add(cache_read_tokens);
    let mut usage = json!({
        "promptTokenCount": prompt_wire,
        "candidatesTokenCount": completion_tokens,
        "totalTokenCount": prompt_wire
            .saturating_add(completion_tokens)
            .saturating_add(reasoning_tokens),
    });
    if cache_read_tokens > 0 {
        usage["cachedContentTokenCount"] = json!(cache_read_tokens);
    }
    if reasoning_tokens > 0 {
        usage["thoughtsTokenCount"] = json!(reasoning_tokens);
    }
    if audio_tokens > 0 {
        usage["promptTokensDetails"] = json!([{
            "modality": "AUDIO",
            "tokenCount": audio_tokens
        }]);
    }
    if completion_audio_tokens > 0 {
        usage["candidatesTokensDetails"] = json!([{
            "modality": "AUDIO",
            "tokenCount": completion_audio_tokens
        }]);
    }
    json!({ "usageMetadata": usage })
}

fn gemini_audio_tokens(usage: &Value, camel: &str, snake: &str) -> u32 {
    let details = usage
        .get(camel)
        .or_else(|| usage.get(snake))
        .and_then(Value::as_array);
    let Some(details) = details else {
        return 0;
    };
    let mut total = 0u32;
    for item in details {
        let modality = item.get("modality").and_then(Value::as_str).unwrap_or("");
        if !modality.eq_ignore_ascii_case("AUDIO") {
            continue;
        }
        if let Some(n) = u32_field(item, "tokenCount").or_else(|| u32_field(item, "token_count")) {
            total = total.saturating_add(n);
        }
    }
    total
}

fn nested_u32(value: &Value, object: &str, key: &str) -> Option<u32> {
    u32_field(value.get(object)?, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_usage_inference_geo_round_trips_and_other_wires_omit_it() {
        let usage = json!({
            "input_tokens": 3,
            "output_tokens": 5,
            "inference_geo": "us",
        });
        let ev = from_anthropic(&usage);
        let IrStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            audio_tokens,
            completion_audio_tokens,
            inference_geo,
        } = &ev
        else {
            panic!("expected usage, got {ev:?}");
        };
        assert_eq!(*prompt_tokens, 3);
        assert_eq!(*completion_tokens, 5);
        assert_eq!(inference_geo.as_deref(), Some("us"));
        let padded = json!({
            "input_tokens": 1,
            "output_tokens": 1,
            "inference_geo": " us ",
        });
        let padded_ev = from_anthropic(&padded);
        let IrStreamEvent::Usage {
            inference_geo: padded_geo,
            ..
        } = &padded_ev
        else {
            panic!("expected usage, got {padded_ev:?}");
        };
        assert_eq!(
            padded_geo.as_deref(),
            Some("us"),
            "inference_geo must be trimmed, got {padded_geo:?}"
        );

        let messages = encode_anthropic(
            *prompt_tokens,
            *completion_tokens,
            *cache_read_tokens,
            *cache_write_tokens,
            *reasoning_tokens,
            inference_geo.as_deref(),
        );
        assert_eq!(
            messages
                .pointer("/usage/inference_geo")
                .and_then(Value::as_str),
            Some("us"),
            "Messages usage must keep inference_geo, got {messages}"
        );

        let chat = encode_chat(
            *prompt_tokens,
            *completion_tokens,
            *cache_read_tokens,
            *cache_write_tokens,
            *reasoning_tokens,
            *audio_tokens,
            *completion_audio_tokens,
        );
        assert!(
            chat.pointer("/usage/inference_geo").is_none(),
            "Chat usage must omit inference_geo, got {chat}"
        );
        let responses = encode_responses(
            *prompt_tokens,
            *completion_tokens,
            *cache_read_tokens,
            *cache_write_tokens,
            *reasoning_tokens,
        );
        assert!(
            responses.pointer("/response/usage/inference_geo").is_none(),
            "Responses usage must omit inference_geo, got {responses}"
        );
        let gemini = encode_gemini(
            *prompt_tokens,
            *completion_tokens,
            *cache_read_tokens,
            *reasoning_tokens,
            *audio_tokens,
            *completion_audio_tokens,
        );
        assert!(
            gemini.pointer("/usageMetadata/inference_geo").is_none(),
            "Gemini usage must omit inference_geo, got {gemini}"
        );

        let blank = json!({
            "input_tokens": 1,
            "output_tokens": 2,
            "inference_geo": "  ",
        });
        let blank_ev = from_anthropic(&blank);
        let IrStreamEvent::Usage {
            inference_geo: blank_geo,
            ..
        } = &blank_ev
        else {
            panic!("expected usage, got {blank_ev:?}");
        };
        assert!(
            blank_geo.is_none(),
            "blank inference_geo must decode as None, got {blank_geo:?}"
        );
        let blank_messages = encode_anthropic(1, 2, 0, 0, 0, blank_geo.as_deref());
        assert!(
            blank_messages.pointer("/usage/inference_geo").is_none(),
            "blank inference_geo must be omitted, got {blank_messages}"
        );
    }
}

//! Usage field mapping. Zero cache counts omit cache keys on encode.

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;

use super::u32_field;

pub(super) fn from_chat(usage: &Value) -> IrStreamEvent {
    let cache_read = nested_u32(usage, "prompt_tokens_details", "cached_tokens")
        .filter(|&n| n > 0)
        .or_else(|| u32_field(usage, "cached_tokens").filter(|&n| n > 0))
        .or_else(|| u32_field(usage, "prompt_cache_hit_tokens"))
        .unwrap_or(0);
    let cache_write = nested_u32(usage, "prompt_tokens_details", "cache_write_tokens").unwrap_or(0);
    let reasoning = nested_u32(usage, "completion_tokens_details", "reasoning_tokens").unwrap_or(0);
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
    }
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
    }
}

pub(super) fn from_responses(usage: &Value) -> IrStreamEvent {
    let cache_read = nested_u32(usage, "input_tokens_details", "cached_tokens").unwrap_or(0);
    let reasoning = nested_u32(usage, "output_tokens_details", "reasoning_tokens").unwrap_or(0);
    IrStreamEvent::Usage {
        prompt_tokens: u32_field(usage, "input_tokens")
            .unwrap_or(0)
            .saturating_sub(cache_read),
        completion_tokens: u32_field(usage, "output_tokens")
            .unwrap_or(0)
            .saturating_sub(reasoning),
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
        reasoning_tokens: reasoning,
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
    }
}

pub(super) fn encode_chat(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    reasoning_tokens: u32,
) -> Value {
    let prompt_wire = prompt_tokens.saturating_add(cache_read_tokens);
    let completion_wire = completion_tokens.saturating_add(reasoning_tokens);
    let mut usage = json!({
        "prompt_tokens": prompt_wire,
        "completion_tokens": completion_wire,
        "total_tokens": prompt_wire.saturating_add(completion_wire),
    });
    if cache_read_tokens > 0 || cache_write_tokens > 0 {
        let mut details = json!({});
        if cache_read_tokens > 0 {
            details["cached_tokens"] = json!(cache_read_tokens);
        }
        if cache_write_tokens > 0 {
            details["cache_write_tokens"] = json!(cache_write_tokens);
        }
        usage["prompt_tokens_details"] = details;
    }
    if reasoning_tokens > 0 {
        usage["completion_tokens_details"] = json!({ "reasoning_tokens": reasoning_tokens });
    }
    json!({ "choices": [], "usage": usage })
}

pub(super) fn encode_anthropic(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    reasoning_tokens: u32,
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
    reasoning_tokens: u32,
) -> Value {
    let mut usage = json!({
        "input_tokens": prompt_tokens.saturating_add(cache_read_tokens),
        "output_tokens": completion_tokens.saturating_add(reasoning_tokens),
    });
    if cache_read_tokens > 0 {
        usage["input_tokens_details"] = json!({ "cached_tokens": cache_read_tokens });
    }
    if reasoning_tokens > 0 {
        usage["output_tokens_details"] = json!({ "reasoning_tokens": reasoning_tokens });
    }
    json!({
        "type": "response.completed",
        "response": { "status": "completed", "usage": usage },
    })
}

pub(super) fn encode_gemini(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    reasoning_tokens: u32,
) -> Value {
    let mut usage = json!({
        "promptTokenCount": prompt_tokens.saturating_add(cache_read_tokens),
        "candidatesTokenCount": completion_tokens,
    });
    if cache_read_tokens > 0 {
        usage["cachedContentTokenCount"] = json!(cache_read_tokens);
    }
    if reasoning_tokens > 0 {
        usage["thoughtsTokenCount"] = json!(reasoning_tokens);
    }
    json!({ "usageMetadata": usage })
}

fn nested_u32(value: &Value, object: &str, key: &str) -> Option<u32> {
    u32_field(value.get(object)?, key)
}

//! Usage field mapping. Zero cache counts omit cache keys on encode.

use serde_json::{Value, json};

use crate::ir::IrStreamEvent;

use super::u32_field;

pub(super) fn from_chat(usage: &Value) -> IrStreamEvent {
    IrStreamEvent::Usage {
        prompt_tokens: u32_field(usage, "prompt_tokens").unwrap_or(0),
        completion_tokens: u32_field(usage, "completion_tokens").unwrap_or(0),
        cache_read_tokens: nested_u32(usage, "prompt_tokens_details", "cached_tokens")
            .or_else(|| u32_field(usage, "cached_tokens"))
            .unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: nested_u32(usage, "completion_tokens_details", "reasoning_tokens")
            .unwrap_or(0),
    }
}

pub(super) fn from_anthropic(usage: &Value) -> IrStreamEvent {
    IrStreamEvent::Usage {
        prompt_tokens: u32_field(usage, "input_tokens").unwrap_or(0),
        completion_tokens: u32_field(usage, "output_tokens").unwrap_or(0),
        cache_read_tokens: u32_field(usage, "cache_read_input_tokens").unwrap_or(0),
        cache_write_tokens: u32_field(usage, "cache_creation_input_tokens").unwrap_or(0),
        reasoning_tokens: nested_u32(usage, "output_tokens_details", "thinking_tokens")
            .unwrap_or(0),
    }
}

pub(super) fn from_responses(usage: &Value) -> IrStreamEvent {
    IrStreamEvent::Usage {
        prompt_tokens: u32_field(usage, "input_tokens").unwrap_or(0),
        completion_tokens: u32_field(usage, "output_tokens").unwrap_or(0),
        cache_read_tokens: nested_u32(usage, "input_tokens_details", "cached_tokens").unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: nested_u32(usage, "output_tokens_details", "reasoning_tokens")
            .unwrap_or(0),
    }
}

pub(super) fn encode_chat(
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    reasoning_tokens: u32,
) -> Value {
    let mut usage = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens.saturating_add(completion_tokens),
    });
    // Only emit cache/reasoning details when the IR actually has them.
    if cache_read_tokens > 0 {
        usage["prompt_tokens_details"] = json!({ "cached_tokens": cache_read_tokens });
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
        "output_tokens": completion_tokens,
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
        "input_tokens": prompt_tokens,
        "output_tokens": completion_tokens,
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

fn nested_u32(value: &Value, object: &str, key: &str) -> Option<u32> {
    u32_field(value.get(object)?, key)
}

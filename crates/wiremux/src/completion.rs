//! Whether a JSON success body already has caller-visible output.
//!
//! Chat `choices` and Gemini `candidates` count when a slot has text,
//! a refusal, or a tool call. An empty string does not. Responses
//! `output` and Messages `content` stay non-empty arrays, including a
//! message shell with no text field.

use serde_json::Value;

pub(crate) fn json_has_completion(value: &Value) -> bool {
    array_any(value, "choices", choice_visible)
        || array_any(value, "candidates", candidate_visible)
        || nonempty_array(value, "output")
        || nonempty_array(value, "content")
}

fn nonempty_array(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
}

fn array_any(value: &Value, key: &str, pred: impl Fn(&Value) -> bool) -> bool {
    value
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(pred))
}

fn nonempty_str(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty())
}

fn content_visible(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => items.iter().any(content_part_visible),
        _ => false,
    }
}

fn content_part_visible(item: &Value) -> bool {
    item.as_str().is_some_and(|text| !text.is_empty())
        || nonempty_str(item, "text")
        || nonempty_str(item, "refusal")
        || item
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "image_url" | "image" | "input_audio" | "audio"))
}

fn choice_visible(choice: &Value) -> bool {
    let body = choice
        .get("message")
        .or_else(|| choice.get("delta"))
        .unwrap_or(choice);
    if body.get("content").is_some_and(content_visible) {
        return true;
    }
    if nonempty_str(body, "refusal")
        || nonempty_str(body, "reasoning")
        || nonempty_str(body, "reasoning_content")
    {
        return true;
    }
    if audio_visible(body) {
        return true;
    }
    if body
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
    {
        return true;
    }
    body.get("function_call").is_some_and(Value::is_object)
}

fn audio_visible(body: &Value) -> bool {
    let Some(audio) = body.get("audio").filter(|value| value.is_object()) else {
        return false;
    };
    nonempty_str(audio, "data") || nonempty_str(audio, "transcript")
}

fn candidate_visible(candidate: &Value) -> bool {
    nonempty_str(candidate, "finishMessage")
        || candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .is_some_and(|parts| parts.iter().any(gemini_part_visible))
}

fn gemini_part_visible(part: &Value) -> bool {
    const KEPT: &[&str] = &[
        "functionCall",
        "inlineData",
        "fileData",
        "executableCode",
        "codeExecutionResult",
        "functionResponse",
        "toolCall",
        "toolResponse",
    ];
    nonempty_str(part, "text") || KEPT.iter().any(|key| part.get(*key).is_some())
}

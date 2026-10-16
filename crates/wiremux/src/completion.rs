//! Whether a JSON success body already has caller-visible output.
//!
//! Chat `choices` and Gemini `candidates` count when a slot has text,
//! a refusal, audio bytes or a transcript, or a tool call. An empty
//! string does not. An `image_url` or `input_audio` part counts only
//! when it has a URL, data, or transcript. A tool call counts when it
//! has an id, a name, or arguments. A Gemini part counts only when the
//! leaf has a name, an id, args, bytes, a file URI, code, an outcome,
//! or a response. The key alone does not. Responses `output` and
//! Messages `content` stay non-empty arrays, including a message shell
//! with no text field.

use serde_json::Value;

pub(crate) fn json_has_completion(value: &Value) -> bool {
    array_any(value, "choices", choice_visible)
        || array_any(value, "candidates", candidate_visible)
        || nonempty_array(value, "output")
        || nonempty_array(value, "content")
}

/// Non-empty `error` string. Object errors stay on the object parsers.
pub(crate) fn json_string_error(value: &Value) -> Option<&str> {
    value
        .get("error")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// `error` object text. Numeric `code` and `status` count, same as the proxy.
pub(crate) fn error_object_detail(err: &Value) -> Option<String> {
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let code = ["code", "type", "status"].iter().find_map(|key| {
        let value = err.get(*key)?;
        if let Some(text) = value.as_str() {
            let text = text.trim();
            if text.is_empty() || (*key == "type" && text == "error") {
                return None;
            }
            return Some(text.to_string());
        }
        value.as_i64().map(|number| number.to_string())
    });
    match (code, message) {
        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
        (None, Some(message)) => Some(message.to_string()),
        (Some(code), None) => Some(code),
        (None, None) => None,
    }
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
        || media_payload_visible(item)
}

/// URL, bytes, or transcript. The `type` name alone is an empty shell.
fn media_payload_visible(value: &Value) -> bool {
    nonempty_str(value, "url")
        || nonempty_str(value, "data")
        || nonempty_str(value, "transcript")
        || value.get("image_url").is_some_and(media_leaf_visible)
        || value.get("input_audio").is_some_and(media_leaf_visible)
        || value.get("audio").is_some_and(media_leaf_visible)
        || value.get("source").is_some_and(media_leaf_visible)
}

fn media_leaf_visible(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.is_empty(),
        Value::Object(_) => {
            nonempty_str(value, "url")
                || nonempty_str(value, "data")
                || nonempty_str(value, "transcript")
                || value.get("source").is_some_and(media_leaf_visible)
        }
        _ => false,
    }
}

fn argument_visible(value: &Value) -> bool {
    match value.get("arguments").or_else(|| value.get("input")) {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

/// Id, name, or arguments. `{}` and an index-only delta are not a call.
fn tool_call_visible(call: &Value) -> bool {
    if nonempty_str(call, "id") || nonempty_str(call, "name") || argument_visible(call) {
        return true;
    }
    ["function", "custom"].iter().any(|key| {
        call.get(*key).is_some_and(|body| {
            nonempty_str(body, "name") || nonempty_str(body, "id") || argument_visible(body)
        })
    })
}

pub(crate) fn choice_visible(choice: &Value) -> bool {
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
        .is_some_and(|calls| calls.iter().any(tool_call_visible))
    {
        return true;
    }
    body.get("function_call").is_some_and(tool_call_visible)
}

fn audio_visible(body: &Value) -> bool {
    let Some(audio) = body.get("audio").filter(|value| value.is_object()) else {
        return false;
    };
    nonempty_str(audio, "data") || nonempty_str(audio, "transcript")
}

pub(crate) fn candidate_visible(candidate: &Value) -> bool {
    nonempty_str(candidate, "finishMessage")
        || candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .is_some_and(|parts| parts.iter().any(gemini_part_visible))
}

pub(crate) fn gemini_function_call_visible(call: &Value) -> bool {
    nonempty_str(call, "id") || nonempty_str(call, "name") || json_payload(call.get("args"))
}

fn json_payload(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

fn gemini_response_visible(value: &Value) -> bool {
    nonempty_str(value, "name") || nonempty_str(value, "id") || json_payload(value.get("response"))
}

/// File, code, or tool payload. An empty object is not a part.
pub(crate) fn gemini_side_part_visible(part: &Value) -> bool {
    part.get("fileData")
        .is_some_and(|value| nonempty_str(value, "fileUri") || nonempty_str(value, "uri"))
        || part
            .get("executableCode")
            .is_some_and(|value| nonempty_str(value, "language") || nonempty_str(value, "code"))
        || part
            .get("codeExecutionResult")
            .is_some_and(|value| nonempty_str(value, "outcome") || nonempty_str(value, "output"))
        || part
            .get("functionResponse")
            .is_some_and(gemini_response_visible)
        || part
            .get("toolCall")
            .is_some_and(gemini_function_call_visible)
        || part
            .get("toolResponse")
            .is_some_and(gemini_response_visible)
}

fn gemini_part_visible(part: &Value) -> bool {
    nonempty_str(part, "text")
        || part
            .get("functionCall")
            .is_some_and(gemini_function_call_visible)
        || part
            .get("inlineData")
            .is_some_and(|value| nonempty_str(value, "data"))
        || gemini_side_part_visible(part)
}

//! SSE encode/decode for the three v1 dialects.

mod chat;
mod gemini;
mod messages;
mod responses;
mod sse;
mod usage;

use serde_json::Value;
use wiremux_auth::{ResolvedProfile, StreamUnknownPolicy, Wire};

use crate::ir::IrStreamEvent;
use crate::map::MapError;

/// Max parallel tool-call index. Hostile values must not force an allocation.
pub const MAX_TOOL_CALL_INDEX: u32 = 128;
/// Max Anthropic `content_block` index. Same cap as [`MAX_TOOL_CALL_INDEX`].
pub const MAX_CONTENT_BLOCK_INDEX: u32 = 128;

/// One SSE frame (`event:` + `data:`). Chat Completions omits `event:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSse {
    /// SSE `event:` field, if present.
    pub event: Option<String>,
    /// SSE `data:` payload (JSON or `[DONE]`).
    pub data: String,
}

impl RawSse {
    /// Parse a document of SSE frames. Comments and blank lines are skipped.
    #[must_use]
    pub fn parse_all(text: &str) -> Vec<Self> {
        let mut reader = sse::SseFrameReader::new();
        let mut out = reader.feed(text.as_bytes()).unwrap_or_default();
        if let Some(last) = reader.drain() {
            out.push(last);
        }
        out
    }
}

pub use sse::{MAX_SSE_PENDING, SseFrameReader};

/// Decode one SSE frame. `None` is a recognized no-op (ping, empty delta).
///
/// Unknown names follow `profile.dialect.stream_unknown_policy`. A
/// tool-bearing frame is never `Ok(None)`.
pub fn decode_stream_event(
    wire: Wire,
    raw: &RawSse,
    profile: &ResolvedProfile,
) -> Result<Option<IrStreamEvent>, MapError> {
    if raw.event.is_none() && raw.data.trim().is_empty() {
        return Ok(None);
    }

    let name = frame_event_name(wire, raw);
    if !profile.dialect.stream_events.iter().any(|ev| ev == &name) {
        return unknown_event(profile, &name, &raw.data);
    }

    if name == "[DONE]" || raw.data.trim() == "[DONE]" {
        return Ok(Some(IrStreamEvent::Done));
    }

    let value: Value = serde_json::from_str(&raw.data)?;
    match wire {
        Wire::ChatCompletions => chat::decode(&value),
        Wire::Messages => messages::decode(&name, &value),
        Wire::Responses => responses::decode(&name, &value),
        Wire::Gemini => gemini::decode(&value),
    }
}

/// Decode one SSE frame into every IR event it carries.
///
/// Bline emits usage and finish from the same `message_delta`. A 1:1 map
/// would drop one. Empty vec is a recognized no-op.
pub fn decode_stream_events(
    wire: Wire,
    raw: &RawSse,
    profile: &ResolvedProfile,
) -> Result<Vec<IrStreamEvent>, MapError> {
    let first = decode_stream_event(wire, raw, profile)?;
    let Some(first) = first else {
        return Ok(Vec::new());
    };
    let mut out = vec![first];
    if matches!(wire, Wire::Messages)
        && frame_event_name(wire, raw) == "message_delta"
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
        && matches!(out[0], IrStreamEvent::FinishReason { .. })
        && let Some(usage) = value.get("usage").filter(|v| v.is_object())
    {
        out.push(usage::from_anthropic(usage));
    }
    Ok(out)
}

/// Encode one IR event into the target dialect's SSE shape.
pub fn encode_stream_event(wire: Wire, ev: &IrStreamEvent) -> Result<RawSse, MapError> {
    match ev {
        IrStreamEvent::Unknown { event, raw } => Ok(encode_named(event, raw)),
        IrStreamEvent::Protocol { item_type, payload } => Ok(encode_named(item_type, payload)),
        other => match wire {
            Wire::ChatCompletions => chat::encode(other),
            Wire::Messages => messages::encode(other),
            Wire::Responses => responses::encode(other),
            Wire::Gemini => gemini::encode(other),
        },
    }
}

fn encode_named(name: &str, raw: &Value) -> RawSse {
    let data = match raw {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    RawSse {
        event: sse_event_field(name),
        data,
    }
}

fn sse_event_field(name: &str) -> Option<String> {
    match name {
        "" | "chunk" | "[DONE]" => None,
        other => Some(other.to_string()),
    }
}

fn frame_event_name(wire: Wire, raw: &RawSse) -> String {
    if let Some(ev) = raw.event.as_deref().filter(|s| !s.is_empty()) {
        return ev.to_string();
    }
    let trimmed = raw.data.trim();
    if trimmed == "[DONE]" {
        return "[DONE]".into();
    }
    if matches!(wire, Wire::ChatCompletions | Wire::Gemini) {
        return "chunk".into();
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && let Some(ty) = value.get("type").and_then(Value::as_str)
    {
        return ty.to_string();
    }
    "chunk".into()
}

fn unknown_event(
    profile: &ResolvedProfile,
    name: &str,
    data: &str,
) -> Result<Option<IrStreamEvent>, MapError> {
    match profile.dialect.stream_unknown_policy {
        StreamUnknownPolicy::HardError => Err(MapError::HardError {
            path: name.to_string(),
            detail: format!("unknown stream event `{name}`"),
        }),
        StreamUnknownPolicy::Passthrough => {
            let raw =
                serde_json::from_str(data).unwrap_or_else(|_| Value::String(data.to_string()));
            Ok(Some(IrStreamEvent::Unknown {
                event: name.to_string(),
                raw,
            }))
        }
    }
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// Reject a present index above the cap. Absence is allowed.
///
/// Compare only; never resize a buffer to `index`.
fn check_index(value: &Value, key: &str, cap: u32, kind: &str) -> Result<(), MapError> {
    let Some(raw) = value.get(key) else {
        return Ok(());
    };
    let Some(n) = raw.as_u64() else {
        return Err(MapError::Invalid(format!(
            "{kind} index exceeds cap ({cap})"
        )));
    };
    if n > u64::from(cap) {
        return Err(MapError::Invalid(format!(
            "{kind} index {n} exceeds cap ({cap})"
        )));
    }
    Ok(())
}

fn protocol(name: &str, value: &Value) -> IrStreamEvent {
    IrStreamEvent::Protocol {
        item_type: name.to_string(),
        payload: value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_splits_frames_and_skips_comments() {
        let frames = RawSse::parse_all(
            ": keep-alive\n\nevent: ping\ndata: {\"type\":\"ping\"}\n\ndata: [DONE]\n\n",
        );
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("ping"));
        assert_eq!(frames[0].data, r#"{"type":"ping"}"#);
        assert_eq!(frames[1].event, None);
        assert_eq!(frames[1].data, "[DONE]");
    }
}

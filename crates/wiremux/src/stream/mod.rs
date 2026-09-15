//! SSE encode/decode for the four v1 dialects.

mod chat;
mod complete;
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

#[cfg(feature = "proxy")]
pub(crate) use chat::map_finish;
pub use complete::{decode_response, encode_response};
pub use sse::{MAX_SSE_PENDING, SseFrameReader};
#[cfg(feature = "proxy")]
pub(crate) use usage::from_chat;

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
    if matches!(wire, Wire::ChatCompletions)
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
    {
        let events = chat::decode_all(&value)?;
        if !events.is_empty() {
            if let Some(IrStreamEvent::Protocol { .. }) = events.first()
                && let Some(mut expanded) = expand_complete_tool_call(wire, &events[0], raw)
            {
                expanded.extend(events.into_iter().skip(1));
                return Ok(expanded);
            }
            let lone_protocol = matches!(events.as_slice(), [IrStreamEvent::Protocol { .. }]);
            if !lone_protocol {
                return Ok(events);
            }
        }
    }
    if matches!(wire, Wire::Responses)
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
        && let Some(events) =
            responses::decode_terminal_events(&frame_event_name(wire, raw), &value)
    {
        return Ok(events);
    }
    let Some(first) = first else {
        return Ok(Vec::new());
    };
    if matches!(wire, Wire::Gemini)
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
        && let Some(events) = fan_out_gemini_parts(&value)
    {
        return Ok(events);
    }
    if let Some(expanded) = expand_complete_tool_call(wire, &first, raw) {
        return Ok(expanded);
    }
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

/// Walk every Gemini part. First-part-wins in `decode` would drop a later
/// `functionCall` after thought or text.
fn fan_out_gemini_parts(value: &Value) -> Option<Vec<IrStreamEvent>> {
    let parts = value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)?;
    let mut out = Vec::new();
    for part in parts {
        out.extend(gemini_part_events(part));
    }
    if let Some(reason) = value
        .pointer("/candidates/0/finishReason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::FinishReason {
            reason: gemini::map_finish(reason).to_string(),
        });
    }
    if let Some(usage) = value.get("usageMetadata").filter(|v| v.is_object()) {
        out.push(usage::from_gemini(usage));
    }
    if out.len() < 2 {
        return None;
    }
    Some(out)
}

fn gemini_part_events(part: &Value) -> Vec<IrStreamEvent> {
    let mut out = Vec::new();
    if part.get("thought").and_then(Value::as_bool) == Some(true)
        && let Some(text) = part
            .get("text")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::ReasoningDelta {
            text: text.to_string(),
        });
    }
    if let Some(sig) = part
        .get("thoughtSignature")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        && part.get("functionCall").is_none()
    {
        out.push(IrStreamEvent::ReasoningSignature {
            signature: sig.to_string(),
        });
    }
    if let Some(fc) = part.get("functionCall") {
        let name = str_field(fc, "name").unwrap_or_default();
        let thought_signature = part
            .get("thoughtSignature")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.push(IrStreamEvent::ToolCallStart {
            id: name.clone(),
            name,
            thought_signature,
        });
        if let Some(args) = fc
            .get("args")
            .filter(|a| a.as_object().is_none_or(|m| !m.is_empty()) && !a.is_null())
        {
            out.push(IrStreamEvent::ToolCallArgDelta {
                delta: args.to_string(),
            });
        }
    } else if out.is_empty()
        && let Some(text) = part
            .get("text")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::TextDelta {
            text: text.to_string(),
        });
    }
    out
}

fn expand_complete_tool_call(
    wire: Wire,
    first: &IrStreamEvent,
    raw: &RawSse,
) -> Option<Vec<IrStreamEvent>> {
    let value: Value = serde_json::from_str(&raw.data).ok()?;
    match wire {
        Wire::ChatCompletions => expand_chat_tool_call(first, &value),
        Wire::Gemini => expand_gemini_function_call(first, &value),
        Wire::Responses => expand_responses_function_call(first, &value),
        Wire::Messages => None,
    }
}

fn expand_chat_tool_call(first: &IrStreamEvent, value: &Value) -> Option<Vec<IrStreamEvent>> {
    let IrStreamEvent::Protocol { .. } = first else {
        return None;
    };
    let tool_calls = value
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)?;
    if tool_calls.len() > 1 {
        return None;
    }
    let call = tool_calls.first()?;
    if let Some(ty) = call.get("type").and_then(Value::as_str)
        && ty != "function"
    {
        return None;
    }
    let func = call.get("function").unwrap_or(call);
    let id = str_field(call, "id").unwrap_or_default();
    let name = str_field(func, "name").unwrap_or_default();
    let args = str_field(func, "arguments").filter(|s| !s.is_empty())?;
    if id.is_empty() && name.is_empty() {
        return None;
    }
    Some(vec![
        IrStreamEvent::ToolCallStart {
            id,
            name,
            thought_signature: None,
        },
        IrStreamEvent::ToolCallArgDelta { delta: args },
    ])
}

fn expand_gemini_function_call(first: &IrStreamEvent, value: &Value) -> Option<Vec<IrStreamEvent>> {
    let parts = value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)?;
    let part = parts.iter().find(|p| p.get("functionCall").is_some())?;
    let fc = part.get("functionCall")?;
    let name = str_field(fc, "name").unwrap_or_default();
    let args = fc
        .get("args")
        .filter(|a| a.as_object().is_none_or(|m| !m.is_empty()) && !a.is_null())?;
    let thought_signature = part
        .get("thoughtSignature")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    match first {
        IrStreamEvent::Protocol { .. } | IrStreamEvent::ToolCallStart { .. } => {}
        _ => return None,
    }
    Some(vec![
        IrStreamEvent::ToolCallStart {
            id: name.clone(),
            name,
            thought_signature,
        },
        IrStreamEvent::ToolCallArgDelta {
            delta: args.to_string(),
        },
    ])
}

fn expand_responses_function_call(
    first: &IrStreamEvent,
    value: &Value,
) -> Option<Vec<IrStreamEvent>> {
    let IrStreamEvent::ToolCallStart {
        id,
        name,
        thought_signature,
    } = first
    else {
        return None;
    };
    let args = value
        .pointer("/item/arguments")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    Some(vec![
        IrStreamEvent::ToolCallStart {
            id: id.clone(),
            name: name.clone(),
            thought_signature: thought_signature.clone(),
        },
        IrStreamEvent::ToolCallArgDelta {
            delta: args.to_string(),
        },
    ])
}

/// Merge Chat tool-call starts that arrive as id-only then name-only.
#[derive(Debug, Default)]
pub struct ToolCallAssembler {
    pending: Option<IrStreamEvent>,
}

impl ToolCallAssembler {
    /// Empty assembler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one event. Incomplete starts are held until the name or id arrives.
    pub fn push(&mut self, ev: IrStreamEvent) -> Vec<IrStreamEvent> {
        match ev {
            IrStreamEvent::ToolCallStart {
                id,
                name,
                thought_signature,
            } => self.push_start(id, name, thought_signature),
            other => {
                let mut out = Vec::new();
                if let Some(pending) = self.pending.take() {
                    out.push(pending);
                }
                out.push(other);
                out
            }
        }
    }

    /// Emit a held start at end of stream.
    pub fn flush(&mut self) -> Vec<IrStreamEvent> {
        self.pending.take().into_iter().collect()
    }

    fn push_start(
        &mut self,
        id: String,
        name: String,
        thought_signature: Option<String>,
    ) -> Vec<IrStreamEvent> {
        let incoming = IrStreamEvent::ToolCallStart {
            id,
            name,
            thought_signature,
        };
        match self.pending.take() {
            Some(IrStreamEvent::ToolCallStart {
                id: pid,
                name: pname,
                thought_signature: psig,
            }) => {
                let IrStreamEvent::ToolCallStart {
                    id,
                    name,
                    thought_signature,
                } = incoming
                else {
                    unreachable!("incoming is ToolCallStart");
                };
                let merged = IrStreamEvent::ToolCallStart {
                    id: if id.is_empty() { pid } else { id },
                    name: if name.is_empty() { pname } else { name },
                    thought_signature: thought_signature.or(psig),
                };
                if let IrStreamEvent::ToolCallStart { id, name, .. } = &merged
                    && (id.is_empty() || name.is_empty())
                {
                    self.pending = Some(merged);
                    Vec::new()
                } else {
                    vec![merged]
                }
            }
            Some(other) => {
                self.pending = Some(incoming);
                vec![other]
            }
            None => {
                if let IrStreamEvent::ToolCallStart { id, name, .. } = &incoming
                    && (id.is_empty() || name.is_empty())
                {
                    self.pending = Some(incoming);
                    Vec::new()
                } else {
                    vec![incoming]
                }
            }
        }
    }
}

/// Whether this IR event has a slot on `wire` SSE.
///
/// Protocol names are dialect-specific. A Messages `message_start` must
/// not become `event: message_start` on a Chat Completions client.
#[cfg(feature = "proxy")]
#[must_use]
pub(crate) fn event_has_slot(wire: Wire, ev: &IrStreamEvent) -> bool {
    match ev {
        IrStreamEvent::Protocol { item_type, .. } => wire
            .default_stream_events()
            .iter()
            .any(|name| *name == item_type),
        IrStreamEvent::Unknown { .. } => matches!(wire, Wire::Messages | Wire::Responses),
        _ => true,
    }
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
            detail: format!(
                "unknown stream event `{name}` (stream_unknown_policy = hard-error|passthrough, or add `{name}` to stream_events)"
            ),
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

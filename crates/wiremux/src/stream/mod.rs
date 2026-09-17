//! SSE encode/decode for the v1 dialects.

mod chat;
mod complete;
mod converse;
mod encoder;
mod eventstream;
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

pub use complete::{decode_response, encode_response, encode_response_with_model};
pub use encoder::StreamEncoder;
#[cfg(feature = "proxy")]
pub(crate) use eventstream::unwrap_event_payload;
pub use eventstream::{
    EventStreamReader, MAX_EVENTSTREAM_PENDING,
    encode_exception_message as encode_eventstream_exception,
    encode_message as encode_eventstream_message,
};
pub(crate) use gemini::gemini_call_id;
pub use sse::{MAX_SSE_PENDING, SseFrameReader};

/// Incremental frames from SSE or AWS Event Stream.
pub enum UpstreamFrames {
    /// `text/event-stream`.
    Sse(SseFrameReader),
    /// `application/vnd.amazon.eventstream`.
    Event(EventStreamReader),
}

impl UpstreamFrames {
    /// Event Stream for Converse; SSE otherwise.
    #[must_use]
    pub fn for_wire(wire: Wire) -> Self {
        if matches!(wire, Wire::Converse) {
            Self::Event(EventStreamReader::new())
        } else {
            Self::Sse(SseFrameReader::new())
        }
    }

    /// Append bytes and emit complete frames.
    ///
    /// The optional string is a terminal Event Stream exception after
    /// any frames already parsed from the same chunk.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(Vec<RawSse>, Option<String>), String> {
        match self {
            Self::Sse(r) => r.feed(bytes).map(|frames| (frames, None)),
            Self::Event(r) => r.feed(bytes),
        }
    }

    /// Trailing SSE frame, if any.
    pub fn drain(&mut self) -> Option<RawSse> {
        match self {
            Self::Sse(r) => r.drain(),
            Self::Event(r) => r.drain(),
        }
    }
}

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
        Wire::Converse => converse::decode(&value),
        _ => Err(MapError::Invalid(format!(
            "unsupported wire `{}`",
            wire.as_str()
        ))),
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
fn gemini_value_has_function_call(value: &Value) -> bool {
    value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(|p| p.get("functionCall").is_some()))
}

fn fan_out_gemini_parts(value: &Value) -> Option<Vec<IrStreamEvent>> {
    let parts = value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)?;
    let mut out = Vec::new();
    let mut call_seq = 0usize;
    for part in parts {
        out.extend(gemini_part_events(part, &mut call_seq));
    }
    if let Some(reason) = value
        .pointer("/candidates/0/finishReason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::FinishReason {
            reason: gemini::map_finish(reason, gemini_value_has_function_call(value)).to_string(),
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

fn gemini_part_events(part: &Value, call_seq: &mut usize) -> Vec<IrStreamEvent> {
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
        let id = gemini::gemini_call_id(fc, &name, *call_seq);
        *call_seq += 1;
        let index = u32::try_from(*call_seq).unwrap_or(0);
        out.push(IrStreamEvent::ToolCallStart {
            id,
            name,
            thought_signature,
            index,
        });
        if let Some(args) = fc
            .get("args")
            .filter(|a| a.as_object().is_none_or(|m| !m.is_empty()) && !a.is_null())
        {
            out.push(IrStreamEvent::ToolCallArgDelta {
                delta: args.to_string(),
                index,
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
        Wire::Messages | Wire::Converse => None,
        _ => None,
    }
}

fn expand_chat_tool_call(first: &IrStreamEvent, value: &Value) -> Option<Vec<IrStreamEvent>> {
    let IrStreamEvent::Protocol { .. } = first else {
        return None;
    };
    let tool_calls = value
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)?;
    let mut out = Vec::new();
    for call in tool_calls {
        let expanded = chat::expand_tool_call(call, value);
        if expanded
            .iter()
            .all(|ev| matches!(ev, IrStreamEvent::Protocol { .. }))
        {
            continue;
        }
        out.extend(
            expanded
                .into_iter()
                .filter(|ev| !matches!(ev, IrStreamEvent::Protocol { .. })),
        );
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
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
            index: 0,
        },
        IrStreamEvent::ToolCallArgDelta {
            delta: args.to_string(),
            index: 0,
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
        index,
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
            index: *index,
        },
        IrStreamEvent::ToolCallArgDelta {
            delta: args.to_string(),
            index: *index,
        },
    ])
}

/// Merge Chat tool-call starts that arrive as id-only then name-only.
///
/// Pending starts are keyed by `index` so interleaved parallel calls
/// do not overwrite each other.
#[derive(Debug, Default)]
pub struct ToolCallAssembler {
    pending: std::collections::BTreeMap<u32, IrStreamEvent>,
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
                index,
            } => self.push_start(id, name, thought_signature, index),
            IrStreamEvent::ToolCallArgDelta { delta, index } => {
                let mut out = Vec::new();
                if let Some(pending) = self.pending.remove(&index) {
                    out.push(pending);
                }
                out.push(IrStreamEvent::ToolCallArgDelta { delta, index });
                out
            }
            other => {
                let mut out = self.flush();
                out.push(other);
                out
            }
        }
    }

    /// Emit held starts at end of stream.
    pub fn flush(&mut self) -> Vec<IrStreamEvent> {
        std::mem::take(&mut self.pending).into_values().collect()
    }

    fn push_start(
        &mut self,
        id: String,
        name: String,
        thought_signature: Option<String>,
        index: u32,
    ) -> Vec<IrStreamEvent> {
        let incoming = IrStreamEvent::ToolCallStart {
            id,
            name,
            thought_signature,
            index,
        };
        match self.pending.remove(&index) {
            Some(IrStreamEvent::ToolCallStart {
                id: pid,
                name: pname,
                thought_signature: psig,
                index: _,
            }) => {
                let IrStreamEvent::ToolCallStart {
                    id,
                    name,
                    thought_signature,
                    index,
                } = incoming
                else {
                    unreachable!("incoming is ToolCallStart");
                };
                let merged = IrStreamEvent::ToolCallStart {
                    id: if id.is_empty() { pid } else { id },
                    name: if name.is_empty() { pname } else { name },
                    thought_signature: thought_signature.or(psig),
                    index,
                };
                if let IrStreamEvent::ToolCallStart { id, name, .. } = &merged
                    && (id.is_empty() || name.is_empty())
                {
                    self.pending.insert(index, merged);
                    Vec::new()
                } else {
                    vec![merged]
                }
            }
            Some(other) => {
                self.pending.insert(index, incoming);
                vec![other]
            }
            None => {
                if let IrStreamEvent::ToolCallStart { id, name, .. } = &incoming
                    && (id.is_empty() || name.is_empty())
                {
                    self.pending.insert(index, incoming);
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
/// Protocol is never a slot. Names are dialect-specific (`chunk` is
/// Chat and Gemini), so a Chat Protocol must not re-emit on Gemini.
#[cfg(feature = "proxy")]
#[must_use]
pub(crate) fn event_has_slot(wire: Wire, ev: &IrStreamEvent) -> bool {
    match ev {
        IrStreamEvent::Protocol { .. } => false,
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
            Wire::Converse => {
                let value = converse::encode(other)?;
                Ok(RawSse {
                    event: None,
                    data: value.to_string(),
                })
            }
            _ => Err(MapError::Invalid(format!(
                "unsupported wire `{}`",
                wire.as_str()
            ))),
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

pub(crate) fn frame_event_name(wire: Wire, raw: &RawSse) -> String {
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
    if matches!(wire, Wire::Converse) {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            for name in [
                "contentBlockDelta",
                "contentBlockStart",
                "contentBlockStop",
                "messageStop",
                "metadata",
                "messageStart",
            ] {
                if value.get(name).is_some() {
                    return name.to_string();
                }
            }
        }
        return "contentBlockDelta".into();
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

    #[cfg(feature = "proxy")]
    #[test]
    fn event_has_slot_protocol_chunk_is_not_gemini_slot() {
        let ev = IrStreamEvent::Protocol {
            item_type: "chunk".into(),
            payload: Value::Null,
        };
        assert!(!event_has_slot(Wire::Gemini, &ev));
    }
}

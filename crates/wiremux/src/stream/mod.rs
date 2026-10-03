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
    /// A line or data payload over the SSE cap is an error, not an empty list.
    pub fn parse_all(text: &str) -> Result<Vec<Self>, String> {
        let mut reader = sse::SseFrameReader::new();
        let mut out = reader.feed(text.as_bytes())?;
        if let Some(last) = reader.finish()? {
            out.push(last);
        }
        Ok(out)
    }
}

pub use complete::{
    decode_response, decode_response_with_loss, encode_response, encode_response_with_model,
};
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

    /// EOF. SSE may emit one last frame. Event Stream fails if bytes remain.
    #[cfg(any(feature = "client", feature = "proxy"))]
    pub fn finish(&mut self) -> Result<Option<RawSse>, String> {
        match self {
            Self::Sse(r) => r.finish(),
            Self::Event(r) => r.finish(),
        }
    }
}

/// Decode one SSE frame. `None` is a recognized no-op (ping, empty delta).
///
/// Unknown names follow `profile.dialect.stream_unknown_policy`. A
/// tool-bearing frame is never `Ok(None)`.
///
/// Chat Completions returns one event for a single text, audio, finish,
/// or usage field. Content together with `finish_reason` or `usage`,
/// two media parts, two `tool_calls`, or one call that includes an id
/// or a name plus `arguments`, is [`MapError::Invalid`] and names
/// [`decode_stream_events`]. That function keeps every call and its
/// argument text. A later chunk that is only
/// `{"index":0,"function":{"arguments":"..."}}` stays
/// [`IrStreamEvent::ToolCallArgDelta`].
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
        return unknown_event(wire, profile, &name, &raw.data);
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

/// Gemini function-call sequence for one upstream stream.
///
/// [`decode_stream_events`] starts a fresh decoder, so its ids begin at 0
/// on every call. No process-global counter.
#[derive(Debug, Default)]
pub struct StreamDecoder {
    gemini_call_seq: usize,
}

impl StreamDecoder {
    /// Sequence starts at 0.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode one frame. Gemini call ids advance across calls on `self`.
    pub fn decode(
        &mut self,
        wire: Wire,
        raw: &RawSse,
        profile: &ResolvedProfile,
    ) -> Result<Vec<IrStreamEvent>, MapError> {
        decode_stream_events_seq(wire, raw, profile, &mut self.gemini_call_seq)
    }
}

/// Decode one SSE frame into every IR event it carries.
///
/// Chat Completions can put content, `finish_reason`, and `usage` on
/// one chunk. A Messages `message_delta` can carry a stop reason and
/// `usage`. Singular decode keeps one of those. Empty vec is a
/// recognized no-op.
///
/// Gemini function-call ids start at 0. Use [`StreamDecoder`] to keep
/// the sequence across chunks of one stream.
pub fn decode_stream_events(
    wire: Wire,
    raw: &RawSse,
    profile: &ResolvedProfile,
) -> Result<Vec<IrStreamEvent>, MapError> {
    let mut seq = 0usize;
    decode_stream_events_seq(wire, raw, profile, &mut seq)
}

fn decode_stream_events_seq(
    wire: Wire,
    raw: &RawSse,
    profile: &ResolvedProfile,
    call_seq: &mut usize,
) -> Result<Vec<IrStreamEvent>, MapError> {
    let first = decode_stream_event(wire, raw, profile);
    if matches!(wire, Wire::ChatCompletions)
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
    {
        let events = chat::decode_all(&value)?;
        // Singular refuses a frame that would drop arguments or a second
        // call. The plural decode above already kept them.
        let singular_limit = matches!(
            &first,
            Err(MapError::Invalid(detail)) if detail.contains("decode_stream_events")
        );
        if !events.is_empty() && (first.is_ok() || singular_limit) {
            // decode_all already expanded every tool call. Re-expanding a
            // leading Protocol entry and then appending the tail duplicates
            // a later custom or function call and drops the unknown entry.
            let lone_protocol = matches!(events.as_slice(), [IrStreamEvent::Protocol { .. }]);
            if !lone_protocol {
                return Ok(events);
            }
            if let Some(expanded) = expand_complete_tool_call(wire, &events[0], raw)? {
                return Ok(expanded);
            }
        }
    }
    if matches!(wire, Wire::Responses)
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
    {
        let name = frame_event_name(wire, raw);
        let singular_limit = matches!(
            &first,
            Err(MapError::Invalid(detail)) if detail.contains("decode_stream_events")
        );
        if let Some(events) = responses::decode_terminal_events(&name, &value)
            && first.is_ok()
        {
            return Ok(events);
        }
        let events = responses::decode_all(&name, &value)?;
        if !events.is_empty() && (first.is_ok() || singular_limit) {
            return Ok(events);
        }
    }
    let first = first?;
    if matches!(wire, Wire::Converse)
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
    {
        let events = converse::decode_metadata_events(&value);
        if events.len() >= 2 {
            return Ok(events);
        }
    }
    let Some(first) = first else {
        return Ok(Vec::new());
    };
    if matches!(wire, Wire::Gemini)
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
        && let Some(events) = fan_out_gemini_parts(&value, call_seq)
    {
        return Ok(events);
    }
    if let Some(expanded) = expand_complete_tool_call(wire, &first, raw)? {
        return Ok(expanded);
    }
    if matches!(wire, Wire::Messages)
        && let Ok(value) = serde_json::from_str::<Value>(&raw.data)
    {
        let name = frame_event_name(wire, raw);
        let usage = match name.as_str() {
            "message_start" => value.pointer("/message/usage"),
            "message_delta" => value.get("usage"),
            _ => None,
        }
        .filter(|v| v.is_object());
        let mut out = Vec::new();
        if name == "message_start"
            && matches!(first, IrStreamEvent::Usage { .. })
            && let Some(id) = value
                .pointer("/message/id")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
        {
            out.push(IrStreamEvent::Protocol {
                item_type: "messages_id".into(),
                payload: serde_json::json!(id),
            });
        }
        if let Some(usage) = usage
            && let Some(tier) = messages::service_tier_from_usage(usage)
        {
            out.push(IrStreamEvent::ServiceTier { tier });
        }
        if name == "message_delta"
            && let Some(text) = messages::stop_details_explanation(&value)
            && !matches!(first, IrStreamEvent::RefusalDelta { .. })
        {
            out.push(IrStreamEvent::RefusalDelta { text });
        }
        let add_delta_usage =
            name == "message_delta" && matches!(first, IrStreamEvent::FinishReason { .. });
        let keep_container = matches!(
            first,
            IrStreamEvent::Usage { .. } | IrStreamEvent::FinishReason { .. }
        );
        if keep_container {
            let container = if name == "message_start" {
                value.pointer("/message/container")
            } else if name == "message_delta" {
                value.get("container")
            } else {
                None
            };
            if let Some(container) = container.filter(|container| container.is_object()) {
                out.push(IrStreamEvent::Container {
                    value: container.clone(),
                });
            }
        }
        if add_delta_usage
            && let Some(text) = value
                .pointer("/delta/stop_sequence")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
        {
            out.push(IrStreamEvent::StopSequence {
                text: text.to_string(),
            });
        }
        if name == "message_delta"
            && matches!(
                first,
                IrStreamEvent::FinishReason { .. } | IrStreamEvent::Usage { .. }
            )
            && let Some(managed) = value.get("context_management").filter(|v| v.is_object())
        {
            out.push(IrStreamEvent::ContextManagement {
                value: managed.clone(),
            });
        }
        if name == "message_delta"
            && matches!(
                first,
                IrStreamEvent::FinishReason { .. } | IrStreamEvent::Usage { .. }
            )
            && let Some(reason) = value
                .pointer("/diagnostics/cache_miss_reason")
                .filter(|reason| reason.is_object())
        {
            out.push(IrStreamEvent::Diagnostics {
                cache_miss_reason: reason.clone(),
            });
        }
        out.push(first);
        if let Some(usage) = usage {
            out.extend(usage::messages_web_search_events(usage));
            out.extend(usage::messages_cache_creation_events(usage));
        }
        if add_delta_usage && let Some(usage) = usage {
            out.push(usage::from_anthropic(usage));
        }
        return Ok(out);
    }
    Ok(vec![first])
}

/// Walk every Gemini part. First-part-wins in `decode` would drop a later
/// `functionCall` after thought or text.
fn gemini_value_has_function_call(value: &Value) -> bool {
    value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(|p| p.get("functionCall").is_some()))
}

fn fan_out_gemini_parts(value: &Value, call_seq: &mut usize) -> Option<Vec<IrStreamEvent>> {
    let seq_at_entry = *call_seq;
    let mut out = Vec::new();
    if let Some(id) = value
        .get("responseId")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        out.push(IrStreamEvent::Protocol {
            item_type: "gemini_response_id".into(),
            payload: serde_json::json!(id),
        });
    }
    if let Some(parts) = value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
    {
        for part in parts {
            out.extend(gemini_part_events(part, call_seq));
        }
    }
    if let Some(chunks) = value
        .pointer("/candidates/0/groundingMetadata/groundingChunks")
        .and_then(Value::as_array)
    {
        let supports = value
            .pointer("/candidates/0/groundingMetadata/groundingSupports")
            .and_then(Value::as_array);
        for (idx, chunk) in chunks.iter().enumerate() {
            if let Some(mut annotation) = gemini::annotation_from_grounding_chunk(chunk) {
                if let Some(supports) = supports {
                    gemini::apply_grounding_support(&mut annotation, idx, supports);
                }
                out.push(IrStreamEvent::AnnotationAdded { annotation });
            }
        }
    }
    if let Some(candidate) = value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
    {
        for annotation in gemini::citation_annotations(candidate) {
            out.push(IrStreamEvent::AnnotationAdded { annotation });
        }
    }
    if let Some(metadata) = value
        .pointer("/candidates/0/urlContextMetadata")
        .filter(|metadata| {
            metadata
                .get("urlMetadata")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
        })
    {
        out.push(IrStreamEvent::Protocol {
            item_type: "gemini_url_context".into(),
            payload: metadata.clone(),
        });
    }
    if let Some(metadata) = value
        .pointer("/candidates/0/citationMetadata")
        .filter(|metadata| {
            let sources = metadata
                .get("citationSources")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty());
            let citations = metadata
                .get("citations")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty());
            sources || citations
        })
    {
        out.push(IrStreamEvent::Protocol {
            item_type: "gemini_citation_metadata".into(),
            payload: metadata.clone(),
        });
    }
    if let Some(attrs) = value
        .pointer("/candidates/0/groundingAttributions")
        .and_then(Value::as_array)
    {
        for attr in attrs {
            if let Some(annotation) = gemini::annotation_from_grounding_chunk(attr) {
                out.push(IrStreamEvent::AnnotationAdded { annotation });
            }
        }
    }
    if let Some(ev) = gemini::search_entry_from_value(value) {
        out.push(ev);
    }
    if let Some(content) = value
        .pointer("/candidates/0/logprobsResult")
        .and_then(gemini::logprobs_from_result)
    {
        out.push(IrStreamEvent::Logprobs { content });
    }
    if let Some(text) = value
        .pointer("/candidates/0/finishMessage")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::RefusalDelta {
            text: text.to_string(),
        });
    }
    if let Some(ratings) = value
        .pointer("/candidates/0/safetyRatings")
        .filter(|ratings| ratings.as_array().is_some_and(|items| !items.is_empty()))
    {
        out.push(IrStreamEvent::Protocol {
            item_type: "gemini_safety_ratings".into(),
            payload: ratings.clone(),
        });
    }
    if let Some(ratings) = value
        .pointer("/promptFeedback/safetyRatings")
        .filter(|ratings| ratings.as_array().is_some_and(|items| !items.is_empty()))
    {
        out.push(IrStreamEvent::Protocol {
            item_type: "gemini_prompt_safety".into(),
            payload: ratings.clone(),
        });
    }
    if let Some(message) = value
        .pointer("/promptFeedback/blockReasonMessage")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        out.push(IrStreamEvent::Protocol {
            item_type: "gemini_block_reason_message".into(),
            payload: Value::from(message),
        });
        if value
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
            .is_none_or(|reason| reason.is_empty())
            && let Some(reason) = value
                .pointer("/promptFeedback/blockReason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty())
        {
            out.push(IrStreamEvent::FinishReason {
                reason: gemini::map_block(reason),
                vendor: Some(reason.to_string()),
            });
        }
    }
    if let Some(score) = value
        .pointer("/candidates/0/avgLogprobs")
        .and_then(Value::as_f64)
        .filter(|score| score.is_finite())
    {
        out.push(IrStreamEvent::Protocol {
            item_type: "gemini_avg_logprobs".into(),
            payload: Value::from(score),
        });
    }
    if let Some(count) = value
        .pointer("/candidates/0/tokenCount")
        .and_then(Value::as_u64)
        .filter(|count| *count > 0)
        .and_then(|count| u32::try_from(count).ok())
    {
        out.push(IrStreamEvent::Protocol {
            item_type: "gemini_candidate_tokens".into(),
            payload: Value::from(count),
        });
    }
    if let Some(reason) = value
        .pointer("/candidates/0/finishReason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push(IrStreamEvent::FinishReason {
            reason: gemini::map_finish(reason, gemini_value_has_function_call(value)),
            vendor: Some(reason.to_string()),
        });
    }
    if let Some(ev) = gemini::service_tier_from_chunk(value) {
        out.push(ev);
    }
    out.extend(usage::gemini_traffic_type_events(value));
    out.extend(usage::gemini_tool_use_prompt_events(value));
    out.extend(usage::gemini_modality_detail_events(value));
    if let Some(ev) = gemini::usage_from_chunk(value) {
        out.push(ev);
    }
    let has_call = out
        .iter()
        .any(|ev| matches!(ev, IrStreamEvent::ToolCallStart { .. }));
    let has_safety = out.iter().any(|ev| {
        matches!(
            ev,
            IrStreamEvent::Protocol { item_type, .. } if item_type == "gemini_safety_ratings"
        )
    });
    let has_url_context = out.iter().any(|ev| {
        matches!(
            ev,
            IrStreamEvent::Protocol { item_type, .. } if item_type == "gemini_url_context"
        )
    });
    let has_citation = out.iter().any(|ev| {
        matches!(
            ev,
            IrStreamEvent::Protocol { item_type, .. } if item_type == "gemini_citation_metadata"
        )
    });
    let has_prompt_safety = out.iter().any(|ev| {
        matches!(
            ev,
            IrStreamEvent::Protocol { item_type, .. } if item_type == "gemini_prompt_safety"
        )
    });
    let has_avg_logprobs = out.iter().any(|ev| {
        matches!(
            ev,
            IrStreamEvent::Protocol { item_type, .. } if item_type == "gemini_avg_logprobs"
        )
    });
    let has_candidate_tokens = out.iter().any(|ev| {
        matches!(
            ev,
            IrStreamEvent::Protocol { item_type, .. } if item_type == "gemini_candidate_tokens"
        )
    });
    let has_modality_details = out.iter().any(|ev| {
        matches!(
            ev,
            IrStreamEvent::Protocol { item_type, .. } if item_type == "gemini_prompt_token_details"
                || item_type == "gemini_candidate_token_details"
        )
    });
    let only_search_entry = matches!(out.as_slice(), [IrStreamEvent::SearchEntryPoint { .. }]);
    if out.is_empty()
        || (out.len() < 2
            && !has_call
            && !only_search_entry
            && !has_safety
            && !has_url_context
            && !has_citation
            && !has_prompt_safety
            && !has_avg_logprobs
            && !has_candidate_tokens
            && !has_modality_details)
    {
        *call_seq = seq_at_entry;
        return None;
    }
    Some(out)
}

/// Why a client or proxy rejects a stream that produced frames and then ended.
#[cfg(any(feature = "client", feature = "proxy"))]
pub(crate) const INCOMPLETE_STREAM_MESSAGE: &str = "upstream stream ended before a terminal event";

/// Upstream frame that ends a stream. An empty body is not terminal.
///
/// Maps-only hosts call this on each SSE frame. EOF after content is a
/// failure unless one frame was terminal. [`StreamEncoder::finish`] is
/// for a stream the caller already knows completed.
pub fn frame_is_terminal(wire: Wire, raw: &RawSse, profile: &ResolvedProfile) -> bool {
    match wire {
        Wire::ChatCompletions => chat_frame_is_terminal(raw),
        Wire::Messages => messages_frame_is_terminal(raw),
        Wire::Responses => responses_frame_is_terminal(raw),
        Wire::Gemini => gemini_frame_is_terminal(raw),
        Wire::Converse => decode_stream_events(wire, raw, profile).is_ok_and(|events| {
            events
                .iter()
                .any(|ev| matches!(ev, IrStreamEvent::FinishReason { .. }))
        }),
        _ => false,
    }
}

/// True when any frame is terminal. An empty slice is not.
#[must_use]
pub fn stream_has_terminal(wire: Wire, frames: &[RawSse], profile: &ResolvedProfile) -> bool {
    frames
        .iter()
        .any(|raw| frame_is_terminal(wire, raw, profile))
}

fn chat_frame_is_terminal(raw: &RawSse) -> bool {
    if raw.event.as_deref() == Some("[DONE]") || raw.data.trim() == "[DONE]" {
        return true;
    }
    let Ok(value) = serde_json::from_str::<Value>(&raw.data) else {
        return false;
    };
    value
        .pointer("/choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                choice
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .is_some_and(|reason| !reason.is_empty())
            })
        })
}

fn messages_frame_is_terminal(raw: &RawSse) -> bool {
    let name = frame_event_name(Wire::Messages, raw);
    if name == "message_stop" {
        return true;
    }
    if name != "message_delta" {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(&raw.data) else {
        return false;
    };
    value
        .pointer("/delta/stop_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.is_empty())
}

fn responses_frame_is_terminal(raw: &RawSse) -> bool {
    matches!(
        frame_event_name(Wire::Responses, raw).as_str(),
        "response.completed" | "response.incomplete" | "response.failed"
    )
}

fn gemini_frame_is_terminal(raw: &RawSse) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(&raw.data) else {
        return false;
    };
    if value
        .pointer("/candidates/0/finishReason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.is_empty())
    {
        return true;
    }
    // A blocked prompt finishes without a candidate finishReason.
    value
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.is_empty())
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
        let seq = *call_seq;
        *call_seq += 1;
        let id = gemini::gemini_call_id(fc, &name, seq);
        let index = u32::try_from(seq).unwrap_or(0);
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
    } else if let Some(ev) = gemini::inline_data_event(part) {
        out.push(ev);
    } else if part.get("thought").and_then(Value::as_bool) != Some(true)
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
) -> Result<Option<Vec<IrStreamEvent>>, MapError> {
    let Ok(value) = serde_json::from_str::<Value>(&raw.data) else {
        return Ok(None);
    };
    match wire {
        Wire::ChatCompletions => expand_chat_tool_call(first, &value),
        Wire::Gemini => Ok(expand_gemini_function_call(first, &value)),
        Wire::Responses => Ok(expand_responses_function_call(first, &value)),
        Wire::Messages | Wire::Converse => Ok(None),
        _ => Ok(None),
    }
}

fn expand_chat_tool_call(
    first: &IrStreamEvent,
    value: &Value,
) -> Result<Option<Vec<IrStreamEvent>>, MapError> {
    let IrStreamEvent::Protocol { .. } = first else {
        return Ok(None);
    };
    let Some(tool_calls) = value
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for call in tool_calls {
        let expanded = chat::expand_tool_call(call, value)?;
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
        return Ok(None);
    }
    Ok(Some(out))
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
            id: gemini::gemini_call_id(fc, &name, 0),
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
/// Protocol names are dialect-specific (`chunk` is Chat and Gemini), so
/// a Chat Protocol must not re-emit on Gemini. A Messages protocol frame
/// is a same-wire event, such as a document citation, and stays on Messages.
#[cfg(feature = "proxy")]
#[must_use]
pub(crate) fn event_has_slot(wire: Wire, ev: &IrStreamEvent) -> bool {
    match ev {
        IrStreamEvent::Protocol { item_type, payload } => {
            (wire == Wire::Messages
                && (messages_protocol_reemits(item_type)
                    || item_type == "messages_id"
                    || item_type == "messages_web_search_requests"
                    || item_type == "messages_web_fetch_requests"
                    || item_type == "messages_cache_creation"
                    || item_type == "messages_citation"))
                || (wire == Wire::Responses
                    && (item_type == "responses_id" || responses_output_item(item_type, payload)))
                || (wire == Wire::ChatCompletions
                    && matches!(
                        item_type.as_str(),
                        "system_fingerprint"
                            | "chat_completion_id"
                            | "chat_audio_id"
                            | "chat_audio_expires"
                            | "chat_accepted_prediction_tokens"
                            | "chat_rejected_prediction_tokens"
                    ))
                || (wire == Wire::Gemini
                    && matches!(
                        item_type.as_str(),
                        "gemini_response_id"
                            | "gemini_safety_ratings"
                            | "gemini_url_context"
                            | "gemini_citation_metadata"
                            | "gemini_prompt_safety"
                            | "gemini_block_reason_message"
                            | "gemini_traffic_type"
                            | "gemini_tool_use_prompt_tokens"
                            | "gemini_avg_logprobs"
                            | "gemini_candidate_tokens"
                            | "gemini_prompt_token_details"
                            | "gemini_candidate_token_details"
                    ))
                || (wire == Wire::Converse
                    && matches!(
                        item_type.as_str(),
                        "additionalModelResponseFields"
                            | "metrics"
                            | "trace"
                            | "performanceConfig"
                            | "converse_frame"
                            | "converse_citation"
                            | "converse_redacted_content"
                    ))
        }
        IrStreamEvent::Unknown { .. } => matches!(wire, Wire::Messages | Wire::Responses),
        _ => true,
    }
}

#[cfg(feature = "proxy")]
fn messages_protocol_reemits(item_type: &str) -> bool {
    matches!(
        item_type,
        "message_start"
            | "content_block_start"
            | "content_block_delta"
            | "content_block_stop"
            | "message_delta"
            | "message_stop"
            | "ping"
    )
}

/// Encode one IR event into the target dialect's SSE shape.
pub fn encode_stream_event(wire: Wire, ev: &IrStreamEvent) -> Result<RawSse, MapError> {
    match ev {
        IrStreamEvent::Unknown { event, raw } => Ok(encode_named(event, raw)),
        IrStreamEvent::Protocol { item_type, payload } => {
            if wire == Wire::Responses && responses_output_item(item_type, payload) {
                return Ok(responses_output_item_frame(payload));
            }
            if wire == Wire::ChatCompletions
                && matches!(
                    item_type.as_str(),
                    "system_fingerprint" | "chat_audio_id" | "chat_audio_expires"
                )
            {
                return chat::encode(ev);
            }
            if wire == Wire::Converse && item_type == "converse_frame" {
                return Ok(RawSse {
                    event: None,
                    data: payload.to_string(),
                });
            }
            if wire == Wire::Converse && item_type == "converse_citation" {
                return Ok(RawSse {
                    event: None,
                    data: serde_json::json!({
                        "contentBlockDelta": {
                            "contentBlockIndex": 0,
                            "delta": { "citation": payload }
                        }
                    })
                    .to_string(),
                });
            }
            Ok(encode_named(item_type, payload))
        }
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

fn responses_output_item(item_type: &str, payload: &Value) -> bool {
    if item_type == "chunk" || item_type == "output_image" {
        return false;
    }
    payload.get("type").and_then(Value::as_str) == Some(item_type)
}

fn responses_output_item_frame(payload: &Value) -> RawSse {
    let index = payload
        .get("output_index")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut item = payload.clone();
    if let Some(obj) = item.as_object_mut() {
        obj.remove("output_index");
    }
    RawSse {
        event: Some("response.output_item.done".into()),
        data: serde_json::json!({
            "type": "response.output_item.done",
            "output_index": index,
            "item": item
        })
        .to_string(),
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
    wire: Wire,
    profile: &ResolvedProfile,
    name: &str,
    data: &str,
) -> Result<Option<IrStreamEvent>, MapError> {
    match profile.dialect.stream_unknown_policy {
        StreamUnknownPolicy::HardError => {
            // A Responses status frame repeats ids. Skip it. A nonempty
            // delta, arguments, code, command, text, or image is the only
            // copy of that content, so it still fails.
            if wire == Wire::Responses && !responses_unknown_carries_payload(data) {
                return Ok(None);
            }
            Err(MapError::HardError {
                path: name.to_string(),
                detail: format!(
                    "unknown stream event `{name}` (stream_unknown_policy = hard-error|passthrough, or add `{name}` to stream_events)"
                ),
            })
        }
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

/// Top-level payload only. `response.queued` nests a full Response
/// under `response`; walking that object would treat the snapshot as
/// new text.
fn responses_unknown_carries_payload(data: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return true;
    };
    let Some(obj) = value.as_object() else {
        return value.as_str().is_some_and(|text| !text.trim().is_empty());
    };
    const KEYS: &[&str] = &[
        "delta",
        "arguments",
        "code",
        "command",
        "text",
        "input",
        "refusal",
        "partial_image_b64",
        "output",
    ];
    KEYS.iter()
        .any(|key| obj.get(*key).is_some_and(responses_payload_value_present))
}

fn responses_payload_value_present(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => items.iter().any(responses_payload_value_present),
        Value::Object(map) => map.values().any(responses_payload_value_present),
        _ => false,
    }
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

/// Chat-shaped SSE `data` that is only an `error` object.
///
/// `choices` or `delta` means a normal chunk, even if `error` is also set.
#[cfg(any(feature = "client", feature = "proxy"))]
pub(crate) fn sse_wrapped_error_message(data: &str) -> Option<String> {
    let value: Value = serde_json::from_str(data).ok()?;
    if value.get("choices").is_some() || value.get("delta").is_some() {
        return None;
    }
    // Responses `event: error` is a flat object. Other wires nest `error`.
    let error = if let Some(error) = value.get("error").filter(|v| v.is_object()) {
        error
    } else if value.get("type").and_then(Value::as_str) == Some("error") {
        &value
    } else {
        return None;
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    let code = ["type", "code", "status"].iter().find_map(|key| {
        let text = error
            .get(*key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())?;
        if *key == "type" && text == "error" {
            return None;
        }
        Some(text.to_string())
    });
    match (code, message) {
        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
        (None, Some(message)) => Some(message.to_string()),
        _ => Some(data.to_string()),
    }
}

/// String, object, or array. Objects and arrays become compact JSON.
/// Empty string is absent. Anything else is an error, not a silent drop.
fn json_text_field(value: &Value, key: &str) -> Result<Option<String>, MapError> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(text @ (Value::Object(_) | Value::Array(_))) => Ok(Some(text.to_string())),
        Some(_) => Err(MapError::Invalid(format!(
            "{key} must be a string, object, or array"
        ))),
    }
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
            "{kind} index must be an unsigned integer"
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
        )
        .expect("parse short SSE document");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("ping"));
        assert_eq!(frames[0].data, r#"{"type":"ping"}"#);
        assert_eq!(frames[1].event, None);
        assert_eq!(frames[1].data, "[DONE]");
    }

    #[test]
    fn parse_all_errors_when_line_exceeds_sse_cap() {
        let frames = RawSse::parse_all("data: hi\n\n").expect("parse one SSE frame");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hi");

        let oversized = "a".repeat(MAX_SSE_PENDING + 1);
        let err = RawSse::parse_all(&oversized).expect_err("oversized SSE line");
        assert!(err.contains("exceeds"), "{err}");
    }

    #[test]
    fn parse_all_errors_when_joined_data_exceeds_sse_cap() {
        let half = "a".repeat(MAX_SSE_PENDING / 2);
        let doc = format!("data: {half}\ndata: {half}\n\n");
        let err = RawSse::parse_all(&doc).expect_err("joined data over the cap");
        assert!(err.contains("SSE data exceeds"), "{err}");
    }

    fn profile() -> ResolvedProfile {
        wiremux_auth::parse_profile_str(
            r#"
schema_version = 1
id = "t"
wire = "chat-completions"
base_url = "http://127.0.0.1"
"#,
        )
        .expect("profile")
    }

    fn frame(event: Option<&str>, data: &str) -> RawSse {
        RawSse {
            event: event.map(str::to_string),
            data: data.to_string(),
        }
    }

    #[test]
    fn chat_tool_stream_without_done_is_not_terminal() {
        let profile = profile();
        let frames = [frame(
            None,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"a","arguments":""}}]}}]}"#,
        )];
        assert!(!stream_has_terminal(Wire::ChatCompletions, &[], &profile));
        assert!(!frame_is_terminal(
            Wire::ChatCompletions,
            &frames[0],
            &profile
        ));
        assert!(!stream_has_terminal(
            Wire::ChatCompletions,
            &frames,
            &profile
        ));
        let done = frame(None, "[DONE]");
        assert!(frame_is_terminal(Wire::ChatCompletions, &done, &profile));
        assert!(stream_has_terminal(
            Wire::ChatCompletions,
            &[frames[0].clone(), done],
            &profile
        ));
    }

    #[test]
    fn messages_content_block_stop_is_not_terminal() {
        let profile = profile();
        let stop_block = frame(
            Some("content_block_stop"),
            r#"{"type":"content_block_stop","index":0}"#,
        );
        assert!(!frame_is_terminal(Wire::Messages, &stop_block, &profile));
        let message_stop = frame(Some("message_stop"), r#"{"type":"message_stop"}"#);
        assert!(frame_is_terminal(Wire::Messages, &message_stop, &profile));
    }

    #[test]
    fn wrapped_complete_json_with_finish_reason_is_terminal() {
        let profile = profile();
        let body = frame(
            None,
            r#"{"choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#,
        );
        assert!(frame_is_terminal(Wire::ChatCompletions, &body, &profile));
    }

    #[test]
    fn gemini_visible_text_keeps_thought_signature() {
        let part = serde_json::json!({
            "text": "answer",
            "thoughtSignature": "sig"
        });
        let mut seq = 0;
        let events = gemini_part_events(&part, &mut seq);
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "answer")),
            "{events:?}"
        );
        assert!(
            events.iter().any(
                |ev| matches!(ev, IrStreamEvent::ReasoningSignature { signature } if signature == "sig")
            ),
            "{events:?}"
        );
    }

    #[cfg(any(feature = "client", feature = "proxy"))]
    #[test]
    fn sse_wrapped_error_message_extracts_error_object() {
        let msg = sse_wrapped_error_message(
            r#"{"error":{"message":"upstream failed","type":"server_error"}}"#,
        );
        assert_eq!(msg.as_deref(), Some("server_error: upstream failed"));
    }

    #[cfg(any(feature = "client", feature = "proxy"))]
    #[test]
    fn sse_wrapped_error_message_reads_responses_error_event() {
        let raw = r#"{"type":"error","code":"server_error","message":"The server had an error","param":null}"#;
        assert_eq!(
            sse_wrapped_error_message(raw).as_deref(),
            Some("server_error: The server had an error")
        );
    }

    #[test]
    fn responses_error_event_keeps_vendor_message() {
        let profile = wiremux_auth::parse_profile_str(
            r#"
schema_version = 1
id = "t"
wire = "responses"
base_url = "http://127.0.0.1"
"#,
        )
        .expect("profile");
        let raw = frame(
            Some("error"),
            r#"{"type":"error","code":"server_error","message":"The server had an error","param":null}"#,
        );
        let err = decode_stream_event(Wire::Responses, &raw, &profile)
            .expect_err("responses error event");
        let msg = err.to_string();
        assert!(
            msg.contains("The server had an error"),
            "vendor message must survive, got {msg}"
        );
        assert!(
            !msg.contains("unknown stream event"),
            "error is a known Responses event, got {msg}"
        );
    }

    #[test]
    fn responses_done_events_do_not_fail_or_repeat_deltas() {
        let profile = wiremux_auth::parse_profile_str(
            r#"
schema_version = 1
id = "t"
wire = "responses"
base_url = "http://127.0.0.1"
"#,
        )
        .expect("profile");
        let cases = [
            (
                "response.output_text.done",
                r#"{"type":"response.output_text.done","text":"Hi"}"#,
            ),
            (
                "response.content_part.done",
                r#"{"type":"response.content_part.done"}"#,
            ),
            (
                "response.function_call_arguments.done",
                r#"{"type":"response.function_call_arguments.done","arguments":"{}"}"#,
            ),
            (
                "response.refusal.done",
                r#"{"type":"response.refusal.done","refusal":"no"}"#,
            ),
            ("response.audio.done", r#"{"type":"response.audio.done"}"#),
            (
                "response.audio.transcript.done",
                r#"{"type":"response.audio.transcript.done","transcript":"Hi"}"#,
            ),
        ];
        for (event, data) in cases {
            let raw = frame(Some(event), data);
            let events = decode_stream_events(Wire::Responses, &raw, &profile)
                .unwrap_or_else(|err| panic!("{event} is part of a normal stream, got {err}"));
            assert!(
                events.iter().all(|ev| !matches!(
                    ev,
                    IrStreamEvent::TextDelta { .. }
                        | IrStreamEvent::RefusalDelta { .. }
                        | IrStreamEvent::ToolCallArgDelta { .. }
                        | IrStreamEvent::AudioDelta { .. }
                        | IrStreamEvent::AudioTranscriptDelta { .. }
                )),
                "{event} must not repeat the delta, got {events:?}"
            );
        }
    }

    #[test]
    fn responses_reasoning_and_custom_tool_events_keep_deltas() {
        let profile = wiremux_auth::parse_profile_str(
            r#"
schema_version = 1
id = "t"
wire = "responses"
base_url = "http://127.0.0.1"
"#,
        )
        .expect("profile");
        let think = decode_stream_events(
            Wire::Responses,
            &frame(
                Some("response.reasoning_text.delta"),
                r#"{"type":"response.reasoning_text.delta","delta":"think"}"#,
            ),
            &profile,
        )
        .expect("reasoning text delta is a normal event");
        assert!(
            think.iter().any(|ev| matches!(
                ev,
                IrStreamEvent::ReasoningDelta { text } if text == "think"
            )),
            "{think:?}"
        );
        let quiet = [
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_part.done",
            "response.reasoning_summary_text.done",
            "response.reasoning_text.done",
            "response.custom_tool_call_input.done",
        ];
        for event in quiet {
            let events = decode_stream_events(
                Wire::Responses,
                &frame(
                    Some(event),
                    &format!(r#"{{"type":"{event}","text":"full","input":"full"}}"#),
                ),
                &profile,
            )
            .unwrap_or_else(|err| panic!("{event} follows a delta we already accept, got {err}"));
            assert!(
                events.iter().all(|ev| !matches!(
                    ev,
                    IrStreamEvent::ReasoningDelta { .. }
                        | IrStreamEvent::CustomToolCallInputDelta { .. }
                )),
                "{event} must not repeat the delta, got {events:?}"
            );
        }
    }

    #[test]
    fn responses_hosted_tool_progress_is_protocol() {
        let profile = wiremux_auth::parse_profile_str(
            r#"
schema_version = 1
id = "t"
wire = "responses"
base_url = "http://127.0.0.1"
"#,
        )
        .expect("profile");
        let names = [
            "response.web_search_call.in_progress",
            "response.web_search_call.searching",
            "response.web_search_call.completed",
            "response.file_search_call.in_progress",
            "response.file_search_call.searching",
            "response.file_search_call.completed",
            "response.code_interpreter_call.in_progress",
            "response.code_interpreter_call.interpreting",
            "response.code_interpreter_call.completed",
            "response.image_generation_call.in_progress",
            "response.image_generation_call.generating",
            "response.image_generation_call.completed",
            "response.mcp_call.in_progress",
            "response.mcp_call.completed",
            "response.mcp_call.failed",
            "response.mcp_list_tools.in_progress",
            "response.mcp_list_tools.completed",
            "response.mcp_list_tools.failed",
        ];
        for event in names {
            let events = decode_stream_events(
                Wire::Responses,
                &frame(
                    Some(event),
                    &format!(r#"{{"type":"{event}","item_id":"id_1"}}"#),
                ),
                &profile,
            )
            .unwrap_or_else(|err| panic!("{event} must not abort the stream, got {err}"));
            assert!(
                events.iter().any(|ev| matches!(
                    ev,
                    IrStreamEvent::Protocol { item_type, .. } if item_type == event
                )),
                "{event} must stay protocol, got {events:?}"
            );
        }
    }

    #[test]
    fn responses_status_only_unknown_is_skipped_on_hard_error() {
        let profile =
            wiremux_auth::parse_profile_str(include_str!("../../../../presets/openai-codex.toml"))
                .expect("openai-codex");
        let text = decode_stream_events(
            Wire::Responses,
            &frame(
                Some("response.output_text.delta"),
                r#"{"type":"response.output_text.delta","delta":"Hi"}"#,
            ),
            &profile,
        )
        .expect("text delta");
        assert!(
            text.iter()
                .any(|ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "Hi")),
            "text delta must be kept, got {text:?}"
        );
        let quiet = [
            (
                "response.queued",
                r#"{"type":"response.queued","response":{"id":"resp_1","output":[{"type":"message","content":[{"type":"output_text","text":"already sent"}]}]}}"#,
            ),
            (
                "response.compaction.compacting",
                r#"{"type":"response.compaction.compacting","sequence_number":0,"output_index":0,"item_id":"item_1"}"#,
            ),
            (
                "response.shell_call_command.added",
                r#"{"type":"response.shell_call_command.added","item_id":"sh_1","sequence_number":1}"#,
            ),
        ];
        for (event, data) in quiet {
            let events = decode_stream_events(Wire::Responses, &frame(Some(event), data), &profile)
                .unwrap_or_else(|err| panic!("{event} is status only, got {err}"));
            assert!(
                events.is_empty(),
                "{event} must not emit text or a second delta, got {events:?}"
            );
        }
        let searching = decode_stream_events(
            Wire::Responses,
            &frame(
                Some("response.web_search_call.searching"),
                r#"{"type":"response.web_search_call.searching","item_id":"ws_1"}"#,
            ),
            &profile,
        )
        .expect("searching");
        assert!(
            searching
                .iter()
                .all(|ev| !matches!(ev, IrStreamEvent::TextDelta { .. })),
            "searching must not become text, got {searching:?}"
        );
        let completed = decode_stream_events(
            Wire::Responses,
            &frame(
                Some("response.completed"),
                r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed"}}"#,
            ),
            &profile,
        )
        .expect("completed");
        assert!(
            completed
                .iter()
                .all(|ev| !matches!(ev, IrStreamEvent::TextDelta { .. })),
            "completed must not repeat the text delta, got {completed:?}"
        );

        let payload = [
            (
                "response.mcp_call_arguments.delta",
                r#"{"type":"response.mcp_call_arguments.delta","delta":"{"}"#,
            ),
            (
                "response.mcp_call_arguments.done",
                r#"{"type":"response.mcp_call_arguments.done","arguments":"{}"}"#,
            ),
            (
                "response.code_interpreter_call_code.delta",
                r#"{"type":"response.code_interpreter_call_code.delta","delta":"print(1)"}"#,
            ),
            (
                "response.code_interpreter_call_code.done",
                r#"{"type":"response.code_interpreter_call_code.done","code":"print(1)"}"#,
            ),
            (
                "response.image_generation_call.partial_image",
                r#"{"type":"response.image_generation_call.partial_image","partial_image_b64":"abcd"}"#,
            ),
            (
                "response.shell_call_command.delta",
                r#"{"type":"response.shell_call_command.delta","delta":"ls"}"#,
            ),
            (
                "response.shell_call_command.added",
                r#"{"type":"response.shell_call_command.added","command":"ls"}"#,
            ),
            (
                "response.shell_call_output_content.delta",
                r#"{"type":"response.shell_call_output_content.delta","delta":{"stdout":"ok","stderr":""}}"#,
            ),
            (
                "response.shell_call_output_content.done",
                r#"{"type":"response.shell_call_output_content.done","output":[{"stdout":"ok","stderr":""}]}"#,
            ),
        ];
        for (event, data) in payload {
            let err = decode_stream_events(Wire::Responses, &frame(Some(event), data), &profile)
                .expect_err(event);
            let crate::map::MapError::HardError { detail, .. } = err else {
                panic!("{event} must stay a hard-error, got {err}");
            };
            assert!(
                detail.contains(event),
                "{event} detail must name the event, got {detail}"
            );
        }

        let pass = wiremux_auth::parse_profile_str(
            r#"
schema_version = 1
id = "t"
wire = "responses"
stream_unknown_policy = "passthrough"
base_url = "http://127.0.0.1"
"#,
        )
        .expect("passthrough");
        let events = decode_stream_events(
            Wire::Responses,
            &frame(
                Some("response.queued"),
                r#"{"type":"response.queued","sequence_number":1}"#,
            ),
            &pass,
        )
        .expect("passthrough queued");
        assert!(
            events.iter().any(|ev| matches!(
                ev,
                IrStreamEvent::Unknown { event, .. } if event == "response.queued"
            )),
            "passthrough must forward queued, got {events:?}"
        );
    }

    #[cfg(any(feature = "client", feature = "proxy"))]
    #[test]
    fn sse_wrapped_error_message_ignores_error_when_choices_present() {
        let msg = sse_wrapped_error_message(r#"{"choices":[],"error":{"message":"nope"}}"#);
        assert!(msg.is_none(), "{msg:?}");
    }

    #[cfg(any(feature = "client", feature = "proxy"))]
    #[test]
    fn sse_wrapped_error_message_keeps_code_only_payload() {
        let raw = r#"{"error":{"code":"server_error"}}"#;
        assert_eq!(sse_wrapped_error_message(raw).as_deref(), Some(raw));
    }

    #[cfg(any(feature = "client", feature = "proxy"))]
    #[test]
    fn sse_wrapped_error_message_ignores_normal_chat_chunk() {
        let msg =
            sse_wrapped_error_message(r#"{"choices":[{"delta":{"content":"hi"},"index":0}]}"#);
        assert!(msg.is_none(), "{msg:?}");
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

    #[cfg(feature = "proxy")]
    #[test]
    fn event_has_slot_keeps_responses_web_search_call() {
        let ev = IrStreamEvent::Protocol {
            item_type: "web_search_call".into(),
            payload: serde_json::json!({"type": "web_search_call", "id": "ws_1"}),
        };
        assert!(event_has_slot(Wire::Responses, &ev));
    }

    #[cfg(feature = "proxy")]
    #[test]
    fn event_has_slot_keeps_converse_document_frame() {
        let ev = IrStreamEvent::Protocol {
            item_type: "converse_frame".into(),
            payload: serde_json::json!({ "contentBlockDelta": {} }),
        };
        assert!(event_has_slot(Wire::Converse, &ev));
        assert!(!event_has_slot(Wire::ChatCompletions, &ev));
        let citation = IrStreamEvent::Protocol {
            item_type: "converse_citation".into(),
            payload: serde_json::json!({ "location": { "documentChar": { "documentIndex": 0 } } }),
        };
        assert!(event_has_slot(Wire::Converse, &citation));
        assert!(!event_has_slot(Wire::Messages, &citation));
    }

    #[test]
    fn check_index_rejects_non_integer_without_saying_over_cap() {
        let err = check_index(
            &serde_json::json!({"index": "nope"}),
            "index",
            4,
            "tool call",
        )
        .expect_err("string index");
        let msg = err.to_string();
        assert!(msg.contains("must be an unsigned integer"), "{msg}");
        assert!(!msg.contains("exceeds cap"), "{msg}");

        let err = check_index(&serde_json::json!({"index": 99}), "index", 4, "tool call")
            .expect_err("index over cap");
        assert!(err.to_string().contains("exceeds cap"), "{err}");
    }

    #[test]
    fn messages_document_citation_is_not_dropped() {
        let profile = wiremux_auth::parse_profile_str(
            r#"
schema_version = 1
id = "t"
wire = "messages"
base_url = "http://127.0.0.1"
"#,
        )
        .expect("profile");
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"char_location","cited_text":"The grass is green.","document_index":0,"document_title":"My Document","start_char_index":0,"end_char_index":20}}}"#;
        let events = decode_stream_events(
            Wire::Messages,
            &frame(Some("content_block_delta"), data),
            &profile,
        )
        .expect("char_location");
        assert!(
            events.iter().any(|ev| match ev {
                IrStreamEvent::Protocol { payload, .. } =>
                    payload.to_string().contains("The grass is green."),
                IrStreamEvent::AnnotationAdded { annotation } => {
                    annotation.to_string().contains("The grass is green.")
                        && !annotation
                            .to_string()
                            .contains("web_search_result_location")
                }
                _ => false,
            }),
            "document citation must be kept, got {events:?}"
        );
    }
}

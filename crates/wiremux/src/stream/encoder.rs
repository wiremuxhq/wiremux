//! Stateful SSE encoder. One instance per output stream.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde_json::{Value, json};
use wiremux_auth::Wire;

use crate::ir::IrStreamEvent;
use crate::map::MapError;

use super::messages::encode_stop_reason;
use super::usage;
use super::{RawSse, encode_stream_event};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockKind {
    Text,
    Thinking,
    Tool,
    CustomTool,
}

/// Accumulates IR events into dialect-correct SSE frames.
pub struct StreamEncoder {
    wire: Wire,
    model: String,
    started: bool,
    finished: bool,
    next_block: u32,
    open: Option<(u32, BlockKind)>,
    finish: Option<String>,
    usage: Option<(u32, u32, u32, u32, u32, u32, u32)>,
    used_tool: HashSet<u32>,
    saw_custom_tool: bool,
    next_tool: u32,
    tool_slots: HashMap<u32, VecDeque<u32>>,
    tool_got_arg: HashSet<u32>,
    last_tool: HashMap<u32, u32>,
    tool_items: HashMap<u32, (String, String, String)>,
    text_items: HashMap<u32, String>,
    text_annotations: HashMap<u32, Vec<Value>>,
    text_logprobs: HashMap<u32, Vec<Value>>,
    refusal_items: HashMap<u32, String>,
    reasoning_items: HashMap<u32, String>,
    created_at: Option<i64>,
    service_tier: Option<String>,
    metadata: Option<BTreeMap<String, String>>,
    moderation: Option<(Option<Value>, Option<Value>)>,
    refusal: String,
    /// Tool starts that arrived while another tool block was still open.
    /// Flushed when that block closes, in content-block index order.
    held_tools: BTreeMap<u32, (u32, String, String, String)>,
    /// Non-tool events that arrived while a tool block was still open.
    deferred: Vec<IrStreamEvent>,
    closed_tools: HashSet<u32>,
}

impl StreamEncoder {
    /// Encoder for `wire` client frames.
    #[must_use]
    pub fn new(wire: Wire) -> Self {
        Self {
            wire,
            model: String::new(),
            started: false,
            finished: false,
            next_block: 0,
            open: None,
            finish: None,
            usage: None,
            used_tool: HashSet::new(),
            saw_custom_tool: false,
            next_tool: 0,
            tool_slots: HashMap::new(),
            tool_got_arg: HashSet::new(),
            last_tool: HashMap::new(),
            tool_items: HashMap::new(),
            text_items: HashMap::new(),
            text_annotations: HashMap::new(),
            text_logprobs: HashMap::new(),
            refusal_items: HashMap::new(),
            reasoning_items: HashMap::new(),
            created_at: None,
            service_tier: None,
            metadata: None,
            moderation: None,
            refusal: String::new(),
            held_tools: BTreeMap::new(),
            deferred: Vec::new(),
            closed_tools: HashSet::new(),
        }
    }

    /// Dest request model for Chat, Messages, Gemini, and Responses encode.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Encode one IR event. May emit opening or close frames first.
    pub fn push(&mut self, ev: IrStreamEvent) -> Result<Vec<RawSse>, MapError> {
        if self.finished {
            return Ok(Vec::new());
        }
        match ev {
            IrStreamEvent::Protocol { .. } | IrStreamEvent::Unknown { .. } => {
                Ok(vec![encode_stream_event(self.wire, &ev)?])
            }
            IrStreamEvent::Done => self.finish(),
            other => match self.wire {
                Wire::Messages => self.push_messages(other),
                Wire::Responses => self.push_responses(other),
                Wire::ChatCompletions => self.push_chat(other),
                Wire::Converse => self.push_converse(other),
                Wire::Gemini => {
                    let frame = encode_stream_event(self.wire, &other)?;
                    Ok(vec![self.attach_dest_model(frame)])
                }
                _ => Ok(vec![encode_stream_event(self.wire, &other)?]),
            },
        }
    }

    /// Close open blocks and emit the terminal frames.
    pub fn finish(&mut self) -> Result<Vec<RawSse>, MapError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        match self.wire {
            Wire::Messages => Ok(self.finish_messages()),
            Wire::Responses => Ok(self.finish_responses()),
            Wire::ChatCompletions => Ok(self.finish_chat()),
            Wire::Gemini => {
                encode_stream_event(self.wire, &IrStreamEvent::Done).map(|frame| vec![frame])
            }
            Wire::Converse => self.finish_converse(),
            _ => Ok(Vec::new()),
        }
    }

    fn attach_dest_model(&self, frame: RawSse) -> RawSse {
        self.attach_dest_model_key(frame, "modelVersion")
    }

    fn attach_chat_dest_model(&self, frame: RawSse) -> RawSse {
        self.attach_dest_model_key(frame, "model")
    }

    fn attach_dest_model_key(&self, frame: RawSse, key: &str) -> RawSse {
        if self.model.is_empty() || frame.data.trim() == "[DONE]" {
            return frame;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(&frame.data) else {
            return frame;
        };
        let Value::Object(obj) = &mut value else {
            return frame;
        };
        obj.insert(key.into(), json!(self.model.clone()));
        RawSse {
            event: frame.event,
            data: value.to_string(),
        }
    }

    fn alloc_tool(&mut self, ir_index: u32) -> u32 {
        let enc = if matches!(self.wire, Wire::ChatCompletions) {
            if !self.used_tool.contains(&ir_index) {
                ir_index
            } else {
                let mut n = self.next_tool;
                while self.used_tool.contains(&n) {
                    n = n.saturating_add(1);
                }
                n
            }
        } else {
            let n = self.next_block;
            self.next_block = n.saturating_add(1);
            n
        };
        self.used_tool.insert(enc);
        self.next_tool = self.next_tool.max(enc.saturating_add(1));
        let slots = self.tool_slots.entry(ir_index).or_default();
        let replace = slots
            .back()
            .is_some_and(|prev| self.tool_got_arg.contains(prev));
        if replace {
            slots.clear();
        }
        slots.push_back(enc);
        self.last_tool.insert(ir_index, enc);
        enc
    }

    fn tool_enc(&mut self, ir_index: u32) -> u32 {
        let slots = self.tool_slots.entry(ir_index).or_default();
        let enc = if let Some(&front) = slots.front() {
            front
        } else if let Some(&last) = self.last_tool.get(&ir_index) {
            last
        } else {
            let enc = self.alloc_tool(ir_index);
            return enc;
        };
        self.tool_got_arg.insert(enc);
        if slots.len() > 1 {
            slots.pop_front();
        }
        enc
    }

    fn push_messages(&mut self, ev: IrStreamEvent) -> Result<Vec<RawSse>, MapError> {
        if let IrStreamEvent::ServiceTier { ref tier } = ev {
            self.service_tier = Some(tier.clone());
        }
        let mut out = Vec::new();
        if !self.started {
            if matches!(ev, IrStreamEvent::ServiceTier { .. }) {
                return Ok(out);
            }
            self.started = true;
            out.push(self.messages_start_frame());
        }
        match ev {
            IrStreamEvent::TextDelta { text } => {
                if self.tool_block_open() {
                    self.deferred.push(IrStreamEvent::TextDelta { text });
                } else {
                    out.extend(self.emit_text(&text));
                }
            }
            IrStreamEvent::ReasoningDelta { text } => {
                if self.tool_block_open() {
                    self.deferred.push(IrStreamEvent::ReasoningDelta { text });
                } else {
                    out.extend(self.ensure_block(BlockKind::Thinking));
                    let index = self.open.map(|(i, _)| i).unwrap_or(0);
                    out.push(named(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "thinking_delta", "thinking": text }
                        }),
                    ));
                }
            }
            IrStreamEvent::ReasoningSignature { signature } => {
                if self.tool_block_open() {
                    self.deferred
                        .push(IrStreamEvent::ReasoningSignature { signature });
                } else {
                    out.extend(self.ensure_block(BlockKind::Thinking));
                    let index = self.open.map(|(i, _)| i).unwrap_or(0);
                    out.push(named(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "signature_delta", "signature": signature }
                        }),
                    ));
                }
            }
            IrStreamEvent::ToolCallStart {
                id, name, index, ..
            } => {
                let open_same = self.open.is_some_and(|(enc, kind)| {
                    kind == BlockKind::Tool && self.last_tool.get(&index) == Some(&enc)
                });
                if open_same {
                    // A later chunk repeated this call's id or name.
                } else if let Some(held) = self.held_tools.get_mut(&index) {
                    if held.1.is_empty() {
                        held.1 = id;
                    }
                    if held.2.is_empty() {
                        held.2 = name;
                    }
                } else {
                    let enc = self.alloc_tool(index);
                    let other_tool_open = self
                        .open
                        .is_some_and(|(open_enc, kind)| kind == BlockKind::Tool && open_enc != enc);
                    if other_tool_open {
                        self.held_tools
                            .insert(index, (enc, id, name, String::new()));
                    } else {
                        out.extend(self.close_open());
                        out.push(self.tool_start_frame(enc, &id, &name));
                        self.open = Some((enc, BlockKind::Tool));
                    }
                }
            }
            IrStreamEvent::ToolCallArgDelta { delta, index } => {
                let enc = self.tool_enc(index);
                if self.open == Some((enc, BlockKind::Tool)) {
                    out.push(self.tool_arg_frame(enc, &delta));
                } else if let Some(held) = self.held_tools.get_mut(&index) {
                    held.3.push_str(&delta);
                } else if !self.closed_tools.contains(&enc) {
                    out.push(self.tool_arg_frame(enc, &delta));
                }
            }
            IrStreamEvent::ToolCallEnd => {}
            IrStreamEvent::FinishReason { reason } => {
                self.finish = Some(reason);
            }
            IrStreamEvent::RefusalDelta { text } => {
                self.refusal.push_str(&text);
            }
            IrStreamEvent::AudioDelta { .. } => {}
            IrStreamEvent::Logprobs { .. } => {}
            IrStreamEvent::Created { .. }
            | IrStreamEvent::ServiceTier { .. }
            | IrStreamEvent::Metadata { .. }
            | IrStreamEvent::Moderation { .. } => {}
            IrStreamEvent::AudioTranscriptDelta { text } => {
                if self.tool_block_open() {
                    self.deferred
                        .push(IrStreamEvent::AudioTranscriptDelta { text });
                } else {
                    out.extend(self.emit_text(&text));
                }
            }
            IrStreamEvent::AnnotationAdded { annotation } => {
                if self.tool_block_open() {
                    self.deferred
                        .push(IrStreamEvent::AnnotationAdded { annotation });
                } else {
                    out.extend(self.ensure_block(BlockKind::Text));
                    let index = self.open.map(|(i, _)| i).unwrap_or(0);
                    out.push(named(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {
                                "type": "citations_delta",
                                "citation": super::messages::citation_from_annotation(&annotation)
                            }
                        }),
                    ));
                }
            }
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                audio_tokens,
                completion_audio_tokens,
            } => {
                self.usage = Some((
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
                    audio_tokens,
                    completion_audio_tokens,
                ));
            }
            other => out.push(encode_stream_event(Wire::Messages, &other)?),
        }
        Ok(out)
    }

    fn ensure_block(&mut self, kind: BlockKind) -> Vec<RawSse> {
        if self.open.is_some_and(|(_, k)| k == kind) {
            return Vec::new();
        }
        let mut out = self.close_open();
        let index = self.next_block;
        self.next_block = self.next_block.saturating_add(1);
        let content_block = match kind {
            BlockKind::Text => json!({ "type": "text", "text": "" }),
            BlockKind::Thinking => json!({ "type": "thinking", "thinking": "" }),
            BlockKind::Tool | BlockKind::CustomTool => {
                json!({ "type": "tool_use", "id": "", "name": "", "input": {} })
            }
        };
        out.push(named(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": content_block
            }),
        ));
        self.open = Some((index, kind));
        out
    }

    fn tool_start_frame(&self, enc: u32, id: &str, name: &str) -> RawSse {
        named(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": enc,
                "content_block": {
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": {}
                }
            }),
        )
    }

    fn tool_arg_frame(&self, enc: u32, delta: &str) -> RawSse {
        named(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": enc,
                "delta": { "type": "input_json_delta", "partial_json": delta }
            }),
        )
    }

    fn tool_block_open(&self) -> bool {
        self.open.is_some_and(|(_, kind)| kind == BlockKind::Tool) || !self.held_tools.is_empty()
    }

    fn emit_text(&mut self, text: &str) -> Vec<RawSse> {
        let mut out = self.ensure_block(BlockKind::Text);
        let index = self.open.map(|(i, _)| i).unwrap_or(0);
        out.push(named(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            }),
        ));
        out
    }

    fn close_open(&mut self) -> Vec<RawSse> {
        let Some((index, _)) = self.open.take() else {
            return self.flush_held_tools();
        };
        self.closed_tools.insert(index);
        let mut out = vec![named(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": index }),
        )];
        out.extend(self.flush_held_tools());
        out
    }

    fn messages_start_frame(&self) -> RawSse {
        let mut usage = json!({ "input_tokens": 0, "output_tokens": 0 });
        if let Some(mapped) = self
            .service_tier
            .as_deref()
            .and_then(super::messages::usage_service_tier_to_messages)
        {
            usage["service_tier"] = json!(mapped);
        }
        named(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_wiremux",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": usage
                }
            }),
        )
    }

    fn finish_messages(&mut self) -> Vec<RawSse> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(self.messages_start_frame());
        }
        out.extend(self.close_open());
        for ev in std::mem::take(&mut self.deferred) {
            if let Ok(frames) = self.push_messages(ev) {
                out.extend(frames);
            }
        }
        out.extend(self.close_open());
        let refusal = std::mem::take(&mut self.refusal);
        let mapped = self.finish.as_deref().map(encode_stop_reason);
        let saw_tool = !self.used_tool.is_empty();
        let stop = if !refusal.is_empty() && mapped.is_none_or(|r| r == "content_filter") {
            "refusal"
        } else if saw_tool && mapped.is_none_or(|reason| reason == "end_turn") {
            "tool_use"
        } else {
            mapped.unwrap_or("end_turn")
        };
        let mut delta = json!({ "stop_reason": stop, "stop_sequence": null });
        if !refusal.is_empty() {
            delta["stop_details"] = json!({
                "type": "refusal",
                "explanation": refusal,
            });
        }
        let mut data = json!({
            "type": "message_delta",
            "delta": delta
        });
        if let Some((p, c, cr, cw, r, _, _)) = self.usage {
            let usage = usage::encode_anthropic(p, c, cr, cw, r);
            if let Some(u) = usage.get("usage") {
                data["usage"] = u.clone();
            }
        }
        out.push(named("message_delta", data));
        out.push(named("message_stop", json!({ "type": "message_stop" })));
        out
    }

    fn flush_held_tools(&mut self) -> Vec<RawSse> {
        let held = std::mem::take(&mut self.held_tools);
        let mut held: Vec<_> = held.into_values().collect();
        held.sort_by_key(|(enc, _, _, _)| *enc);
        let mut out = Vec::new();
        for (enc, id, name, args) in held {
            self.closed_tools.insert(enc);
            out.push(self.tool_start_frame(enc, &id, &name));
            if !args.is_empty() {
                out.push(self.tool_arg_frame(enc, &args));
            }
            out.push(named(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": enc }),
            ));
        }
        out
    }

    fn push_responses(&mut self, ev: IrStreamEvent) -> Result<Vec<RawSse>, MapError> {
        let mut out = Vec::new();
        if let IrStreamEvent::Created { unix } = ev {
            self.created_at = Some(unix);
        }
        if let IrStreamEvent::ServiceTier { ref tier } = ev {
            self.service_tier = Some(tier.clone());
        }
        if let IrStreamEvent::Metadata { ref metadata } = ev {
            self.metadata = Some(metadata.clone());
        }
        if let IrStreamEvent::Moderation {
            ref input,
            ref output,
        } = ev
        {
            self.moderation = Some((input.clone(), output.clone()));
        }
        if !self.started
            && !matches!(
                ev,
                IrStreamEvent::Created { .. }
                    | IrStreamEvent::ServiceTier { .. }
                    | IrStreamEvent::Metadata { .. }
                    | IrStreamEvent::Moderation { .. }
            )
        {
            self.started = true;
            let mut created = json!({ "id": "resp_wiremux", "status": "in_progress" });
            if !self.model.is_empty() {
                created["model"] = json!(self.model);
            }
            if let Some(unix) = self.created_at {
                created["created_at"] = json!(unix);
            }
            if let Some(ref tier) = self.service_tier {
                created["service_tier"] = json!(tier);
            }
            if let Some(ref meta) = self.metadata {
                created["metadata"] = json!(meta);
            }
            out.push(named(
                "response.created",
                json!({
                    "type": "response.created",
                    "response": created
                }),
            ));
        }
        match ev {
            IrStreamEvent::Created { .. }
            | IrStreamEvent::ServiceTier { .. }
            | IrStreamEvent::Metadata { .. }
            | IrStreamEvent::Moderation { .. } => {}
            IrStreamEvent::TextDelta { text } => {
                out.extend(self.ensure_item(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                self.text_items.entry(index).or_default().push_str(&text);
                out.push(named(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "output_index": index,
                        "delta": text
                    }),
                ));
            }
            IrStreamEvent::RefusalDelta { text } => {
                out.extend(self.ensure_item(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                self.refusal_items.entry(index).or_default().push_str(&text);
                out.push(named(
                    "response.refusal.delta",
                    json!({
                        "type": "response.refusal.delta",
                        "output_index": index,
                        "delta": text
                    }),
                ));
            }
            IrStreamEvent::ReasoningDelta { text } => {
                out.extend(self.ensure_item(BlockKind::Thinking));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                self.reasoning_items
                    .entry(index)
                    .or_default()
                    .push_str(&text);
                out.push(named(
                    "response.reasoning_summary_text.delta",
                    json!({
                        "type": "response.reasoning_summary_text.delta",
                        "output_index": index,
                        "delta": text
                    }),
                ));
            }
            IrStreamEvent::ReasoningSignature { signature } => {
                out.extend(self.ensure_item(BlockKind::Thinking));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                out.push(named(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": index,
                        "item": { "type": "reasoning", "signature": signature }
                    }),
                ));
            }
            IrStreamEvent::ToolCallStart {
                id, name, index, ..
            } => {
                let enc = self.alloc_tool(index);
                out.extend(self.close_item());
                out.push(named(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": enc,
                        "item": {
                            "type": "function_call",
                            "id": id,
                            "call_id": id,
                            "name": name,
                            "arguments": ""
                        }
                    }),
                ));
                self.tool_items
                    .insert(enc, (id.clone(), name.clone(), String::new()));
                self.open = Some((enc, BlockKind::Tool));
            }
            IrStreamEvent::AnnotationAdded { annotation } => {
                out.extend(self.ensure_item(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                let annotations = self.text_annotations.entry(index).or_default();
                annotations.push(annotation.clone());
                let annotation_index =
                    u32::try_from(annotations.len().saturating_sub(1)).unwrap_or(0);
                out.push(named(
                    "response.output_text.annotation.added",
                    json!({
                        "type": "response.output_text.annotation.added",
                        "output_index": index,
                        "content_index": 0,
                        "annotation_index": annotation_index,
                        "annotation": annotation
                    }),
                ));
            }
            IrStreamEvent::AudioDelta { data } => {
                out.push(named(
                    "response.audio.delta",
                    json!({
                        "type": "response.audio.delta",
                        "delta": data
                    }),
                ));
            }
            IrStreamEvent::Logprobs { content } => {
                out.extend(self.ensure_item(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                match &content {
                    Value::Array(arr) => {
                        self.text_logprobs
                            .entry(index)
                            .or_default()
                            .extend(arr.iter().cloned());
                    }
                    other if !other.is_null() => {
                        self.text_logprobs
                            .entry(index)
                            .or_default()
                            .push(other.clone());
                    }
                    _ => {}
                }
                out.push(named(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "output_index": index,
                        "delta": "",
                        "logprobs": content
                    }),
                ));
            }
            IrStreamEvent::AudioTranscriptDelta { text } => {
                out.push(named(
                    "response.audio.transcript.delta",
                    json!({
                        "type": "response.audio.transcript.delta",
                        "delta": text
                    }),
                ));
            }
            IrStreamEvent::CustomToolCallStart { id, name, index } => {
                let enc = self.alloc_tool(index);
                out.extend(self.close_item());
                out.push(named(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": enc,
                        "item": {
                            "type": "custom_tool_call",
                            "id": id,
                            "call_id": id,
                            "name": name,
                            "input": ""
                        }
                    }),
                ));
                self.tool_items
                    .insert(enc, (id.clone(), name.clone(), String::new()));
                self.open = Some((enc, BlockKind::CustomTool));
            }
            IrStreamEvent::CustomToolCallInputDelta { delta, index } => {
                let enc = self.tool_enc(index);
                if let Some((_, _, args)) = self.tool_items.get_mut(&enc) {
                    args.push_str(&delta);
                }
                let item_id = self
                    .tool_items
                    .get(&enc)
                    .map(|(id, _, _)| id.clone())
                    .unwrap_or_default();
                out.push(named(
                    "response.custom_tool_call_input.delta",
                    json!({
                        "type": "response.custom_tool_call_input.delta",
                        "output_index": enc,
                        "item_id": item_id,
                        "delta": delta
                    }),
                ));
            }
            IrStreamEvent::ToolCallArgDelta { delta, index } => {
                let enc = self.tool_enc(index);
                if let Some((_, _, args)) = self.tool_items.get_mut(&enc) {
                    args.push_str(&delta);
                }
                out.push(named(
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": enc,
                        "delta": delta
                    }),
                ));
            }
            IrStreamEvent::ToolCallEnd => {
                out.extend(self.close_item());
            }
            IrStreamEvent::FinishReason { reason } => {
                self.finish = Some(reason);
            }
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                audio_tokens,
                completion_audio_tokens,
            } => {
                self.usage = Some((
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
                    audio_tokens,
                    completion_audio_tokens,
                ));
            }
            other => out.push(encode_stream_event(Wire::Responses, &other)?),
        }
        Ok(out)
    }

    fn ensure_item(&mut self, kind: BlockKind) -> Vec<RawSse> {
        if self.open.is_some_and(|(_, k)| k == kind) {
            return Vec::new();
        }
        let mut out = self.close_item();
        let index = self.next_block;
        self.next_block = self.next_block.saturating_add(1);
        let item = match kind {
            BlockKind::Text => json!({ "type": "message", "role": "assistant", "content": [] }),
            BlockKind::Thinking => json!({ "type": "reasoning" }),
            BlockKind::Tool => json!({ "type": "function_call" }),
            BlockKind::CustomTool => json!({ "type": "custom_tool_call" }),
        };
        out.push(named(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": index,
                "item": item
            }),
        ));
        self.open = Some((index, kind));
        out
    }

    fn close_item(&mut self) -> Vec<RawSse> {
        let Some((index, kind)) = self.open.take() else {
            return Vec::new();
        };
        let mut text_done: Option<Value> = None;
        let item = match kind {
            BlockKind::Text => {
                let text = self.text_items.remove(&index).unwrap_or_default();
                let refusal = self.refusal_items.remove(&index).unwrap_or_default();
                let annotations = self.text_annotations.remove(&index).unwrap_or_default();
                let logprobs = self.text_logprobs.remove(&index).unwrap_or_default();
                let mut content = Vec::new();
                if !text.is_empty() || !annotations.is_empty() || !logprobs.is_empty() {
                    let mut part = json!({ "type": "output_text", "text": text });
                    if !annotations.is_empty() {
                        part["annotations"] = json!(annotations);
                    }
                    if !logprobs.is_empty() {
                        part["logprobs"] = json!(logprobs);
                    }
                    content.push(part);
                }
                if !refusal.is_empty() {
                    content.push(json!({ "type": "refusal", "refusal": refusal }));
                }
                if content.is_empty() {
                    content.push(json!({ "type": "output_text", "text": "" }));
                }
                if !text.is_empty() || !logprobs.is_empty() {
                    let mut done = json!({
                        "type": "response.output_text.done",
                        "output_index": index,
                        "text": text,
                    });
                    if !logprobs.is_empty() {
                        done["logprobs"] = json!(logprobs);
                    }
                    text_done = Some(done);
                }
                json!({
                    "type": "message",
                    "role": "assistant",
                    "content": content
                })
            }
            BlockKind::Thinking => {
                let text = self.reasoning_items.remove(&index).unwrap_or_default();
                json!({
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": text }]
                })
            }
            BlockKind::Tool => match self.tool_items.remove(&index) {
                Some((id, name, arguments)) => json!({
                    "type": "function_call",
                    "id": id,
                    "call_id": id,
                    "name": name,
                    "arguments": arguments
                }),
                None => json!({ "type": "function_call" }),
            },
            BlockKind::CustomTool => match self.tool_items.remove(&index) {
                Some((id, name, input)) => json!({
                    "type": "custom_tool_call",
                    "id": id,
                    "call_id": id,
                    "name": name,
                    "input": input
                }),
                None => json!({ "type": "custom_tool_call" }),
            },
        };
        let mut out = Vec::new();
        if let Some(done) = text_done {
            out.push(named("response.output_text.done", done));
        }
        out.push(named(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": index,
                "item": item
            }),
        ));
        out
    }

    fn finish_responses(&mut self) -> Vec<RawSse> {
        let mut out = self.close_item();
        let reason = self.finish.as_deref().unwrap_or("stop");
        let (event, status) = match reason {
            "failed" => ("response.failed", "failed"),
            "incomplete" | "length" | "max_tokens" | "content_filter" => {
                ("response.incomplete", "incomplete")
            }
            _ => ("response.completed", "completed"),
        };
        let mut response = json!({ "status": status });
        if let Some(detail) = super::responses::incomplete_details_reason(reason) {
            response["incomplete_details"] = json!({ "reason": detail });
        }
        if !self.model.is_empty() {
            response["model"] = json!(self.model);
        }
        if let Some((p, c, cr, cw, r, _, _)) = self.usage {
            let encoded = usage::encode_responses(p, c, cr, cw, r);
            if let Some(u) = encoded.pointer("/response/usage") {
                response["usage"] = u.clone();
            }
        }
        if let Some((ref input, ref output)) = self.moderation
            && let Some(value) = super::complete::responses_moderation_value(input, output)
        {
            response["moderation"] = value;
        }
        if let Some(ref meta) = self.metadata {
            response["metadata"] = json!(meta);
        }
        out.push(named(
            event,
            json!({
                "type": event,
                "response": response
            }),
        ));
        out
    }

    fn push_chat(&mut self, ev: IrStreamEvent) -> Result<Vec<RawSse>, MapError> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(RawSse {
                event: None,
                data: json!({
                    "choices": [{ "index": 0, "delta": { "role": "assistant" } }]
                })
                .to_string(),
            });
        }
        match ev {
            IrStreamEvent::ToolCallStart {
                id, name, index, ..
            } => {
                let enc = self.alloc_tool(index);
                out.push(RawSse {
                    event: None,
                    data: json!({
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": enc,
                                    "id": id,
                                    "type": "function",
                                    "function": { "name": name, "arguments": "" }
                                }]
                            }
                        }]
                    })
                    .to_string(),
                });
            }
            IrStreamEvent::ToolCallArgDelta { delta, index } => {
                let enc = self.tool_enc(index);
                out.push(RawSse {
                    event: None,
                    data: json!({
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": enc,
                                    "function": { "arguments": delta }
                                }]
                            }
                        }]
                    })
                    .to_string(),
                });
            }
            IrStreamEvent::FinishReason { reason } => {
                self.finish = Some(reason);
            }
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                audio_tokens,
                completion_audio_tokens,
            } => {
                self.usage = Some((
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
                    audio_tokens,
                    completion_audio_tokens,
                ));
            }
            IrStreamEvent::ToolCallEnd => {}
            other => {
                if matches!(
                    other,
                    IrStreamEvent::CustomToolCallStart { .. }
                        | IrStreamEvent::CustomToolCallInputDelta { .. }
                ) {
                    self.saw_custom_tool = true;
                }
                out.push(encode_stream_event(Wire::ChatCompletions, &other)?);
            }
        }
        Ok(out
            .into_iter()
            .map(|frame| self.attach_chat_dest_model(frame))
            .collect())
    }

    fn finish_chat(&mut self) -> Vec<RawSse> {
        let mut out = Vec::new();
        if let Some(reason) = self.finish.take() {
            let saw_tool = !self.used_tool.is_empty() || self.saw_custom_tool;
            let reason = super::chat::finish_after_tool(&reason, saw_tool);
            out.push(RawSse {
                event: None,
                data: json!({
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }]
                })
                .to_string(),
            });
        }
        if let Some((p, c, cr, cw, r, audio, completion_audio)) = self.usage.take() {
            out.push(RawSse {
                event: None,
                data: usage::encode_chat(p, c, cr, cw, r, audio, completion_audio).to_string(),
            });
        }
        out.push(RawSse {
            event: None,
            data: "[DONE]".into(),
        });
        out.into_iter()
            .map(|frame| self.attach_chat_dest_model(frame))
            .collect()
    }

    fn push_converse(&mut self, ev: IrStreamEvent) -> Result<Vec<RawSse>, MapError> {
        let mut out = Vec::new();
        match ev {
            IrStreamEvent::TextDelta { text } => {
                out.extend(self.ensure_converse_block(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                out.push(converse_frame_with_index(
                    super::converse::encode(&IrStreamEvent::TextDelta { text })?,
                    index,
                ));
            }
            IrStreamEvent::ReasoningDelta { text } => {
                out.extend(self.ensure_converse_block(BlockKind::Thinking));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                out.push(converse_frame_with_index(
                    super::converse::encode(&IrStreamEvent::ReasoningDelta { text })?,
                    index,
                ));
            }
            IrStreamEvent::ReasoningSignature { signature } => {
                out.extend(self.ensure_converse_block(BlockKind::Thinking));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                out.push(converse_frame_with_index(
                    super::converse::encode(&IrStreamEvent::ReasoningSignature { signature })?,
                    index,
                ));
            }
            IrStreamEvent::ToolCallStart {
                id,
                name,
                thought_signature,
                index,
            } => {
                let enc = self.alloc_tool(index);
                out.extend(self.close_converse());
                out.push(converse_frame_with_index(
                    super::converse::encode(&IrStreamEvent::ToolCallStart {
                        id,
                        name,
                        thought_signature,
                        index,
                    })?,
                    enc,
                ));
                self.open = Some((enc, BlockKind::Tool));
            }
            IrStreamEvent::ToolCallArgDelta { delta, index } => {
                let enc = self.tool_enc(index);
                out.push(converse_frame_with_index(
                    super::converse::encode(&IrStreamEvent::ToolCallArgDelta { delta, index })?,
                    enc,
                ));
            }
            IrStreamEvent::ToolCallEnd => {
                out.extend(self.close_converse());
            }
            IrStreamEvent::FinishReason { reason } => {
                self.finish = Some(reason);
            }
            IrStreamEvent::AudioDelta { .. } => {}
            IrStreamEvent::Logprobs { .. } => {}
            IrStreamEvent::Created { .. }
            | IrStreamEvent::ServiceTier { .. }
            | IrStreamEvent::Metadata { .. }
            | IrStreamEvent::Moderation { .. } => {}
            IrStreamEvent::AudioTranscriptDelta { text } => {
                out.extend(self.ensure_converse_block(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                out.push(converse_frame_with_index(
                    super::converse::encode(&IrStreamEvent::AudioTranscriptDelta { text })?,
                    index,
                ));
            }
            IrStreamEvent::AnnotationAdded { annotation } => {
                out.extend(self.ensure_converse_block(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                out.push(converse_frame_with_index(
                    super::converse::encode(&IrStreamEvent::AnnotationAdded { annotation })?,
                    index,
                ));
            }
            other => out.push(encode_stream_event(Wire::Converse, &other)?),
        }
        Ok(out)
    }

    fn ensure_converse_block(&mut self, kind: BlockKind) -> Vec<RawSse> {
        if self.open.is_some_and(|(_, k)| k == kind) {
            return Vec::new();
        }
        let out = self.close_converse();
        let index = self.next_block;
        self.next_block = self.next_block.saturating_add(1);
        self.open = Some((index, kind));
        out
    }

    fn close_converse(&mut self) -> Vec<RawSse> {
        let Some((index, _)) = self.open.take() else {
            return Vec::new();
        };
        vec![converse_frame_with_index(
            json!({ "contentBlockStop": {} }),
            index,
        )]
    }

    fn finish_converse(&mut self) -> Result<Vec<RawSse>, MapError> {
        let mut out = self.close_converse();
        let reason = self.finish.take().unwrap_or_else(|| "end_turn".into());
        out.push(encode_stream_event(
            Wire::Converse,
            &IrStreamEvent::FinishReason { reason },
        )?);
        Ok(out)
    }
}

fn converse_frame_with_index(mut value: Value, index: u32) -> RawSse {
    attach_block_index(&mut value, index);
    RawSse {
        event: None,
        data: value.to_string(),
    }
}

fn attach_block_index(value: &mut Value, index: u32) {
    for key in ["contentBlockDelta", "contentBlockStart", "contentBlockStop"] {
        if let Some(Value::Object(obj)) = value.get_mut(key) {
            obj.insert("contentBlockIndex".into(), json!(index));
        }
    }
}

fn named(event: &str, data: Value) -> RawSse {
    RawSse {
        event: Some(event.into()),
        data: data.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrStreamEvent;

    fn stop_reason(frames: &[RawSse]) -> Option<String> {
        frames.iter().find_map(|frame| {
            serde_json::from_str::<Value>(&frame.data)
                .ok()
                .and_then(|v| {
                    v.pointer("/messageStop/stopReason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        })
    }

    fn chat_finish_reason(frames: &[RawSse]) -> Option<String> {
        frames.iter().find_map(|frame| {
            serde_json::from_str::<Value>(&frame.data)
                .ok()
                .and_then(|v| {
                    v.pointer("/choices/0/finish_reason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        })
    }

    #[test]
    fn responses_encoder_created_uses_dest_model() {
        let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
        let frames = enc
            .push(IrStreamEvent::TextDelta { text: "hi".into() })
            .expect("push");
        let created = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.created"))
            .expect("response.created");
        assert!(
            created.data.contains("\"model\":\"gpt-4o\""),
            "response.created must use dest model, got {}",
            created.data
        );
        let done = enc.finish().expect("finish");
        assert!(
            done.iter()
                .any(|frame| frame.data.contains("\"model\":\"gpt-4o\"")),
            "response.completed must use dest model, got {done:?}"
        );
    }

    #[test]
    fn responses_encoder_output_item_done_keeps_function_call() {
        let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
        enc.push(IrStreamEvent::ToolCallStart {
            id: "call_1".into(),
            name: "get_weather".into(),
            thought_signature: None,
            index: 0,
        })
        .expect("start");
        enc.push(IrStreamEvent::ToolCallArgDelta {
            delta: r#"{"city":"Paris"}"#.into(),
            index: 0,
        })
        .expect("args");
        let done = enc.finish().expect("finish");
        let item_done = done
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.output_item.done"))
            .expect("response.output_item.done");
        assert!(
            item_done.data.contains("\"name\":\"get_weather\"")
                && item_done
                    .data
                    .contains(r#""arguments":"{\"city\":\"Paris\"}""#)
                && item_done.data.contains("\"call_id\":\"call_1\""),
            "output_item.done must keep the function_call item, got {}",
            item_done.data
        );
    }

    #[test]
    fn responses_encoder_output_item_done_keeps_text() {
        let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
        enc.push(IrStreamEvent::TextDelta { text: "P".into() })
            .expect("p");
        enc.push(IrStreamEvent::TextDelta { text: "ong".into() })
            .expect("ong");
        let done = enc.finish().expect("finish");
        let item_done = done
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.output_item.done"))
            .expect("response.output_item.done");
        assert!(
            item_done.data.contains("\"type\":\"message\"")
                && item_done.data.contains("\"text\":\"Pong\""),
            "output_item.done must keep assembled text, got {}",
            item_done.data
        );
    }

    #[test]
    fn responses_encoder_output_item_done_keeps_reasoning() {
        let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
        enc.push(IrStreamEvent::ReasoningDelta {
            text: "think".into(),
        })
        .expect("reason");
        let mut frames = enc
            .push(IrStreamEvent::TextDelta { text: "hi".into() })
            .expect("text");
        frames.extend(enc.finish().expect("finish"));
        let reason_done = frames
            .iter()
            .find(|frame| {
                frame.event.as_deref() == Some("response.output_item.done")
                    && frame.data.contains("\"type\":\"reasoning\"")
            })
            .expect("reasoning output_item.done");
        assert!(
            reason_done.data.contains("think"),
            "output_item.done must keep assembled reasoning, got {}",
            reason_done.data
        );
    }

    #[test]
    fn gemini_encoder_chunks_use_dest_model_version() {
        let mut enc = StreamEncoder::new(Wire::Gemini).with_model("gemini-2.5-flash");
        let frames = enc
            .push(IrStreamEvent::TextDelta { text: "hi".into() })
            .expect("push");
        assert!(
            frames
                .iter()
                .any(|frame| frame.data.contains("\"modelVersion\":\"gemini-2.5-flash\"")),
            "dest Gemini chunk must include modelVersion, got {frames:?}"
        );
        let done = enc.finish().expect("finish");
        assert!(
            done.iter().any(|frame| frame.data.trim() == "[DONE]"),
            "Gemini finish stays [DONE], got {done:?}"
        );
        assert!(
            done.iter()
                .all(|frame| !frame.data.contains("modelVersion")),
            "[DONE] must not grow a modelVersion, got {done:?}"
        );
    }

    #[test]
    fn chat_encoder_chunks_use_dest_model() {
        let mut enc = StreamEncoder::new(Wire::ChatCompletions).with_model("claude-haiku-4-5");
        let frames = enc
            .push(IrStreamEvent::TextDelta { text: "hi".into() })
            .expect("push");
        assert!(
            frames
                .iter()
                .any(|frame| frame.data.contains("\"model\":\"claude-haiku-4-5\"")),
            "dest Chat stream must include dest model, got {frames:?}"
        );
    }

    #[test]
    fn dest_responses_encoder_refusal_delta_is_refusal_event() {
        let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
        let frames = enc
            .push(IrStreamEvent::RefusalDelta {
                text: "nope".into(),
            })
            .expect("push dest Responses refusal");
        assert!(
            frames.iter().any(|frame| {
                frame.event.as_deref() == Some("response.refusal.delta")
                    && frame.data.contains(r#""delta":"nope""#)
            }),
            "dest Responses stream encode must emit response.refusal.delta, got {frames:?}"
        );
        let done = enc.finish().expect("finish dest Responses refusal");
        let item_done = done
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.output_item.done"))
            .expect("response.output_item.done");
        assert!(
            item_done.data.contains(r#""type":"refusal""#)
                && item_done.data.contains(r#""refusal":"nope""#),
            "dest Responses output_item.done must keep dest Chat refusal, got {item_done:?}"
        );
        assert!(
            !item_done.data.contains(r#""type":"output_text""#),
            "dest Responses output_item.done must not overwrite dest Chat refusal as output_text, got {item_done:?}"
        );
    }

    #[test]
    fn dest_responses_encoder_annotation_added_is_annotation_event() {
        let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
        enc.push(IrStreamEvent::TextDelta {
            text: "See https://example.com".into(),
        })
        .expect("push text");
        let frames = enc
            .push(IrStreamEvent::AnnotationAdded {
                annotation: json!({
                    "type": "url_citation",
                    "start_index": 4,
                    "end_index": 23,
                    "title": "Example Domain",
                    "url": "https://example.com"
                }),
            })
            .expect("push dest Responses annotation");
        assert!(
            frames.iter().any(|frame| {
                frame.event.as_deref() == Some("response.output_text.annotation.added")
                    && frame.data.contains("https://example.com")
            }),
            "dest Responses stream encode must emit response.output_text.annotation.added, got {frames:?}"
        );
        let done = enc.finish().expect("finish dest Responses annotation");
        let item_done = done
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.output_item.done"))
            .expect("response.output_item.done");
        assert!(
            item_done.data.contains("annotations")
                && item_done.data.contains("https://example.com"),
            "dest Responses output_item.done output_text must keep annotations, got {}",
            item_done.data
        );
    }

    #[test]
    fn dest_responses_encoder_audio_delta_is_audio_event() {
        let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
        let frames = enc
            .push(IrStreamEvent::AudioDelta {
                data: "SUQz".into(),
            })
            .expect("push dest Responses audio");
        assert!(
            frames.iter().any(|frame| {
                frame.event.as_deref() == Some("response.audio.delta")
                    && frame.data.contains(r#""delta":"SUQz""#)
            }),
            "dest Responses stream encode must emit response.audio.delta, got {frames:?}"
        );
        let more = enc
            .push(IrStreamEvent::AudioTranscriptDelta {
                text: "hello there".into(),
            })
            .expect("push dest Responses audio transcript");
        assert!(
            more.iter().any(|frame| {
                frame.event.as_deref() == Some("response.audio.transcript.delta")
                    && frame.data.contains(r#""delta":"hello there""#)
            }),
            "dest Responses stream encode must emit response.audio.transcript.delta, got {more:?}"
        );
    }

    #[test]
    fn dest_messages_encoder_logprobs_skips_empty_text() {
        let mut enc = StreamEncoder::new(Wire::Messages);
        let frames = enc
            .push(IrStreamEvent::Logprobs {
                content: json!([{
                    "token": "Hi",
                    "logprob": -0.1,
                    "bytes": [72, 105],
                    "top_logprobs": [{ "token": "Hi", "logprob": -0.1, "bytes": [72, 105] }]
                }]),
            })
            .expect("push dest Messages logprobs");
        assert!(
            frames.iter().all(|frame| {
                frame.event.as_deref() != Some("content_block_delta")
                    || !frame.data.contains(r#""text":"""#)
            }),
            "dest Messages STREAM must not emit empty text_delta solely from Logprobs, got {frames:?}"
        );
    }

    #[test]
    fn dest_responses_encoder_custom_tool_call_is_custom_events() {
        let mut enc = StreamEncoder::new(Wire::Responses).with_model("gpt-4o");
        enc.push(IrStreamEvent::CustomToolCallStart {
            id: "call_custom".into(),
            name: "code_exec".into(),
            index: 0,
        })
        .expect("start custom");
        let frames = enc
            .push(IrStreamEvent::CustomToolCallInputDelta {
                delta: "print(1)".into(),
                index: 0,
            })
            .expect("custom input");
        assert!(
            frames.iter().any(|frame| {
                frame.event.as_deref() == Some("response.custom_tool_call_input.delta")
                    && frame.data.contains(r#""delta":"print(1)""#)
            }),
            "dest Responses stream encode must emit response.custom_tool_call_input.delta, got {frames:?}"
        );
        let done = enc.finish().expect("finish custom");
        assert!(
            done.iter().any(|frame| {
                frame.event.as_deref() == Some("response.output_item.done")
                    && frame.data.contains("\"type\":\"custom_tool_call\"")
                    && frame.data.contains("\"name\":\"code_exec\"")
                    && frame.data.contains(r#""input":"print(1)""#)
            }),
            "dest Responses output_item.done must keep custom_tool_call, got {done:?}"
        );
    }

    #[test]
    fn dest_responses_encoder_finish_maps_content_filter() {
        let mut enc = StreamEncoder::new(Wire::Responses);
        enc.push(IrStreamEvent::FinishReason {
            reason: "content_filter".into(),
        })
        .expect("push content_filter");
        let frames = enc.finish().expect("finish content_filter");
        assert!(
            frames
                .iter()
                .all(|frame| frame.event.as_deref() != Some("response.completed")),
            "IR content_filter must not dest-encode as response.completed, got {frames:?}"
        );
        let incomplete = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.incomplete"))
            .expect("response.incomplete");
        let json: Value = serde_json::from_str(&incomplete.data).expect("json");
        assert_eq!(
            json.pointer("/response/status").and_then(Value::as_str),
            Some("incomplete"),
            "IR content_filter must dest-encode status incomplete, got {json}"
        );
        assert_eq!(
            json.pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str),
            Some("content_filter"),
            "IR content_filter must dest-encode incomplete_details.reason, got {json}"
        );
    }

    #[test]
    fn dest_responses_encoder_finish_maps_length() {
        let mut enc = StreamEncoder::new(Wire::Responses);
        enc.push(IrStreamEvent::FinishReason {
            reason: "length".into(),
        })
        .expect("push length");
        let frames = enc.finish().expect("finish length");
        let incomplete = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.incomplete"))
            .expect("response.incomplete");
        let json: Value = serde_json::from_str(&incomplete.data).expect("json");
        assert_eq!(
            json.pointer("/response/status").and_then(Value::as_str),
            Some("incomplete"),
            "IR length must dest-encode status incomplete, got {json}"
        );
        assert_eq!(
            json.pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str),
            Some("max_output_tokens"),
            "IR length must dest-encode incomplete_details.reason=max_output_tokens, got {json}"
        );
    }

    #[test]
    fn dest_responses_encoder_usage_maps_cache_write_and_total() {
        let mut enc = StreamEncoder::new(Wire::Responses);
        enc.push(IrStreamEvent::Usage {
            prompt_tokens: 80,
            completion_tokens: 12,
            cache_read_tokens: 25,
            cache_write_tokens: 9,
            reasoning_tokens: 3,
            audio_tokens: 0,
            completion_audio_tokens: 0,
        })
        .expect("push dest Chat leftover usage");
        let frames = enc.finish().expect("finish usage");
        let completed = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.completed"))
            .expect("response.completed");
        let json: Value = serde_json::from_str(&completed.data).expect("json");
        assert_eq!(
            json.pointer("/response/usage/input_tokens_details/cache_write_tokens")
                .and_then(Value::as_u64),
            Some(9),
            "dest Chat cache_write_tokens must dest-encode dest Responses, got {json}"
        );
        assert_eq!(
            json.pointer("/response/usage/total_tokens")
                .and_then(Value::as_u64),
            Some(120),
            "dest Responses usage must include total_tokens, got {json}"
        );
    }

    #[test]
    fn dest_chat_encoder_refusal_delta_is_delta_refusal() {
        let mut enc = StreamEncoder::new(Wire::ChatCompletions).with_model("gpt-4o");
        let frames = enc
            .push(IrStreamEvent::RefusalDelta {
                text: "nope".into(),
            })
            .expect("push dest Chat refusal");
        assert!(
            frames
                .iter()
                .any(|frame| frame.data.contains(r#""refusal":"nope""#)),
            "dest Chat stream encode must write delta.refusal, got {frames:?}"
        );
    }

    #[test]
    fn dest_messages_encoder_refusal_delta_is_stop_details() {
        let mut enc = StreamEncoder::new(Wire::Messages).with_model("claude-sonnet-4");
        let frames = enc
            .push(IrStreamEvent::RefusalDelta {
                text: "nope".into(),
            })
            .expect("push dest Messages STREAM refusal");
        let done = enc.finish().expect("finish dest Messages STREAM refusal");
        let all: Vec<&RawSse> = frames.iter().chain(done.iter()).collect();
        assert!(
            all.iter().any(|frame| {
                frame.event.as_deref() == Some("message_delta")
                    && serde_json::from_str::<Value>(&frame.data)
                        .ok()
                        .is_some_and(|body| {
                            body.pointer("/delta/stop_details/explanation")
                                .and_then(Value::as_str)
                                == Some("nope")
                                && body
                                    .pointer("/delta/stop_details/type")
                                    .and_then(Value::as_str)
                                    == Some("refusal")
                        })
            }),
            "dest Messages STREAM encode must write message_delta stop_details.explanation, got {all:?}"
        );
        assert!(
            all.iter().all(|frame| {
                serde_json::from_str::<Value>(&frame.data)
                    .ok()
                    .is_none_or(|body| {
                        body.pointer("/delta/text").and_then(Value::as_str) != Some("nope")
                    })
            }),
            "dest Messages STREAM refusal must not fold into text_delta, got {all:?}"
        );
    }

    #[test]
    fn messages_encoder_holds_second_tool_until_first_args_finish() {
        let mut enc = StreamEncoder::new(Wire::Messages);
        let mut frames = Vec::new();
        for ev in [
            IrStreamEvent::ToolCallStart {
                id: "call_a".into(),
                name: "get_weather".into(),
                thought_signature: None,
                index: 0,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: r#"{"city":"Paris"}"#.into(),
                index: 0,
            },
            IrStreamEvent::ToolCallStart {
                id: "call_b".into(),
                name: "get_time".into(),
                thought_signature: None,
                index: 1,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: "{}".into(),
                index: 1,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: " more".into(),
                index: 0,
            },
        ] {
            frames.extend(enc.push(ev).expect("push"));
        }
        frames.extend(enc.finish().expect("finish"));
        let body = frames
            .iter()
            .map(|frame| frame.data.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let stop0 = body.find("\"content_block_stop\"").expect("first stop");
        let more = body.find(" more").expect("late arg");
        assert!(
            more < stop0,
            "late index 0 args must precede content_block_stop, got {body}"
        );
        assert!(
            body.contains("call_b") && body.contains("get_time"),
            "second tool must still be emitted, got {body}"
        );
        assert!(
            body.contains("\"stop_reason\":\"tool_use\""),
            "tool stream with no finish reason must be tool_use, got {body}"
        );

        let mut filtered = StreamEncoder::new(Wire::Messages);
        filtered
            .push(IrStreamEvent::ToolCallStart {
                id: "call_a".into(),
                name: "get_weather".into(),
                thought_signature: None,
                index: 0,
            })
            .expect("tool");
        filtered
            .push(IrStreamEvent::FinishReason {
                reason: "content_filter".into(),
            })
            .expect("filter");
        let filtered_body = filtered
            .finish()
            .expect("finish")
            .iter()
            .map(|frame| frame.data.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            filtered_body.contains("\"stop_reason\":\"content_filter\""),
            "content_filter stays after a tool, got {filtered_body}"
        );

        let mut interrupted = StreamEncoder::new(Wire::Messages);
        let mut frames = Vec::new();
        for ev in [
            IrStreamEvent::ToolCallStart {
                id: "call_a".into(),
                name: "get_weather".into(),
                thought_signature: None,
                index: 0,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: r#"{"city":"Paris"}"#.into(),
                index: 0,
            },
            IrStreamEvent::ToolCallStart {
                id: "call_b".into(),
                name: "get_time".into(),
                thought_signature: None,
                index: 1,
            },
            IrStreamEvent::ToolCallEnd,
            IrStreamEvent::ReasoningDelta {
                text: "think".into(),
            },
            IrStreamEvent::ToolCallStart {
                id: "call_a".into(),
                name: "get_weather".into(),
                thought_signature: None,
                index: 0,
            },
            IrStreamEvent::TextDelta { text: "hi".into() },
            IrStreamEvent::ToolCallArgDelta {
                delta: " more".into(),
                index: 0,
            },
            IrStreamEvent::ToolCallStart {
                id: "call_b".into(),
                name: "get_time".into(),
                thought_signature: None,
                index: 1,
            },
            IrStreamEvent::ToolCallArgDelta {
                delta: "{\"tz\":\"UTC\"}".into(),
                index: 1,
            },
        ] {
            frames.extend(interrupted.push(ev).expect("push"));
        }
        frames.extend(interrupted.finish().expect("finish"));
        let body = frames
            .iter()
            .map(|frame| frame.data.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let stop0 = body.find("\"content_block_stop\"").expect("stop");
        let more = body.find(" more").expect("late arg kept");
        let call_b = body.find("call_b").expect("second tool");
        let hi = body.find("\"text\":\"hi\"").expect("text");
        assert!(more < stop0, "late args stay before stop, got {body}");
        let think = body.find("think").expect("reasoning");
        let weather = body.matches("get_weather").count();
        assert!(call_b < hi, "held tool is framed before text, got {body}");
        assert!(
            stop0 < think,
            "reasoning waits until the tool stops, got {body}"
        );
        assert_eq!(
            weather, 1,
            "repeat start must not open a second block, got {body}"
        );
        assert!(
            body.contains("UTC"),
            "repeat start must keep buffered arguments, got {body}"
        );
        let after_hi = &body[hi..];
        let text_stop = after_hi.find("\"content_block_stop\"").expect("text stop");
        let message_delta = after_hi.find("message_delta").expect("message_delta");
        assert!(
            text_stop < message_delta,
            "deferred text must stop before message_delta, got {body}"
        );
    }

    #[test]
    fn messages_encoder_message_start_uses_dest_model() {
        let mut enc = StreamEncoder::new(Wire::Messages).with_model("claude-sonnet-4");
        let frames = enc
            .push(IrStreamEvent::TextDelta { text: "hi".into() })
            .expect("push");
        let start = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("message_start"))
            .expect("message_start");
        assert!(
            start.data.contains("\"model\":\"claude-sonnet-4\""),
            "message_start must use dest model, got {}",
            start.data
        );
        assert!(
            !start.data.contains("\"model\":\"\""),
            "message_start must not emit empty model, got {}",
            start.data
        );
    }

    #[test]
    fn chat_encoder_stop_after_tool_is_tool_calls() {
        let mut tools = StreamEncoder::new(Wire::ChatCompletions);
        tools
            .push(IrStreamEvent::ToolCallStart {
                id: "call_a".into(),
                name: "get_weather".into(),
                thought_signature: None,
                index: 0,
            })
            .expect("push tool");
        tools
            .push(IrStreamEvent::FinishReason {
                reason: "stop".into(),
            })
            .expect("push stop");
        let frames = tools.finish().expect("finish");
        assert_eq!(
            chat_finish_reason(&frames).as_deref(),
            Some("tool_calls"),
            "stop after a tool call must be tool_calls, got {frames:?}"
        );

        let mut plain = StreamEncoder::new(Wire::ChatCompletions);
        plain
            .push(IrStreamEvent::FinishReason {
                reason: "stop".into(),
            })
            .expect("push stop");
        let plain_frames = plain.finish().expect("finish plain");
        assert_eq!(
            chat_finish_reason(&plain_frames).as_deref(),
            Some("stop"),
            "stop with no tool call stays stop, got {plain_frames:?}"
        );

        let mut filtered = StreamEncoder::new(Wire::ChatCompletions);
        filtered
            .push(IrStreamEvent::ToolCallStart {
                id: "call_a".into(),
                name: "get_weather".into(),
                thought_signature: None,
                index: 0,
            })
            .expect("push tool");
        filtered
            .push(IrStreamEvent::FinishReason {
                reason: "content_filter".into(),
            })
            .expect("push filter");
        let filtered_frames = filtered.finish().expect("finish filter");
        assert_eq!(
            chat_finish_reason(&filtered_frames).as_deref(),
            Some("content_filter"),
            "content_filter stays even after a tool call, got {filtered_frames:?}"
        );
    }

    #[test]
    fn chat_encoder_failed_after_tool_stays_stop() {
        for reason in [
            "failed",
            "cancelled",
            "canceled",
            "pause_turn",
            "stop_sequence",
        ] {
            let mut enc = StreamEncoder::new(Wire::ChatCompletions);
            enc.push(IrStreamEvent::ToolCallStart {
                id: "call_a".into(),
                name: "get_weather".into(),
                thought_signature: None,
                index: 0,
            })
            .expect("push tool");
            enc.push(IrStreamEvent::ToolCallArgDelta {
                delta: "{\"city\":".into(),
                index: 0,
            })
            .expect("push args");
            enc.push(IrStreamEvent::FinishReason {
                reason: reason.into(),
            })
            .expect("push finish");
            let frames = enc.finish().expect("finish");
            assert_eq!(
                chat_finish_reason(&frames).as_deref(),
                Some("stop"),
                "{reason} after a partial tool call must stay stop, got {frames:?}"
            );
        }

        let mut end = StreamEncoder::new(Wire::ChatCompletions);
        end.push(IrStreamEvent::ToolCallStart {
            id: "call_a".into(),
            name: "get_weather".into(),
            thought_signature: None,
            index: 0,
        })
        .expect("push tool");
        end.push(IrStreamEvent::FinishReason {
            reason: "end_turn".into(),
        })
        .expect("push end_turn");
        let frames = end.finish().expect("finish");
        assert_eq!(
            chat_finish_reason(&frames).as_deref(),
            Some("tool_calls"),
            "end_turn after a tool call must be tool_calls, got {frames:?}"
        );
    }

    #[test]
    fn chat_encoder_stop_after_custom_tool_is_tool_calls() {
        let mut tools = StreamEncoder::new(Wire::ChatCompletions);
        let start = tools
            .push(IrStreamEvent::CustomToolCallStart {
                id: "call_c".into(),
                name: "widget".into(),
                index: 0,
            })
            .expect("push custom");
        assert!(
            start
                .iter()
                .any(|frame| frame.data.contains("\"type\":\"custom\"")),
            "custom tool frame must stay type custom, got {start:?}"
        );
        tools
            .push(IrStreamEvent::CustomToolCallInputDelta {
                delta: "x".into(),
                index: 0,
            })
            .expect("push input");
        tools
            .push(IrStreamEvent::FinishReason {
                reason: "stop".into(),
            })
            .expect("push stop");
        let frames = tools.finish().expect("finish");
        assert_eq!(
            chat_finish_reason(&frames).as_deref(),
            Some("tool_calls"),
            "stop after a custom tool must be tool_calls, got {frames:?}"
        );

        let mut filtered = StreamEncoder::new(Wire::ChatCompletions);
        filtered
            .push(IrStreamEvent::CustomToolCallStart {
                id: "call_c".into(),
                name: "widget".into(),
                index: 0,
            })
            .expect("push custom");
        filtered
            .push(IrStreamEvent::FinishReason {
                reason: "content_filter".into(),
            })
            .expect("push filter");
        let filtered_frames = filtered.finish().expect("finish filter");
        assert_eq!(
            chat_finish_reason(&filtered_frames).as_deref(),
            Some("content_filter"),
            "content_filter stays after a custom tool, got {filtered_frames:?}"
        );
    }

    #[test]
    fn converse_encoder_finish_maps_chat_stop_and_tool_calls() {
        let mut stop = StreamEncoder::new(Wire::Converse);
        assert!(
            stop.push(IrStreamEvent::FinishReason {
                reason: "stop".into(),
            })
            .expect("push stop")
            .is_empty()
        );
        let stop_frames = stop.finish().expect("finish stop");
        assert_eq!(
            stop_reason(&stop_frames).as_deref(),
            Some("end_turn"),
            "Chat stop must become AWS end_turn, got {stop_frames:?}"
        );

        let mut tools = StreamEncoder::new(Wire::Converse);
        assert!(
            tools
                .push(IrStreamEvent::FinishReason {
                    reason: "tool_calls".into(),
                })
                .expect("push tool_calls")
                .is_empty()
        );
        let tool_frames = tools.finish().expect("finish tools");
        assert_eq!(
            stop_reason(&tool_frames).as_deref(),
            Some("tool_use"),
            "Chat tool_calls must become AWS tool_use, got {tool_frames:?}"
        );

        let mut filtered = StreamEncoder::new(Wire::Converse);
        assert!(
            filtered
                .push(IrStreamEvent::FinishReason {
                    reason: "content_filter".into(),
                })
                .expect("push content_filter")
                .is_empty()
        );
        let filtered_frames = filtered.finish().expect("finish content_filter");
        assert_eq!(
            stop_reason(&filtered_frames).as_deref(),
            Some("content_filtered"),
            "IR content_filter must become AWS content_filtered, got {filtered_frames:?}"
        );
    }

    #[test]
    fn converse_encoder_text_then_tool_uses_distinct_block_indexes() {
        let mut enc = StreamEncoder::new(Wire::Converse);
        let mut frames = Vec::new();
        frames.extend(
            enc.push(IrStreamEvent::TextDelta { text: "hi".into() })
                .expect("text"),
        );
        frames.extend(
            enc.push(IrStreamEvent::ToolCallStart {
                id: "call_1".into(),
                name: "lookup".into(),
                thought_signature: None,
                index: 0,
            })
            .expect("tool start"),
        );
        frames.extend(enc.finish().expect("finish"));

        let text_idx = frames.iter().find_map(|frame| {
            let value: Value = serde_json::from_str(&frame.data).ok()?;
            (value.pointer("/contentBlockDelta/delta/text")?.as_str()? == "hi")
                .then(|| {
                    value
                        .pointer("/contentBlockDelta/contentBlockIndex")?
                        .as_u64()
                })
                .flatten()
        });
        let tool_idx = frames.iter().find_map(|frame| {
            let value: Value = serde_json::from_str(&frame.data).ok()?;
            value.pointer("/contentBlockStart/start/toolUse")?;
            value
                .pointer("/contentBlockStart/contentBlockIndex")?
                .as_u64()
        });
        assert_eq!(text_idx, Some(0), "text delta index, got {frames:?}");
        assert_eq!(tool_idx, Some(1), "tool start index, got {frames:?}");
    }
}

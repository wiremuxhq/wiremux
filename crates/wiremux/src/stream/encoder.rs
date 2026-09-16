//! Stateful SSE encoder. One instance per output stream.

use std::collections::{HashMap, HashSet, VecDeque};

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
}

/// Accumulates IR events into dialect-correct SSE frames.
pub struct StreamEncoder {
    wire: Wire,
    started: bool,
    finished: bool,
    next_block: u32,
    open: Option<(u32, BlockKind)>,
    finish: Option<String>,
    usage: Option<(u32, u32, u32, u32, u32)>,
    used_tool: HashSet<u32>,
    next_tool: u32,
    tool_slots: HashMap<u32, VecDeque<u32>>,
    tool_got_arg: HashSet<u32>,
    last_tool: HashMap<u32, u32>,
}

impl StreamEncoder {
    /// Encoder for `wire` client frames.
    #[must_use]
    pub fn new(wire: Wire) -> Self {
        Self {
            wire,
            started: false,
            finished: false,
            next_block: 0,
            open: None,
            finish: None,
            usage: None,
            used_tool: HashSet::new(),
            next_tool: 0,
            tool_slots: HashMap::new(),
            tool_got_arg: HashSet::new(),
            last_tool: HashMap::new(),
        }
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
                Wire::Converse => {
                    if let IrStreamEvent::FinishReason { reason } = other {
                        self.finish = Some(reason);
                        return Ok(Vec::new());
                    }
                    Ok(vec![encode_stream_event(self.wire, &other)?])
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
            Wire::Converse => {
                let reason = self.finish.clone().unwrap_or_else(|| "end_turn".into());
                encode_stream_event(self.wire, &IrStreamEvent::FinishReason { reason })
                    .map(|frame| vec![frame])
            }
            _ => Ok(Vec::new()),
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
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(named(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_wiremux",
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": "",
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 }
                    }
                }),
            ));
        }
        match ev {
            IrStreamEvent::TextDelta { text } => {
                out.extend(self.ensure_block(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                out.push(named(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "text_delta", "text": text }
                    }),
                ));
            }
            IrStreamEvent::ReasoningDelta { text } => {
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
            IrStreamEvent::ReasoningSignature { signature } => {
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
            IrStreamEvent::ToolCallStart {
                id, name, index, ..
            } => {
                let enc = self.alloc_tool(index);
                out.extend(self.close_open());
                out.push(named(
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
                ));
                self.open = Some((enc, BlockKind::Tool));
            }
            IrStreamEvent::ToolCallArgDelta { delta, index } => {
                let enc = self.tool_enc(index);
                out.push(named(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": enc,
                        "delta": { "type": "input_json_delta", "partial_json": delta }
                    }),
                ));
            }
            IrStreamEvent::ToolCallEnd => {
                out.extend(self.close_open());
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
            } => {
                self.usage = Some((
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
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
            BlockKind::Tool => json!({ "type": "tool_use", "id": "", "name": "", "input": {} }),
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

    fn close_open(&mut self) -> Vec<RawSse> {
        let Some((index, _)) = self.open.take() else {
            return Vec::new();
        };
        vec![named(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": index }),
        )]
    }

    fn finish_messages(&mut self) -> Vec<RawSse> {
        let mut out = self.close_open();
        let reason = self.finish.as_deref().unwrap_or("stop");
        let stop = encode_stop_reason(reason);
        let mut data = json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop, "stop_sequence": null }
        });
        if let Some((p, c, cr, cw, r)) = self.usage {
            let usage = usage::encode_anthropic(p, c, cr, cw, r);
            if let Some(u) = usage.get("usage") {
                data["usage"] = u.clone();
            }
        }
        out.push(named("message_delta", data));
        out.push(named("message_stop", json!({ "type": "message_stop" })));
        out
    }

    fn push_responses(&mut self, ev: IrStreamEvent) -> Result<Vec<RawSse>, MapError> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(named(
                "response.created",
                json!({
                    "type": "response.created",
                    "response": { "id": "resp_wiremux", "status": "in_progress" }
                }),
            ));
        }
        match ev {
            IrStreamEvent::TextDelta { text } => {
                out.extend(self.ensure_item(BlockKind::Text));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
                out.push(named(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "output_index": index,
                        "delta": text
                    }),
                ));
            }
            IrStreamEvent::ReasoningDelta { text } => {
                out.extend(self.ensure_item(BlockKind::Thinking));
                let index = self.open.map(|(i, _)| i).unwrap_or(0);
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
                self.open = Some((enc, BlockKind::Tool));
            }
            IrStreamEvent::ToolCallArgDelta { delta, index } => {
                let enc = self.tool_enc(index);
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
            } => {
                self.usage = Some((
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
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
        let item_type = match kind {
            BlockKind::Text => "message",
            BlockKind::Thinking => "reasoning",
            BlockKind::Tool => "function_call",
        };
        vec![named(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": index,
                "item": { "type": item_type }
            }),
        )]
    }

    fn finish_responses(&mut self) -> Vec<RawSse> {
        let mut out = self.close_item();
        let reason = self.finish.as_deref().unwrap_or("stop");
        let (event, status) = match reason {
            "failed" => ("response.failed", "failed"),
            "incomplete" | "length" | "max_tokens" => ("response.incomplete", "incomplete"),
            _ => ("response.completed", "completed"),
        };
        let mut response = json!({ "status": status });
        if let Some((p, c, cr, _cw, r)) = self.usage {
            let encoded = usage::encode_responses(p, c, cr, r);
            if let Some(u) = encoded.pointer("/response/usage") {
                response["usage"] = u.clone();
            }
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
            } => {
                self.usage = Some((
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
                ));
            }
            IrStreamEvent::ToolCallEnd => {}
            other => out.push(encode_stream_event(Wire::ChatCompletions, &other)?),
        }
        Ok(out)
    }

    fn finish_chat(&mut self) -> Vec<RawSse> {
        let mut out = Vec::new();
        if let Some(reason) = self.finish.take() {
            let reason = super::chat::encode_finish(&reason);
            out.push(RawSse {
                event: None,
                data: json!({
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }]
                })
                .to_string(),
            });
        }
        if let Some((p, c, cr, cw, r)) = self.usage.take() {
            out.push(RawSse {
                event: None,
                data: usage::encode_chat(p, c, cr, cw, r).to_string(),
            });
        }
        out.push(RawSse {
            event: None,
            data: "[DONE]".into(),
        });
        out
    }
}

fn named(event: &str, data: Value) -> RawSse {
    RawSse {
        event: Some(event.into()),
        data: data.to_string(),
    }
}

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
    model: String,
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
    tool_items: HashMap<u32, (String, String, String)>,
    text_items: HashMap<u32, String>,
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
            next_tool: 0,
            tool_slots: HashMap::new(),
            tool_got_arg: HashSet::new(),
            last_tool: HashMap::new(),
            tool_items: HashMap::new(),
            text_items: HashMap::new(),
        }
    }

    /// Dest request model for Messages, Gemini, and Responses encode.
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
        if self.model.is_empty() || frame.data.trim() == "[DONE]" {
            return frame;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(&frame.data) else {
            return frame;
        };
        let Value::Object(obj) = &mut value else {
            return frame;
        };
        obj.insert("modelVersion".into(), json!(self.model.clone()));
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
                        "model": self.model,
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
            let mut created = json!({ "id": "resp_wiremux", "status": "in_progress" });
            if !self.model.is_empty() {
                created["model"] = json!(self.model);
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
                self.tool_items
                    .insert(enc, (id.clone(), name.clone(), String::new()));
                self.open = Some((enc, BlockKind::Tool));
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
        let item = match kind {
            BlockKind::Text => {
                let text = self.text_items.remove(&index).unwrap_or_default();
                json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": text }]
                })
            }
            BlockKind::Thinking => json!({ "type": "reasoning" }),
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
        };
        vec![named(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": index,
                "item": item
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
        if !self.model.is_empty() {
            response["model"] = json!(self.model);
        }
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

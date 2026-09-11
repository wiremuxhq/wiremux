//! Item-centered IR and LossReport.

/// Protocol-neutral conversation. Hosts convert to/from their own types.
#[derive(Clone, Debug, PartialEq)]
pub struct IrRequest {
    pub model: String,
    pub items: Vec<IrItem>,
    pub tools: Vec<IrTool>,
    pub sampling: IrSampling,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IrSampling {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub tool_choice: IrToolChoice,
    pub parallel_tool_calls: Option<bool>,
    pub store: Option<bool>,
    pub previous_response_id: Option<String>,
    pub cache: IrCache,
    /// Client asked for SSE. Grok TUI always sets this.
    pub stream: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum IrToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IrCache {
    pub enabled: bool,
    /// "5m" or "1h" when the target dialect has a TTL slot.
    pub retention: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum IrItem {
    System {
        text: String,
    },
    /// Chat Completions `developer` degrades to this on Messages.
    Developer {
        text: String,
    },
    User {
        parts: Vec<IrPart>,
    },
    Assistant {
        parts: Vec<IrPart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionOutput {
        call_id: String,
        output: String,
    },
    Reasoning {
        encrypted: Option<String>,
        summary: Option<String>,
        raw: Option<serde_json::Value>,
    },
    HostedToolCall {
        kind: String,
        raw: serde_json::Value,
    },
    Unknown {
        type_name: String,
        raw: serde_json::Value,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrPart {
    Text(String),
    ImageUrl(String),
    ImageBase64 {
        media_type: String,
        data: String,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum IrTool {
    Function {
        name: String,
        description: String,
        parameters: serde_json::Value,
    },
    Namespace {
        name: String,
        raw: serde_json::Value,
    },
    Hosted {
        kind: String,
        raw: serde_json::Value,
    },
    Unknown {
        type_name: String,
        raw: serde_json::Value,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum IrStreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ReasoningSignature {
        signature: String,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgDelta {
        delta: String,
    },
    ToolCallEnd,
    /// Exclusive buckets (same as Bline `Usage`): prompt excludes cache
    /// read, completion excludes reasoning. Encoders re-inflate inclusive
    /// wire totals for Chat and Responses.
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
        reasoning_tokens: u32,
    },
    FinishReason {
        reason: String,
    },
    Protocol {
        item_type: String,
        payload: serde_json::Value,
    },
    Unknown {
        event: String,
        raw: serde_json::Value,
    },
    Done,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LossReport {
    pub events: Vec<LossEvent>,
}

impl LossReport {
    /// Append a loss event.
    pub fn record(
        &mut self,
        path: impl Into<String>,
        action: LossAction,
        detail: impl Into<String>,
    ) {
        self.events.push(LossEvent {
            path: path.into(),
            action,
            detail: detail.into(),
        });
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LossEvent {
    pub path: String,
    pub action: LossAction,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LossAction {
    Preserve,
    Degrade,
    Drop,
    HardError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ir_request_holds_system_user_and_function_tool() {
        let req = IrRequest {
            model: "gpt-4".into(),
            items: vec![
                IrItem::System {
                    text: "You are helpful.".into(),
                },
                IrItem::User {
                    parts: vec![IrPart::Text("Hi".into())],
                },
            ],
            tools: vec![IrTool::Function {
                name: "lookup".into(),
                description: "Look up a term.".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            sampling: IrSampling::default(),
        };

        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.items.len(), 2);
        assert!(matches!(req.items[0], IrItem::System { .. }));
        assert!(matches!(
            &req.items[1],
            IrItem::User { parts } if parts == &[IrPart::Text("Hi".into())]
        ));
        assert_eq!(req.tools.len(), 1);
        assert!(matches!(
            &req.tools[0],
            IrTool::Function { name, description, .. }
                if name == "lookup" && description == "Look up a term."
        ));
        assert!(matches!(req.sampling.tool_choice, IrToolChoice::Auto));
    }

    #[test]
    fn loss_report_records_all_actions() {
        let mut report = LossReport::default();
        report.events.extend([
            LossEvent {
                path: "items[0]".into(),
                action: LossAction::Preserve,
                detail: "kept as-is".into(),
            },
            LossEvent {
                path: "items[1]".into(),
                action: LossAction::Degrade,
                detail: "developer to system".into(),
            },
            LossEvent {
                path: "sampling.store".into(),
                action: LossAction::Drop,
                detail: "no slot".into(),
            },
            LossEvent {
                path: "tools[0]".into(),
                action: LossAction::HardError,
                detail: "namespace".into(),
            },
        ]);

        let actions: Vec<_> = report.events.iter().map(|event| event.action).collect();
        assert_eq!(
            actions,
            [
                LossAction::Preserve,
                LossAction::Degrade,
                LossAction::Drop,
                LossAction::HardError,
            ]
        );
    }

    #[test]
    fn ir_tool_namespace_is_first_class() {
        let tool = IrTool::Namespace {
            name: "apply_patch".into(),
            raw: serde_json::json!({"type": "namespace"}),
        };
        assert!(matches!(
            &tool,
            IrTool::Namespace { name, .. } if name == "apply_patch"
        ));
        assert!(!matches!(tool, IrTool::Function { .. }));
    }

    #[test]
    fn ir_item_developer_is_distinct_from_system() {
        let system = IrItem::System {
            text: "rules".into(),
        };
        let developer = IrItem::Developer {
            text: "rules".into(),
        };
        assert_ne!(system, developer);
        assert!(matches!(system, IrItem::System { .. }));
        assert!(matches!(developer, IrItem::Developer { .. }));
    }

    #[test]
    fn ir_stream_event_usage_does_not_invent_cached_tokens() {
        let usage = IrStreamEvent::Usage {
            prompt_tokens: 20,
            completion_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        };
        match usage {
            IrStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
            } => {
                assert_eq!(prompt_tokens, 20);
                assert_eq!(completion_tokens, 5);
                assert_eq!(cache_read_tokens, 0);
                assert_eq!(cache_write_tokens, 0);
                assert_eq!(reasoning_tokens, 0);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }
}

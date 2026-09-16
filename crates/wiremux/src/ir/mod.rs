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
    /// Host max output tokens.
    ///
    /// Chat Completions encode may write `max_completion_tokens` and omit
    /// temperature (`LossReport`) for o1/o3/o4/gpt-5; decode accepts either key.
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub tool_choice: IrToolChoice,
    pub parallel_tool_calls: Option<bool>,
    /// Chat Completions and Responses `store`. Messages and Gemini drop.
    pub store: Option<bool>,
    pub previous_response_id: Option<String>,
    pub cache: IrCache,
    /// Client asked for SSE. Grok TUI always sets this.
    pub stream: Option<bool>,
    /// Gemini `thinkingConfig.includeThoughts`; Messages `thinking.type`;
    /// Responses `reasoning.summary` (`auto` when true). Chat Completions drops.
    pub include_thoughts: Option<bool>,
    /// Gemini `thinkingConfig.thinkingBudget` (decode and encode).
    /// Messages encode emits this as `thinking.budget_tokens` when
    /// `max_reasoning_tokens` is unset. Messages decode leaves this `None`
    /// and writes `budget_tokens` into `max_reasoning_tokens`.
    pub thinking_budget: Option<u32>,
    /// Dialect effort string (`low`, `high`, `xhigh`). Not a host enum.
    /// Gemini encode emits `thinkingConfig.thinkingLevel` (lowercase;
    /// `xhigh` / `x-high` degrade to `high`). Converse encode emits
    /// `outputConfig.effort` (`low`/`medium`/`high`/`xhigh`/`max`;
    /// `x-high` degrades to `xhigh`).
    pub reasoning_effort: Option<String>,
    /// Host cap on reasoning tokens.
    ///
    /// Messages decode writes `thinking.budget_tokens` here
    /// (`thinking_budget` stays `None`). Messages encode emits this as
    /// `thinking.budget_tokens`. Gemini encode emits it as
    /// `thinkingConfig.thinkingBudget` when `thinking_budget` is unset.
    /// Chat Completions and Responses have no emit slot.
    pub max_reasoning_tokens: Option<u32>,
    /// JSON schema for structured output when the dialect has a slot.
    pub json_schema: Option<serde_json::Value>,
    /// Optional schema name (Responses `text.format.name` / Chat json_schema.name /
    /// Converse `outputConfig.textFormat.structure.jsonSchema.name`).
    pub json_schema_name: Option<String>,
    /// Responses `include` extras (file_search results, etc.). Empty default.
    pub include: Vec<String>,
    /// OpenAI / Codex `prompt_cache_key`. Chat Completions and Responses
    /// emit it. Messages and Gemini drop.
    pub prompt_cache_key: Option<String>,
    /// OpenAI / Codex `service_tier` (`flex`, `priority`, `auto`).
    /// Chat Completions and Responses emit it. Converse emits
    /// `serviceTier.type` (`flex` / `priority` / `reserved` /
    /// `default`; `auto` degrades to `default`). Messages and Gemini drop.
    pub service_tier: Option<String>,
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
    /// Skip `cache_control` when estimated prompt tokens are below this floor.
    /// `None` or `0` means no floor.
    pub min_cacheable_tokens: Option<u32>,
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
        /// Gemini `thoughtSignature` on the same functionCall part.
        thought_signature: Option<String>,
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

#[derive(Clone, Debug, PartialEq)]
pub enum IrPart {
    Text(String),
    ImageUrl(String),
    ImageBase64 {
        media_type: String,
        data: String,
    },
    /// Messages replay needs a nonempty signature. Responses remaps
    /// signed thinking to a reasoning item. Unsigned thinking is dropped
    /// on Messages, Responses, and Chat Completions.
    Thinking {
        text: String,
        signature: Option<String>,
    },
    /// Dialect-native block (Anthropic `redacted_thinking`).
    Raw {
        type_name: String,
        raw: serde_json::Value,
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
        /// Gemini `thoughtSignature` bound to this function call.
        thought_signature: Option<String>,
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

/// Estimate prompt tokens as `chars / 4` over IR text.
///
/// Counts system / developer / user / assistant text and thinking parts,
/// function-call names and args, function-output text, and function-tool
/// names, descriptions, and parameters.
#[must_use]
pub fn estimate_prompt_tokens(req: &IrRequest) -> u32 {
    let mut chars = 0usize;
    for item in &req.items {
        match item {
            IrItem::System { text } | IrItem::Developer { text } => chars += text.len(),
            IrItem::User { parts } | IrItem::Assistant { parts } => {
                for part in parts {
                    match part {
                        IrPart::Text(text) | IrPart::Thinking { text, .. } => {
                            chars += text.len();
                        }
                        _ => {}
                    }
                }
            }
            IrItem::FunctionCall {
                name, arguments, ..
            } => {
                chars += name.len() + arguments.len();
            }
            IrItem::FunctionOutput { output, .. } => {
                chars += output.len();
            }
            _ => {}
        }
    }
    for tool in &req.tools {
        if let IrTool::Function {
            name,
            description,
            parameters,
        } = tool
        {
            chars += name.len() + description.len() + parameters.to_string().len();
        }
    }
    (chars / 4) as u32
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
        report.record("items[0]", LossAction::Preserve, "kept as-is");
        report.record("items[1]", LossAction::Degrade, "developer to system");
        report.record("sampling.store", LossAction::Drop, "no slot");
        report.record("tools[0]", LossAction::HardError, "namespace");

        assert_eq!(
            report.events,
            [
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
    fn estimate_prompt_tokens_counts_developer_text() {
        let req = IrRequest {
            model: "claude".into(),
            items: vec![IrItem::Developer {
                text: "x".repeat(5000),
            }],
            tools: vec![],
            sampling: IrSampling::default(),
        };
        assert!(
            estimate_prompt_tokens(&req) >= 1024,
            "5000 developer chars must meet a 1024-token floor"
        );
    }

    #[test]
    fn estimate_prompt_tokens_counts_function_output() {
        let req = IrRequest {
            model: "claude".into(),
            items: vec![IrItem::FunctionOutput {
                call_id: "c1".into(),
                output: "x".repeat(5000),
            }],
            tools: vec![],
            sampling: IrSampling::default(),
        };
        assert!(
            estimate_prompt_tokens(&req) >= 1024,
            "5000 tool-result chars must meet a 1024-token floor"
        );
    }

    #[test]
    fn estimate_prompt_tokens_counts_thinking_text() {
        let req = IrRequest {
            model: "claude".into(),
            items: vec![IrItem::Assistant {
                parts: vec![IrPart::Thinking {
                    text: "x".repeat(5000),
                    signature: None,
                }],
            }],
            tools: vec![],
            sampling: IrSampling::default(),
        };
        assert!(
            estimate_prompt_tokens(&req) >= 1024,
            "5000 thinking chars must meet a 1024-token floor"
        );
    }
}

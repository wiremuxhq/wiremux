//! Item-centered IR and LossReport.

use std::collections::BTreeMap;

/// Protocol-neutral conversation. Hosts convert to/from their own types.
///
/// Build with [`IrRequest::new`]. Extra fields stay at their defaults.
///
/// ```
/// use wiremux::{IrItem, IrPart, IrRequest};
/// let req = IrRequest::new(
///     "gpt-4",
///     vec![IrItem::User {
///         parts: vec![IrPart::Text("Hi".into())],
///     }],
/// );
/// assert_eq!(req.model, "gpt-4");
/// assert!(req.tools.is_empty());
/// ```
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct IrRequest {
    pub model: String,
    pub items: Vec<IrItem>,
    pub tools: Vec<IrTool>,
    pub sampling: IrSampling,
}

impl IrRequest {
    /// Request with `model` and `items`. Tools and sampling are empty/default.
    ///
    /// Pass a string slice (`"gpt-4o"`). Do not write `model.into()`:
    /// hosts that also depend on `bytes` or `reqwest` see `E0283`.
    #[must_use]
    pub fn new(model: impl AsRef<str>, items: Vec<IrItem>) -> Self {
        Self {
            model: model.as_ref().to_owned(),
            items,
            tools: Vec::new(),
            sampling: IrSampling::default(),
        }
    }

    /// Set tools. For hosts that cannot use a struct literal.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<IrTool>) -> Self {
        self.tools = tools;
        self
    }

    /// Set sampling. For hosts that cannot use a struct literal.
    #[must_use]
    pub fn with_sampling(mut self, sampling: IrSampling) -> Self {
        self.sampling = sampling;
        self
    }
}

impl Default for IrRequest {
    fn default() -> Self {
        Self::new(String::new(), Vec::new())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
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
    /// Chat Completions and Responses `store`. Gemini emits request-root
    /// `store`. Messages drops.
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
    /// Unconstrained JSON object mode (Chat `response_format.type=json_object`,
    /// Responses `text.format.type=json_object`, Gemini `responseMimeType=
    /// application/json` without a schema). `json_schema` wins when both are set.
    pub json_object: Option<bool>,
    /// Responses `include` extras (file_search results, etc.). Empty default.
    pub include: Vec<String>,
    /// OpenAI / Codex `prompt_cache_key`. Chat Completions and Responses
    /// emit it. Messages and Gemini drop.
    pub prompt_cache_key: Option<String>,
    /// Chat Completions and Responses `prompt_cache_retention`
    /// (`24h` / `in_memory`). Distinct from Messages `IrCache.retention`.
    /// Gemini, Messages, and Converse drop.
    pub prompt_cache_retention: Option<String>,
    /// Chat / Responses `prompt_cache_options.mode` (`implicit` / `explicit`).
    pub prompt_cache_mode: Option<String>,
    /// Chat / Responses `prompt_cache_options.ttl` (`30m`). Distinct from Messages IrCache.retention.
    pub prompt_cache_ttl: Option<String>,
    /// Chat / Responses `top_logprobs` (0-20).
    pub top_logprobs: Option<u32>,
    /// Chat / Responses `moderation.model`.
    pub moderation_model: Option<String>,
    /// Chat / Responses `moderation.policy.input.mode` (`score` / `block`).
    pub moderation_input: Option<String>,
    /// Chat / Responses `moderation.policy.output.mode`.
    pub moderation_output: Option<String>,
    /// Chat / Responses `stream_options.include_obfuscation`.
    pub include_obfuscation: Option<bool>,
    /// Chat Completions and Responses `user`. Messages, Gemini, and
    /// Converse drop.
    pub user: Option<String>,
    /// Chat Completions `verbosity` and Responses `text.verbosity`
    /// (`low` / `medium` / `high`). Gemini, Messages, and Converse drop.
    pub verbosity: Option<String>,
    /// Chat Completions and Responses `safety_identifier`.
    /// Distinct from `user`. Gemini, Messages, and Converse drop.
    pub safety_identifier: Option<String>,
    /// Chat Completions and Responses `metadata` string map.
    /// Messages `metadata.user_id` stays on `user`. Gemini and Converse drop.
    pub metadata: BTreeMap<String, String>,
    /// Chat Completions `frequency_penalty`. Gemini `generationConfig.frequencyPenalty`.
    /// Messages, Responses, and Converse drop.
    pub frequency_penalty: Option<f32>,
    /// Chat Completions `presence_penalty`. Gemini `generationConfig.presencePenalty`.
    /// Messages, Responses, and Converse drop.
    pub presence_penalty: Option<f32>,
    /// Chat Completions `seed`. Gemini `generationConfig.seed`.
    /// Messages, Responses, and Converse drop.
    pub seed: Option<i64>,
    /// Chat Completions `n`. Gemini `generationConfig.candidateCount`.
    /// Messages, Responses, and Converse drop.
    pub n: Option<u32>,
    /// OpenAI / Codex `service_tier` (`flex`, `priority`, `auto`).
    /// Chat Completions and Responses emit it. Converse emits
    /// `serviceTier.type` (`flex` / `priority` / `reserved` /
    /// `default`; `auto` degrades to `default`). Gemini emits
    /// request-root `serviceTier` (`flex` / `priority` / `standard`;
    /// `default` / `auto` degrade to `standard`). Messages drops.
    pub service_tier: Option<String>,
    /// Dest Chat `modalities` (`text` / `audio`). Dest Gemini
    /// `generationConfig.responseModalities` (`TEXT` / `AUDIO`).
    /// Dest Messages, dest Responses, and dest Converse drop.
    pub output_modalities: Vec<String>,
    /// Dest Chat `audio.voice` (string or object `id`). Dest Gemini
    /// `generationConfig.speechConfig.voiceConfig.prebuiltVoiceConfig.voiceName`.
    /// Dest Messages, dest Responses, and dest Converse drop.
    pub audio_voice: Option<String>,
    /// Dest Chat `audio.format`. Dest Gemini speechConfig has no format
    /// (Chat encode defaults to `wav` when emitting `audio`). Dest
    /// Messages, dest Responses, and dest Converse drop.
    pub audio_format: Option<String>,
    /// Dest Chat `logprobs` boolean. Dest Gemini
    /// `generationConfig.responseLogprobs`. Distinct from `top_logprobs`.
    /// Dest Messages, dest Responses, and dest Converse drop.
    pub logprobs: Option<bool>,
    /// Dest Chat `logit_bias` token-id to bias (-100..100). Dest Responses,
    /// dest Gemini, dest Messages, and dest Converse drop.
    pub logit_bias: BTreeMap<String, f64>,
    /// Dest Chat `prediction` object (`type` + `content`). Dest Responses,
    /// dest Gemini, dest Messages, and dest Converse drop.
    pub prediction: Option<serde_json::Value>,
    /// Dest Chat `web_search_options` object. Dest Responses, dest Gemini,
    /// dest Messages, and dest Converse drop.
    pub web_search_options: Option<serde_json::Value>,
}

impl IrSampling {
    /// Build sampling by mutating defaults.
    ///
    /// External crates cannot use struct literals on this type.
    pub fn patch(update: impl FnOnce(&mut Self)) -> Self {
        let mut sampling = Self::default();
        update(&mut sampling);
        sampling
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum IrToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct IrCache {
    pub enabled: bool,
    /// "5m" or "1h" when the target dialect has a TTL slot.
    pub retention: Option<String>,
    /// Skip `cache_control` when estimated prompt tokens are below this floor.
    /// `None` or `0` means no floor.
    pub min_cacheable_tokens: Option<u32>,
}

impl IrCache {
    /// Enabled cache with no TTL and no token floor.
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Set Messages-style TTL (`5m` / `1h`).
    ///
    /// Pass a string slice (`"5m"`). Do not write `retention.into()`.
    #[must_use]
    pub fn with_retention(mut self, retention: impl AsRef<str>) -> Self {
        self.retention = Some(retention.as_ref().to_owned());
        self
    }

    /// Skip `cache_control` below this estimated prompt-token floor.
    #[must_use]
    pub fn with_min_cacheable_tokens(mut self, tokens: u32) -> Self {
        self.min_cacheable_tokens = Some(tokens);
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
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
#[non_exhaustive]
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
    /// Messages `document`, Chat/Responses `file`/`input_file`,
    /// Gemini PDF `inlineData`/`fileData`, Converse `document`.
    Document {
        source: IrDocumentSource,
        media_type: String,
        name: Option<String>,
    },
    /// Chat `input_audio` and Gemini `inlineData` with `audio/*`.
    Audio {
        data: String,
        format: String,
    },
}

/// Where a document payload lives.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum IrDocumentSource {
    Base64(String),
    Url(String),
    FileId(String),
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
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
#[non_exhaustive]
pub enum IrStreamEvent {
    TextDelta {
        text: String,
    },
    /// Dest Chat `delta.refusal` / `message.refusal`, dest Responses
    /// `response.refusal.delta` / output content `{ "type": "refusal" }`,
    /// and dest Messages complete `stop_details.explanation`.
    RefusalDelta {
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
        /// Parallel tool-call slot. Chat Completions `tool_calls[].index`.
        /// Other dialects use a block/output index or 0.
        index: u32,
    },
    ToolCallArgDelta {
        delta: String,
        /// Same slot as the matching [`Self::ToolCallStart`].
        index: u32,
    },
    ToolCallEnd,
    /// Dest Chat `message.annotations` url_citation and dest Responses
    /// `response.output_text.annotation.added`.
    AnnotationAdded {
        annotation: serde_json::Value,
    },
    /// Dest Chat `message.audio.data` and dest Responses `response.audio.delta`.
    AudioDelta {
        data: String,
    },
    /// Dest Chat `message.audio.transcript` and dest Responses
    /// `response.audio.transcript.delta`.
    AudioTranscriptDelta {
        text: String,
    },
    /// Dest Chat `tool_calls[].type=custom` and dest Responses
    /// `custom_tool_call` output item.
    CustomToolCallStart {
        id: String,
        name: String,
        index: u32,
    },
    /// Dest Chat custom tool `custom.input` and dest Responses
    /// `response.custom_tool_call_input.delta`.
    CustomToolCallInputDelta {
        delta: String,
        index: u32,
    },
    /// Dest Chat STREAM `choices[].logprobs.content` and dest Responses
    /// `response.output_text.delta.logprobs`. Dest Gemini
    /// `candidates[].logprobsResult`.
    Logprobs {
        content: serde_json::Value,
    },
    /// Dest Chat `created` and dest Responses `created_at` (unix seconds).
    Created {
        unix: i64,
    },
    /// Dest Chat `service_tier`, dest Responses `service_tier`, and dest
    /// Messages `usage.service_tier`.
    ServiceTier {
        tier: String,
    },
    /// Dest Chat `metadata` and dest Responses `metadata` string map.
    Metadata {
        metadata: BTreeMap<String, String>,
    },
    /// Dest Chat `moderation.input` / `moderation.output` (Chat-shaped
    /// `moderation_results` or `error`) and dest Responses the same keys.
    Moderation {
        input: Option<serde_json::Value>,
        output: Option<serde_json::Value>,
    },
    /// Exclusive buckets: prompt excludes cache
    /// read, completion excludes reasoning. Encoders re-inflate inclusive
    /// wire totals for Chat and Responses.
    /// `audio_tokens` is dest Chat `prompt_tokens_details.audio_tokens`
    /// and dest Gemini `promptTokensDetails` modality AUDIO. It is a
    /// subset of prompt, not subtracted from `prompt_tokens`. Dest Chat
    /// has no `image_tokens`; Gemini IMAGE details are an official Drop.
    /// `completion_audio_tokens` is dest Chat
    /// `completion_tokens_details.audio_tokens` and dest Gemini
    /// `candidatesTokensDetails` modality AUDIO. It is a subset of
    /// completion, not subtracted from `completion_tokens`.
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
        reasoning_tokens: u32,
        audio_tokens: u32,
        completion_audio_tokens: u32,
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct LossEvent {
    pub path: String,
    pub action: LossAction,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
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

    #[test]
    fn new_and_retention_take_str_next_to_bytes() {
        let _owned = bytes::Bytes::from_static(b"x");
        let req = IrRequest::new("gpt-4o", vec![]).with_sampling(IrSampling::patch(|s| {
            s.cache = IrCache::enabled().with_retention("5m");
        }));
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.sampling.cache.retention.as_deref(), Some("5m"));
        let owned = IrRequest::new(String::from("gpt-4o"), vec![]);
        assert_eq!(owned.model, "gpt-4o");
    }
}

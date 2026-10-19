# Host attach

How a host application depends on `wiremux` and `wiremux-auth`. This
repository does not add a host path-dep, wrapper, or request map.

Published crates.io is `0.11.6`. <!-- x-release-please-version -->
Current tag is `v0.11.6`. <!-- x-release-please-version -->

`wiremux-auth` is ready to pin (`Static` + `Profile`, IsolatedHome
behind `test-util`, shipped `anthropic-oauth` and
`openai-codex-oauth`). Dialect maps already exist:
`wiremux::{decode,encode}` for Chat Completions, Messages, Responses,
Gemini (`wire = "gemini"`), and Converse. Stream maps include
thoughtSignature and a Chat id-then-name assembler.

```rust
let opts = wiremux_auth::LoadOptions::default();
let profile = wiremux_auth::load_profile("anthropic", &opts)?;
let _provider = wiremux_auth::provider_from_profile(&profile)?;
let (ir, _decode_loss) = wiremux::decode(wiremux::Wire::ChatCompletions, bytes)?;
let (_body, _encode_loss) = wiremux::encode(wiremux::Wire::Messages, &ir, &profile)?;
```

`load_profile_for_wire` is the same attach when the host has a wire
and no catalog id. Auth-only is a valid first attach (`wiremux-auth`
alone). A missing or empty `$VAR`, `${VAR}`, or `{env:VAR}` unsets
that profile field instead of leaving the placeholder or a blank
host.

## Crate history

`shipped_profile_ids()`, `xai-grok-build-messages`, URL-first Grok
Build headers, and Messages `Continue.` shipped in crates.io `0.5.0`.
They were not in crates.io `0.4.0`. crates.io `0.6.0` adds
`list_models` `context_window`, the AWS default credential chain,
Converse `outputConfig` / `serviceTier`, `Wire` plus public IR
`#[non_exhaustive]`, and the extra shipped presets. crates.io
`0.7.0` adds transient connect vs timeout `Display` suffixes
([#181](https://github.com/wiremuxhq/wiremux/pull/181)). Lock the
catalog with `shipped_profile_ids()` instead of copying names.

## LLM layer is wiremux

Dialect maps, IR, TokenProviders (OAuth, static, GCP, Azure, AWS STS,
Copilot store and device login), profile catalog, and login engines
live in this workspace. The host must not keep a second production
copy of those.

The host still owns the agent loop, router, failover, wire logger,
and diagnose. Those are not LLM wire.

`IrRequest` is the LLM request. Do not grow a parallel host dialect
map. Do not `pub use wiremux::IrRequest as ChatRequest`. Public IR
and error enums (`IrItem`, `IrPart`, `IrToolChoice`, `IrStreamEvent`,
`LossAction`, `MapError`, `ClientError`) and the structs `IrRequest`,
`IrSampling`, and `LossReport` are `#[non_exhaustive]`. Hosts build
with `IrRequest::new(model, items)` and `IrSampling::default()` /
`IrSampling::patch`. `IrStreamEvent::ToolCallStart` and
`ToolCallArgDelta` carry `index`.

`wiremux-auth` must not depend on host types. `AuthError` stays
independent of `LlmError`.

## Order

Pin `wiremux-auth` first, then maps at the adapter boundary.

1. Depend on `wiremux-auth` from crates.io (or a path dep for local
   dogfood).
2. Wrap `wiremux_auth::TokenProvider` inside the host token type.
3. Map `AuthError` to the host auth error. `LockTimeout` /
   `EmptyWriteRefused` / `VendorRejected` stay distinguishable.
4. Map the host request type at the adapter boundary
   (`wiremux::{decode,encode}`). Use `default-features = false` so
   clap, tokio, and reqwest stay off the maps crate.
5. Ship the host change behind a feature flag or a single adapter
   call site so rollback is one host revert.
6. Delete the host's parallel TokenProviders and dialect
   conversions. Claude Code oat, IsolatedHome, `secret_store`, and
   host SigV4 Bedrock signing may stay in the host.

crates.io is the attach path for published hosts. Pin the current
published tag `v0.11.6` until the next cut: <!-- x-release-please-version -->

```toml
[dependencies]
wiremux-auth = "0.11.6" # x-release-please-version
wiremux = { version = "0.11.6", default-features = false } # x-release-please-version
```

`default-features = false` is maps plus re-exported profile types.
It does not pull clap, tokio, reqwest, hyper, jsonwebtoken, or
aws-lc. TokenProvider HTTP lives on `wiremux-auth` feature `net`,
which `wiremux` features `client`, `cli`, and `proxy` enable.

Local dogfood may use a path dependency on `crates/wiremux-auth`
instead.

## Wrapper

- The host owns a `wiremux_auth::AnyTokenProvider` (or
  `ProfileTokenProvider`) behind the existing host `TokenProvider`
  trait.
- `get_token`, `mark_stale`, and `wake` forward.
- Every `AuthError` becomes the host auth error. Typed variants
  (`LockTimeout`, `EmptyWriteRefused`, `VendorRejected`,
  `CredentialStoreStale`) stay
  distinguishable so diagnose can tell flake from "re-run
  setup-token".
- Construction is `load_profile(id)` then `provider_from_profile`.
  Shipped `anthropic-oauth` and `openai-codex-oauth` are files, not
  new enum variants.

## Then maps

Map requests at the host adapter boundary only:

| Host today | After attach |
|------------|--------------|
| In-tree dialect conversions | `wiremux::{decode,encode}` inside the adapter |
| Host request type | Unchanged. Adapter maps only |
| Host IR is function tools only | Still host IR. Loss and namespace policy live in wiremux; the adapter reports `LossReport` |

Maps-only hosts that need dest Chat `model` on complete JSON call
`encode_response_with_model`. Crate-root `encode_response` is the
empty-model wrapper and omits the key. STREAM uses
`StreamEncoder::with_model`. `encode_stream_event` encodes one IR
event and has no dest-model argument.

Do not `pub use` `IrRequest` as the host request type. The host
factory continues to construct adapters and still owns router and
failover. Wiremux does not become the router.

Gemini is `wire = "gemini"` in wiremux. The host adapter calls
`decode` / `encode` on `IrRequest`. It does not keep a second Gemini
map.

Gemini finish and block reasons decode to `IrStreamEvent::FinishReason`.
`reason` is the mapped word. `vendor` is the original token.

| Vendor token | `reason` | Notes |
|--------------|----------|-------|
| `STOP` | `stop` or `tool_calls` | `tool_calls` when the candidate has a function call |
| `MAX_TOKENS` | `max_tokens` | |
| `SAFETY`, `RECITATION`, `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII`, `IMAGE_SAFETY`, `LANGUAGE`, `OTHER` | `content_filter` | `vendor` keeps the token |
| `MALFORMED_FUNCTION_CALL` | `malformed_function_call` | Not a successful stop. The host decides whether the turn fails |
| any other `finishReason` | the vendor string | Not rewritten to `stop` |
| unknown `promptFeedback.blockReason` | `content_filter` | `vendor` is the unseen token, not only `content_filter` |

Host thinking slots and `LossReport`:

| IR | Messages | Gemini | Responses | Chat Completions |
|----|----------|--------|-----------|------------------|
| Signed `IrPart::Thinking` | Replay `thinking` + `signature` | `thought` + `thoughtSignature` | Peel to `reasoning` | Drop |
| Unsigned `IrPart::Thinking` | Drop (not on wire) | `thought` (no signature) | Drop | Drop |
| `max_reasoning_tokens` | `thinking.budget_tokens` | `thinkingBudget` when `thinking_budget` unset | Drop | Drop |
| `include_thoughts` | `thinking.type` | `includeThoughts` | `reasoning.summary=auto` | Drop |
| `reasoning_effort` | `output_config.effort` when the trimmed string is non-empty and `include_thoughts` is not false | `thinkingLevel` | `reasoning.effort` | `reasoning_effort` |

Chat Completions and Responses emit `store`. OpenRouter Responses
`openrouter-codex` refuses `store` (`forbidden_field_policy =
hard-error`). Chat Completions `openrouter` does not refuse
`store`. Gemini writes `store` on the body and records Preserve
`gemini store` when `sampling.store` is `Some`. Messages has no slot.

Diagnose should print `LossReport` for `part.thinking` and
`sampling.max_reasoning_tokens`.

Messages encode: an empty or whitespace-only assistant turn becomes
one text block `"."`. Anthropic rejects empty text. Adapters take
the crate choice. Locked by
`messages_whitespace_only_assistant_becomes_dot`.

Messages encode also appends one user text turn `Continue.` when the
last IR item is Assistant, FunctionCall, or CustomToolCall. xAI
sxs-claude on the Grok Build proxy rejects assistant-last. Empty and
user-last IR stay unchanged. Chat Completions, Gemini, and Responses
do not append. Locked by
`messages_encode_appends_continue_on_assistant_last` and
`messages_encode_appends_continue_on_custom_tool_call_last`.

Messages `min_cacheable_tokens` uses `estimate_prompt_tokens`
(`chars / 4`). The count includes image bytes, document text and
base64, and tool-result parts that Messages writes. An image URL, a
document URL, and a document file id count as at least 4096 characters
(1024 tokens), because the string on the wire is a handle, not the
billed bytes. A longer handle still counts its own length. It skips
unsigned thinking (`unsigned thinking is not replayed`) and
whitespace-only text parts. Audio stays uncounted. Hosted items and
unknown raw JSON stay uncounted, so a short prompt is not treated as
long. When the user turn has no text block, the breakpoint is placed
on the image, document, or tool result Messages wrote. A thinking
block is not a breakpoint. An empty-turn `.` does not take the
breakpoint away from a following tool result.

## Stay in the host

| Stay in the host | Why |
|------------------|-----|
| Agent loop | Not LLM wire. |
| Router, failover, wire logger, diagnose, repair | Host control plane. |
| OS wake watcher | Host installs the watcher. It must call `TokenProvider::wake` (or `mark_stale`) on the wiremux provider. Do not keep a second TokenProvider just for wake. |
| Account / provider UI | Host config. Values feed a wiremux profile. |
| Claude Code oat, IsolatedHome, `secret_store`, SigV4 Bedrock | Host-only after attach. |

These belong in wiremux, not a second host copy:

| Surface | Where |
|---------|-------|
| `GcpTokenProvider`, `AzureTokenProvider`, `AwsStsTokenProvider` | `wiremux-auth` |
| Copilot device-flow login | Engine + gist already here. Persist is `copilot-hosts`. |
| `min_cacheable_tokens` / `estimate_prompt_tokens` | `IrCache` |
| Unknown name + Anthropic protocol | `load_profile_for_wire(Wire::Messages)`. Dialect skeleton (no `[oauth]`), not a shipped vendor pack. Load `anthropic-oauth` by catalog id for Anthropic OAuth. |
| Dialect maps / host request conversions | `wiremux::{decode,encode}` |

A refresh-only host should call `token_for_profile_cached` on a
catalog id when it only wants the stored Bearer ("is login
present?"). That helper does not POST `token_url`. Call
`token_for_profile` when the host wants a refresh. Keep
`provider_for_profile` for `mark_stale` / `wake`. Those helpers take
a catalog id, not a file path. Do not wrap host types here.

On a version bump, pin-lock `wiremux_auth::shipped_profile_ids()`
instead of copying catalog names by hand. The list is the same table
`load_profile` walks.

## WireClient (optional `client` feature)

`default-features = false` stays maps-only. Hosts that want POST/SSE
without clap or the `proxy` stack enable feature `client` on crate
`wiremux` only.

A 200 body that is not JSON, including an HTML page, is a vendor error
from `send` and `list_models`. A stream that never becomes a frame uses
that same vendor error. The message is the page text, not a JSON parse
error.

Maps-only hosts that read SSE themselves call `stream_has_terminal`
(or `frame_is_terminal` per frame) after the HTTP body ends. EOF
after content is a failure unless one frame was terminal. Chat is
terminal on `data: [DONE]` or a non-empty `finish_reason`. Messages
is terminal on `message_stop` or a `message_delta` with `stop_reason`.
A Messages citation with a URL becomes an annotation. A document
citation such as `char_location` has no URL and stays a protocol
event, so the cited text is not dropped. The proxy sends that frame
on when the destination is Messages, and does not send it to Chat.
Responses is terminal on `response.completed`, `response.incomplete`,
or `response.failed`. A Responses `error` event fails the stream with
the vendor message. Text already received in that read is kept.
`server_error`, `internal_error`, `connection_failed`,
`request_timeout`, `server_is_overloaded`, and `server_overloaded`
are `ClientError::Transient` with `TransientKind::Http`.
A Messages `api_error` is that same transient error.
`slow_down` and `rate_limit_exceeded` are `ClientError::RateLimit`.
`insufficient_quota` stays `ClientError::Vendor`. An auth message
stays `ClientError::Auth`, and a `model_not_found` message stays
`ClientError::NotFound`. `response.failed` uses `last_error.code`
and `last_error.message` the same way. A code or a message does not
finish as `failed` when that frame has no payload the stream decoder
emits. Non-empty `encrypted_content` and a hosted item with a payload
are delivered and are not a client error. Assistant text, a tool call,
or audio that shows up only on that failed frame still returns the
vendor error. `decode_stream_events` returns that same error, including
when the frame has no SSE `event:` line and the name is only `type`.
The proxy sends the vendor error and does not finish that stream as
`failed`. A failed frame with neither a code nor a message still
finishes as `failed`.
A 200 body with an error is a vendor error when that body has no leaf.
An empty Responses message, an empty `output_text`, an empty audio
item, and a type-only hosted item have no leaf. Non-empty
`encrypted_content`, reasoning text, and a hosted item that carries
an id or another payload do. An empty Messages text or thinking block
has no leaf. Text, a typed citation, a thinking signature, and a named
tool call do. Converse text, a named tool, and a non-empty
`stopReason` do. `response.completed` and `response.failed` use that
same leaf check, so an empty message shell keeps the vendor message
and encrypted reasoning stays on the completion.
`response.output_text.done`, `response.content_part.done`,
`response.function_call_arguments.done`, `response.refusal.done`,
`response.audio.done`, and `response.audio.transcript.done` are
recognized and do not repeat the matching delta. The same is true
for `response.reasoning_summary_part.added`,
`response.reasoning_summary_part.done`,
`response.reasoning_summary_text.done`, `response.reasoning_text.done`,
and `response.custom_tool_call_input.done`.
`response.reasoning_text.delta` is reasoning text, same as
`response.reasoning.delta`. Built-in tool progress
(`web_search_call`, `file_search_call`, `code_interpreter_call`,
`image_generation_call`, and `mcp_call` status frames) stays a
protocol event instead of failing the stream. On `hard-error`, a
Responses event that is not on the allowlist and carries no new text,
arguments, code, command, or image bytes is skipped.
`response.queued` and `response.compaction.compacting` are that kind
of frame. The same unknown event still fails when it carries `delta`,
`arguments`, `code`, `command`, `text`, `partial_image_b64`, or
`output` (shell output).
Passthrough still forwards that frame as an unknown event. Gemini
is terminal on a non-empty `finishReason`,
or on a non-empty `promptFeedback.blockReason` when that frame has no
candidate `finishReason`. Converse is terminal when a frame decodes to
`FinishReason`. An empty frame list is not terminal. `RawSse::parse_all`
returns an error when a line or `data:` payload exceeds the SSE size
cap. It does not turn that document into an empty frame list.
`StreamEncoder::finish()` is for a stream the caller already knows
completed. Do not call it just because the socket closed.

`ClientError::Transient` carries `TransientKind` (`Connect`,
`Timeout`, `Reset`, `Http`). Hosts call `is_connect()` /
`is_timeout()` / `is_reset()` instead of scraping Display
([#216](https://github.com/wiremuxhq/wiremux/issues/216)).
`is_connect()` is suite-abort (never reached the host).
`is_timeout()` and `is_reset()` are after connect; do not abort a
probe suite. Display text is unchanged from 0.7.0.

`decode_response` treats an empty Chat choice or an empty Gemini
candidate as a vendor error when an `error` field is set and that
slot has no text, refusal, audio bytes or transcript, or tool call.
An `image_url` or `input_audio` part with no URL, data, or transcript
does not count. `tool_calls: [{}]` does not count. A call with an id,
a name, or arguments still wins, on `decode_response` and on
`decode_stream_events`. A numeric `code` or `status` is part of the
error text. A choice or candidate that already has that payload stays
a completion. Messages `{"error":{"message":"..."}}` with no `type`
and no content blocks is the same kind of error. Content blocks still
win.

A Converse HTTP 200 JSON exception (`__type` and `message`) is
`ClientError::Vendor` with status 200 on `send` and on `stream`,
not `ClientError::Map` and not an Event Stream CRC error.
`ThrottlingException` is `ClientError::RateLimit`. An internal
exception is `ClientError::Transient`. A Converse JSON completion
(`output` and `stopReason`) on `stream` is decoded, including when
the proxy is same-wire.

A profile `chat_path` may carry a query. Gemini, Vertex Messages,
and Converse streaming rewrites keep that query. Gemini appends
`alt=sse` with `&` when a query is already present.

`WireClient::from_profile` loads a catalog id (shipped `base_url`,
`chat_path`, `auth_scheme`, `[headers]`, `[betas]`) and
`provider_for_profile`. `send` encodes IR, POSTs, and decodes a
complete JSON body, and returns the encode `LossReport`. `stream`
remaps SSE frames and drops that report. `stream_with_loss` returns
the same report before the first event. An encode error from
`stream_with_loss` fails before any HTTP call. HTTP and SSE errors
stay on the event stream. `list_models` GETs
the models catalog. OpenAI-compat uses the chat version prefix,
including directories in front of it.
`/v1/chat/completions` and `/v1/messages` become `{base}/v1/models`.
`/openai/v1/chat/completions` becomes `{base}/openai/v1/models`.
`/api/v3/chat/completions` becomes `{base}/api/v3/models`.
Azure OpenAI's shipped chat path,
`/openai/deployments/{model}/chat/completions?api-version=2024-10-21`,
lists `{base}/openai/models?api-version=2024-10-21`. The `api-version`
comes from that chat path. `{base}/models` is not the Azure list.
The GET sends the profile `api-key` header. A 200 body with `data[].id`
is the catalog. A 404 on the Azure URL is still an empty catalog.
A Vertex path keeps `publishers/.../models`, so
`/v1/projects/p/locations/us-central1/publishers/google/models/{model}:generateContent`
becomes `{base}/v1/projects/p/locations/us-central1/publishers/google/models`,
not `{base}/v1/models`.
It reads `context_length` then `context_window`
(Grok Build `/v1/models` sends the second key). That second key
is not in crates.io `0.5.0`. `list_models` retries HTTP 401 once
when the provider can refresh, same as `send`. A static token still
returns an auth error. The GET is SigV4-signed when the profile sets
`aws_service` and no bearer token is applied. A profile that cannot
sign does not look like an empty catalog. A 404 after the request is
sent, including a signed one, is still an empty catalog.
A profile file that will not load is `ClientError::Transport`, not
`Auth`. A missing catalog id stays `NotFound`.

Converse `toolResult.status` is `error` when the IR tool result is
`is_error`. A missing status is success, on encode and on decode.
An unknown role inside `messages` is a loss. The top-level `system`
array is unchanged.

Refresh-only hosts call `token_for_profile_cached`. A URL-first host
whose `base_url` host is `cli-chat-proxy.grok.com` (trailing-dot FQDN
too) still gets the Grok Build header pack. The version is
`GROK_CHAT_PROXY_CLIENT_VERSION`. This crate's identifier is
`GROK_CHAT_PROXY_CLIENT_IDENTIFIER`. A handmade profile that already
sets either header keeps that value. Other hosts send their own
identifier with the same version. `https://api.x.ai` does not get
those headers.

Shipped catalog ids:

| Common host flag | wiremux catalog id |
|------------------|--------------------|
| `--provider xai` / `--provider grok` | `xai` / `xai-oauth` |
| `--provider grok-build` | `xai-grok-build` |
| `--provider grok-build-messages` | `xai-grok-build-messages` |
| claude + API key | `anthropic` |
| claude, no key | `anthropic-oauth` |
| openai | `openai` |
| openai Responses API key | `openai-codex` |
| openai Codex OAuth (login disabled) | `openai-codex-oauth` |
| openrouter Chat Completions | `openrouter` |
| openrouter Responses API key | `openrouter-codex` |
| gemini | `gemini` |
| lmstudio | `lmstudio` |
| vllm | `vllm` |

Shipped `openai-codex-oauth` is catalog-present with `login = none`
and empty `client_id`. `wiremux auth login` exits not-ready. Hosts
that need Codex use `openai-codex` (API key). Do not fill a product
client id.

Also shipped: `grok-ollama`,
`xai-oauth` (`https://api.x.ai`), `xai-grok-build` (Grok Build CLI
proxy `https://cli-chat-proxy.grok.com`, Chat Completions, same
empty-client `oidc-auth-json` pack as `xai-oauth`, plus
`x-grok-client-version` set to `GROK_CHAT_PROXY_CLIENT_VERSION`
so the proxy does not return HTTP 426; `0.1.202` was rejected and
that body names `1.0.13` as the floor), and `xai-grok-build-messages`
(same host and pack, Messages at `/v1/messages`). crates.io `0.6.0` also ships Bedrock, Azure,
Vertex, Groq, Together, Mistral, DashScope, Moonshot, Zhipu, and
the rest of `shipped_profile_ids()`. Lock that function on a
version bump. Do not hand-copy the name list. Hosts that need a
non-default sxs / composer model set `x-grok-model-override` on
`[headers]`. Do not ship leftover host model ids. Key ids use
top-level `access_env` (first non-empty wins). `lmstudio` and
`vllm` are `auth_scheme = none`. Do not ship a product client id
on either xAI OAuth profile.

## Strict tools and Gemini bounds

A strict Chat or Responses tool, and a strict `json_schema` response,
sets `additionalProperties` to false and lists every property in
`required` on every object. That includes `$defs`, combinators, and
tuple `items`, not only the root.

Messages and Converse do the same when `strict` is true. Converse
also fills `properties: {}` on a bare object. Gemini has no
`additionalProperties` slot. A strict Gemini tool lists every
property in `required` and the loss report drops
`strict additionalProperties`. Gemini also drops
`exclusiveMinimum` and `exclusiveMaximum`. It does not write that
number as `minimum` or `maximum`.

## Out of scope

- No public Claude-Pro-in-Codex, Cline, or OpenCode preset.
  Fingerprint pack is data; do not ship the spoof.
- No third published crate.

Product README and architecture live at the repository root and in
`docs/ARCHITECTURE.md`. This page stays the host attach contract.

## Fields added in 0.11.2

`IrItem::System`, `IrItem::Developer`, `IrItem::Assistant`, and
`IrPart::ImageUrl` gained fields in crates.io `0.11.2`. A struct
literal from `0.11.1` does not compile (`E0063`).
`#[non_exhaustive]` does not let an outside crate omit them.
Set a field you do not use to `None` or an empty vec.

A pull request on the 0.11 line that adds another field to one of
these variants fails the Semver check before publish. The baseline
is the latest `v0.11.*` tag, not the pull request base, so the
baseline does not already contain the new field. `IrSampling` is
different: it is a `#[non_exhaustive]` struct, and hosts build it
with `IrSampling::patch` from `Default`. A new field there, left
unset by `patch`, still passes.

Chat `image_url.detail` of `low` or `high` is kept when the URL is
`data:`. Same-wire Chat writes `detail` back. `auto` stays omitted.
Messages, Gemini, and Converse still receive the image bytes and
record `part.image.detail`. An `https` URL with `detail` is unchanged.

Chat tool-call argument deltas that arrive before the id and the
name stay on that index. The host sees `ToolCallStart`, then the
fragments in order. If the stream ends with arguments and never
sends an id or a name, those fragments are still released. Parallel
indexes do not take each other's arguments.

```rust
IrItem::System {
    text: "You are concise.".into(),
    prompt_cache_breakpoint: None,
    content_parts: vec![],
}
IrItem::Developer {
    text: "Prefer short answers.".into(),
    prompt_cache_breakpoint: None,
    content_parts: vec![],
}
IrItem::Assistant {
    parts: vec![IrPart::Text("Hi".into())],
    phase: None,
}
IrPart::ImageUrl {
    url: "https://example.com/a.png".into(),
    detail: None,
    bucket_owner: None,
    file_id: None,
}
```

`prompt_cache_breakpoint` is the Chat mode on a one-part system or
developer role. `content_parts` keeps a later breakpoint on that role.
`phase` is the Responses assistant phase (`commentary` or
`final_answer`). `bucket_owner` is the Converse S3 account id.
`file_id` is the Responses input image file id. Chat, Messages,
Converse, and Gemini have no slot for `file_id`.

## Pin

Published hosts pin crates.io `0.11.6`. <!-- x-release-please-version -->
Matching tag is `v0.11.6`. <!-- x-release-please-version -->
Published hosts stay on `0.11.6` until they choose a later crates.io cut. <!-- x-release-please-version -->

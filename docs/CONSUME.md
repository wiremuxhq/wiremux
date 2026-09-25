# Host attach

How a host application depends on `wiremux` and `wiremux-auth`. This
repository does not add a host path-dep, wrapper, or request map.

Published crates.io is `0.9.1`. <!-- x-release-please-version -->
Current tag is `v0.9.1`. <!-- x-release-please-version -->

`wiremux-auth` is ready to pin (`Static` + `Profile`, IsolatedHome
behind `test-util`, shipped `anthropic-oauth` and
`openai-codex-oauth`). Dialect maps already exist:
`wiremux::{decode,encode}` for Chat Completions, Messages, Responses,
and Gemini (`wire = "gemini"`). Stream maps include thoughtSignature
and a Chat id-then-name assembler.

Auth-only is a valid first attach (`wiremux-auth` alone).

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
published tag `v0.9.1` until the next cut: <!-- x-release-please-version -->

```toml
[dependencies]
wiremux-auth = "0.9.1" # x-release-please-version
wiremux = { version = "0.9.1", default-features = false } # x-release-please-version
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
  (`LockTimeout`, `EmptyWriteRefused`, `VendorRejected`) stay
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

Host thinking slots and `LossReport`:

| IR | Messages | Gemini | Responses | Chat Completions |
|----|----------|--------|-----------|------------------|
| Signed `IrPart::Thinking` | Replay `thinking` + `signature` | `thought` + `thoughtSignature` | Peel to `reasoning` | Drop |
| Unsigned `IrPart::Thinking` | Drop (not on wire) | `thought` (no signature) | Drop | Drop |
| `max_reasoning_tokens` | `thinking.budget_tokens` | `thinkingBudget` when `thinking_budget` unset | Drop | Drop |
| `include_thoughts` | `thinking.type` | `includeThoughts` | `reasoning.summary=auto` | Drop |
| `reasoning_effort` | Budget defaults only | `thinkingLevel` | `reasoning.effort` | `reasoning_effort` |

Chat Completions and Responses emit `store`. OpenRouter Responses
`openrouter-codex` refuses `store` (`forbidden_field_policy =
hard-error`). Chat Completions `openrouter` does not refuse
`store`. Messages and Gemini have no slot.

Diagnose should print `LossReport` for `part.thinking` and
`sampling.max_reasoning_tokens`.

Messages encode: an empty or whitespace-only assistant turn becomes
one text block `"."`. Anthropic rejects empty text. Adapters take
the crate choice. Locked by
`messages_whitespace_only_assistant_becomes_dot`.

Messages encode also appends one user text turn `Continue.` when the
last IR item is Assistant or FunctionCall. xAI sxs-claude on the Grok
Build proxy rejects assistant-last. Empty and user-last IR stay
unchanged. Chat Completions, Gemini, and Responses do not append.
Locked by `messages_encode_appends_continue_on_assistant_last`.

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

`ClientError::Transient` carries `TransientKind` (`Connect`,
`Timeout`, `Reset`, `Http`). Hosts call `is_connect()` /
`is_timeout()` / `is_reset()` instead of scraping Display
([#216](https://github.com/wiremuxhq/wiremux/issues/216)).
`is_connect()` is suite-abort (never reached the host).
`is_timeout()` and `is_reset()` are after connect; do not abort a
probe suite. Display text is unchanged from 0.7.0.

`WireClient::from_profile` loads a catalog id (shipped `base_url`,
`chat_path`, `auth_scheme`, `[headers]`, `[betas]`) and
`provider_for_profile`. `send` encodes IR, POSTs, and decodes a
complete JSON body. `stream` remaps SSE frames. `list_models` GETs
the models catalog. OpenAI-compat uses the chat version prefix
(`{base}/v1/models` when `chat_path` is `/v1/chat/completions` or
`/v1/messages`). It reads `context_length` then `context_window`
(Grok Build `/v1/models` sends the second key). That second key
is not in crates.io `0.5.0`.

Refresh-only hosts call `token_for_profile_cached`. A URL-first host
whose `base_url` host is `cli-chat-proxy.grok.com` (trailing-dot FQDN
too) still gets the Grok Build header pack (`x-grok-client-version`
and `x-grok-client-identifier`) even when the profile is handmade or
an overlay. `https://api.x.ai` does not get those headers.

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
`x-grok-client-version = 0.1.202` so the proxy does not return HTTP
426), and `xai-grok-build-messages` (same host and pack, Messages at
`/v1/messages`). crates.io `0.6.0` also ships Bedrock, Azure,
Vertex, Groq, Together, Mistral, DashScope, Moonshot, Zhipu, and
the rest of `shipped_profile_ids()`. Lock that function on a
version bump. Do not hand-copy the name list. Hosts that need a
non-default sxs / composer model set `x-grok-model-override` on
`[headers]`. Do not ship leftover host model ids. Key ids use
top-level `access_env` (first non-empty wins). `lmstudio` and
`vllm` are `auth_scheme = none`. Do not ship a product client id
on either xAI OAuth profile.

## Out of scope

- No public Claude-Pro-in-Codex, Cline, or OpenCode preset.
  Fingerprint pack is data; do not ship the spoof.
- No third published crate.

Product README and architecture live at the repository root and in
`docs/ARCHITECTURE.md`. This page stays the host attach contract.

## Pin

Published hosts pin crates.io `0.9.1`. <!-- x-release-please-version -->
Matching tag is `v0.9.1`. <!-- x-release-please-version -->
Published hosts stay on `0.9.1` until they choose a later crates.io cut. <!-- x-release-please-version -->

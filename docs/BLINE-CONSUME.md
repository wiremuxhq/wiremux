# Bline consume spike notes

Written plan only. No Bline code lands in this repository. Bline wrap,
adapter swap, and leftover delete already landed in
[blineai/bline#3992](https://github.com/blineai/bline/pull/3992)
([`21b56488`](https://github.com/blineai/bline/commit/21b56488a94f338682f1c69a1d541053da8f8481)).
Do not dest-parent-copy Bline sources here.

## Status

This is the extract-side plan for design K13. It does not add a path
dependency, a wrapper, or a request map in this repository.

`wiremux-auth` is ready to pin (`Static` + `Profile`, IsolatedHome
behind `test-util`, shipped `anthropic-oauth` and
`openai-codex-oauth`). Dialect maps already exist on main:
`wiremux::{decode,encode}` for Chat Completions, Messages, Responses,
and Gemini (`wire = "gemini"`). Stream maps include thoughtSignature
and a Chat id-then-name assembler.

Auth-only is still a valid first attach (`wiremux-auth` alone). Maps
no longer have to wait.

Bline leftover-prove first pinned both crates in
[blineai/bline#3939](https://github.com/blineai/bline/pull/3939), then
git tag
[v0.1.0](https://github.com/wiremuxhq/wiremux/releases/tag/v0.1.0)
in [blineai/bline#3960](https://github.com/blineai/bline/pull/3960).
[blineai/bline#3991](https://github.com/blineai/bline/pull/3991)
re-pinned leftover-prove to crates.io `0.2.1` (`default-features =
false`). That leftover-prove reported no new crate map or auth bugs.
[blineai/bline#3992](https://github.com/blineai/bline/pull/3992)
then wrapped `wiremux_auth::TokenProvider`, swapped adapters onto
`wiremux::{decode,encode}`, and deleted leftover production
TokenProviders and named host conversions.
[blineai/bline#3988](https://github.com/blineai/bline/issues/3988)
is CLOSED.

Bline pins crates.io `0.3.0` with `wiremux` `default-features = false`
after [blineai/bline#3996](https://github.com/blineai/bline/pull/3996)
([`934e6235`](https://github.com/blineai/bline/commit/934e6235135b79a61730215d615d7f731ad07138)).
That leftover-prove filed no new crate bugs. [#104](https://github.com/wiremuxhq/wiremux/issues/104)
was already open. [#103](https://github.com/wiremuxhq/wiremux/issues/103)
was already tracked and is now CLOSED.

The wiremux README stays:

```
# wiremux

Not ready.
```

Do not add a launch pitch, GitHub topics, or an About string when this
plan is executed.

## LLM layer is wiremux

Dialect maps, IR, TokenProviders (OAuth, static, GCP, Azure, AWS STS,
Copilot store and device login), profile catalog, and login engines
live in this workspace. After the Bline consume PR they must not keep
a second production copy. Bline production now wraps
`wiremux_auth::TokenProvider` and encodes through
`wiremux::{decode,encode}`. leftover-prove tests stay in Bline.

Bline (the host) still owns the agent loop, router, failover, wire
logger, and `bline diagnose`. Those are not LLM wire.

`IrRequest` is the LLM request. Do not grow a parallel Bline dialect
map. A thin host-type shim is allowed only until Bline deletes
`ChatRequest`. Do not `pub use wiremux::IrRequest as ChatRequest`.

`wiremux-auth` must not depend on `bline-types`. `AuthError` stays
independent of `LlmError`.

## Order

Path-dep `wiremux-auth` first, then maps at the adapter boundary.

1. Bline path-deps `wiremux-auth` (local path for dogfood, then a
   pinned git SHA). First leftover prove pin landed in Bline #3939 /
   #3960 (`v0.1.0`). Crates.io leftover-prove re-pin landed in Bline
   #3991 (`0.2.1`).
2. Wrap `wiremux_auth::TokenProvider` inside `bline_auth::TokenProvider`.
   Done in Bline #3992. leftover-prove tests still call the crate;
   `bline-auth` production now wraps it.
3. Map `AuthError` to `LlmError::Auth`. Done in Bline #3992 (same
   wrap PR). LockTimeout / EmptyWriteRefused / VendorRejected stay
   distinguishable.
4. Map `ChatRequest` at the `bline-llm` adapter boundary
   (`wiremux::{decode,encode}`). Use `default-features = false` so
   clap, tokio, and reqwest stay off the maps crate. Done in Bline
   #3992.
5. Ship the Bline change behind a feature flag or a single adapter
   call site so rollback is one Bline revert. Done in Bline #3992
   (adapter call site).
6. After steps 2-5, delete Bline's parallel TokenProviders and
   dialect conversions. Done in Bline #3992. Host leftover request
   structs remain as test fixtures. Claude Code oat, IsolatedHome,
   `secret_store`, and host SigV4 Bedrock signing stay in Bline.

crates.io is an attach path for published hosts. Bline and other
published hosts pin tag
[v0.3.0](https://github.com/wiremuxhq/wiremux/releases/tag/v0.3.0):

```toml
[dependencies]
wiremux-auth = "0.3.0"
wiremux = { version = "0.3.0", default-features = false }
```

Bline #3991 leftover-prove used `0.2.1`. Bline #3996 bumped the
workspace pin to `0.3.0` after wrap.

## Suggested attach (Bline crate, not this repo)

Bline is on the crates.io form above. Other unpublished hosts may
still git-pin a tag. `default-features = false` is maps plus
re-exported profile types. It does not pull clap, a fat tokio, or
reqwest on the `wiremux` crate. `wiremux-auth` still has its own
reqwest for TokenProvider.

Bline `deny.toml` has `unknown-git = deny` and `allow-git` for workpen
only. Bline #3991 dropped the `wiremuxhq/wiremux` git allow row.

Local dogfood may use a path dependency on `crates/wiremux-auth`
instead.

Wrapper that landed in Bline #3992 (do not land it here):

- `bline_auth` owns a `wiremux_auth::AnyTokenProvider` (or
  `ProfileTokenProvider`) behind the existing Bline `TokenProvider`
  trait.
- `get_token`, `mark_stale`, and `wake` forward.
- Every `AuthError` becomes `LlmError::Auth`. Typed variants
  (`LockTimeout`, `EmptyWriteRefused`, `VendorRejected`) stay
  distinguishable in the mapped message or a host-side match so
  diagnose can tell flake from "re-run setup-token".
- Construction is `load_profile(id)` then `provider_from_profile`.
  Shipped `anthropic-oauth` and `openai-codex-oauth` are files, not
  new enum variants.

Do not take a `bline-types` dependency in `wiremux-auth`.

## Then maps

Bline maps requests at the `bline-llm` adapter boundary only:

| Bline today | After consume |
|-------------|----------------|
| In-tree `to_resp_message` / Anthropic conversions | `wiremux::{decode,encode}` inside the adapter |
| `ChatRequest` in `bline-types` | Unchanged. Adapter maps in `bline-llm` only |
| Host IR is function tools only | Still host IR. Loss and namespace policy live in wiremux; the adapter reports `LossReport` |

Do not `pub use` `IrRequest` as `ChatRequest`. The factory continues
to construct Bline adapters and still owns router and failover.
Wiremux does not become the router.

Gemini is `wire = "gemini"` in wiremux. The host adapter calls
`decode` / `encode` on `IrRequest`. It does not keep a second Gemini
map.

Host thinking slots and `LossReport` (adapters and `bline diagnose`):

| IR | Messages | Gemini | Responses | Chat Completions |
|----|----------|--------|-----------|------------------|
| Signed `IrPart::Thinking` | Replay `thinking` + `signature` | `thought` + `thoughtSignature` | Peel to `reasoning` | Drop |
| Unsigned `IrPart::Thinking` | Drop (not on wire) | `thought` (no signature) | Drop | Drop |
| `max_reasoning_tokens` | `thinking.budget_tokens` | `thinkingBudget` when `thinking_budget` unset | Drop | Drop |
| `include_thoughts` | `thinking.type` | `includeThoughts` | `reasoning.summary=auto` | Drop |
| `reasoning_effort` | Budget defaults only | `thinkingLevel` | `reasoning.effort` | `reasoning_effort` |

Chat Completions and Responses emit `store`. OpenRouter
`forbidden_body_fields` still strips it. Messages and Gemini have no slot.

Diagnose should print `LossReport` for `part.thinking` and
`sampling.max_reasoning_tokens`.

Messages encode: an empty or whitespace-only assistant turn becomes
one text block `"."`. Anthropic rejects empty text. The old Bline
host map used `"[empty]"` or omitted the block. After consume,
adapters take the crate choice. Locked by
`messages_whitespace_only_assistant_becomes_dot`.

## Stay in Bline (host only)

| Stay in Bline | Why |
|---------------|-----|
| Agent loop | Not LLM wire. |
| Router, failover, wire logger, diagnose, `repair.rs` | Host control plane. |
| OS wake watcher | Host installs the watcher. It must call `TokenProvider::wake` (or `mark_stale`) on the wiremux provider. Do not keep a Bline TokenProvider just for wake. |
| Account / `ProviderConfig` UI | Host config. Values feed a wiremux profile. |
| Claude Code oat, IsolatedHome, `secret_store`, SigV4 Bedrock | Host-only after #3992. |

Do **not** leave these in Bline:

| Was "later" | Now |
|-------------|-----|
| `GcpTokenProvider`, `AzureTokenProvider`, `AwsStsTokenProvider` | `wiremux-auth` |
| Copilot device-flow login | Engine + gist already here. Persist is `copilot-hosts`. |
| `min_cacheable_tokens` / `estimate_prompt_tokens` | `IrCache` |
| Unknown name + Anthropic protocol | `load_profile_for_wire(Wire::Messages)`. Dialect skeleton (no `[oauth]`), not a shipped vendor pack. Load `anthropic-oauth` by catalog id for Anthropic OAuth. |
| Dialect maps / `ChatRequest` conversions | `wiremux::{decode,encode}` |

## Host follow-up (Bline, after this crate has the APIs)

Bline already wraps TokenProvider and encodes through this crate on
crates.io `0.3.0`. leftover-prove tests stay in Bline
(`leftover_wiremux` / `leftover_3988`). The 0.2.1 wrap landed in
#3992; the 0.3.0 pin landed in #3996. Do not implement a Bline
bump here.

Canact consume of `wiremux-auth` is a later canact PR, not a wiremux
PR and not part of this spike. A refresh-only host (canact or
otherwise) should call `token_for_profile` on a catalog id, or keep
`provider_for_profile` for `mark_stale` / `wake`. Those helpers take a
catalog id, not a file path. Do not wrap host types here.

## WireClient (optional `client` feature)

`default-features = false` stays maps-only. Hosts that want POST/SSE
without clap or the `proxy` stack enable feature `client` on crate
`wiremux` only.

`WireClient::from_profile` loads a catalog id (shipped `base_url`,
`chat_path`, `auth_scheme`, `[headers]`, `[betas]`) and
`provider_for_profile`. `send` encodes IR, POSTs, and decodes a
complete JSON body. `stream` remaps SSE frames. `list_models` GETs
the models catalog. OpenAI-compat uses the chat version prefix
(`{base}/v1/models` when `chat_path` is `/v1/chat/completions` or
`/v1/messages`).

Refresh-only hosts still call `token_for_profile`. Do not wrap Bline
types and do not run `wiremux proxy` for that path.

Shipped catalog ids and the canact mapping:

| canact | wiremux catalog id |
|--------|--------------------|
| `--provider xai` | `xai` |
| claude + API key | `anthropic` |
| claude, no key | `anthropic-oauth` |
| openai | `openai` |
| openrouter Chat Completions | `openrouter` |
| gemini | `gemini` |
| lmstudio | `lmstudio` |
| vllm | `vllm` |

Also shipped, unchanged: `grok-ollama`, `openai-codex-oauth`,
`openrouter-codex`. Key ids use top-level `access_env` (first non-empty
wins). `lmstudio` and `vllm` are `auth_scheme = none`.

## Out of scope

- No public Claude-Pro-in-Codex, Cline, or OpenCode preset. Fingerprint
  pack is data; do not ship the spoof. Consume must not add that
  preset in Bline either.
- No dest-parent-copy of Bline sources into this repo.
- No third published crate.
- No launch pitch. README remains `Not ready.`
- Do not start the Bline path-dep in this repo.

## Rollback

Bline stays on crates.io `0.3.0`. Rollback of the consume spike is
revert the Bline #3992 commit (then the #3996 pin if needed).
leftover-prove tests remain. crates.io versions `0.3.0` match tag
`v0.3.0`. Published hosts pin those versions until they choose a
later crates.io cut.

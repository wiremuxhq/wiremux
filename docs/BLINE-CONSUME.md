# Bline consume spike notes

Written plan only. No Bline code lands in this repository. The first
leftover prove already landed in Bline. Adapter swap is a later Bline
PR against `blineai/bline`.

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

The first leftover prove is in Bline, not in this tree.
[blineai/bline#3939](https://github.com/blineai/bline/pull/3939) pins
both crates at
[`4d939fe87cdf0a3ce9eba599810520eeaf55b6b9`](https://github.com/wiremuxhq/wiremux/commit/4d939fe87cdf0a3ce9eba599810520eeaf55b6b9)
with `wiremux` `default-features = false`. Adapter swap of
`to_resp_message` / Anthropic conversions is still later.

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
live in this workspace. Bline does not keep a second copy.

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
   pinned git SHA). Done in Bline #3939.
2. Wrap `wiremux_auth::TokenProvider` inside `bline_auth::TokenProvider`.
   Done in Bline #3939.
3. Map `AuthError` to `LlmError::Auth`. Done in Bline #3939.
4. Map `ChatRequest` at the `bline-llm` adapter boundary
   (`wiremux::{decode,encode}`). Use `default-features = false` so
   clap, tokio, and reqwest stay off the maps crate. Still later.
5. Ship the Bline change behind a feature flag or a single adapter
   call site so rollback is one Bline revert. Still later.

This workspace has `publish = false`. crates.io is not the attach path.

## Suggested attach (Bline crate, not this repo)

First leftover prove (both crates, or later `main`):

```toml
[dependencies]
wiremux-auth = { git = "https://github.com/wiremuxhq/wiremux", package = "wiremux-auth", rev = "4d939fe87cdf0a3ce9eba599810520eeaf55b6b9" }
wiremux = { git = "https://github.com/wiremuxhq/wiremux", package = "wiremux", rev = "4d939fe87cdf0a3ce9eba599810520eeaf55b6b9", default-features = false }
```

`default-features = false` is maps plus re-exported profile types.
It does not pull clap, a fat tokio, or reqwest on the `wiremux`
crate. `wiremux-auth` still has its own reqwest for TokenProvider.

Bline `deny.toml` has `unknown-git = deny` and `allow-git` for workpen
only. A git pin needs `https://github.com/wiremuxhq/wiremux` on that
allow list. That change lives in Bline, not this repo.

Local dogfood may use a path dependency on `crates/wiremux-auth`
instead.

Wrapper sketch (illustrative; do not land it here):

- `bline_auth` owns a `wiremux_auth::AnyTokenProvider` (or
  `ProfileTokenProvider`) behind the existing Bline `TokenProvider`
  trait.
- `get_token` and `mark_stale` forward.
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

## Stay in Bline (host only)

| Stay in Bline | Why |
|---------------|-----|
| Agent loop | Not LLM wire. |
| Router, failover, wire logger, diagnose, `repair.rs` | Host control plane. |
| OS wake watcher | Host installs the watcher. It must call `TokenProvider::wake` (or `mark_stale`) on the wiremux provider. Do not keep a Bline TokenProvider just for wake. |
| Account / `ProviderConfig` UI | Host config. Values feed a wiremux profile. |

Do **not** leave these in Bline:

| Was "later" | Now |
|-------------|-----|
| `GcpTokenProvider`, `AzureTokenProvider`, `AwsStsTokenProvider` | `wiremux-auth` |
| Copilot device-flow login | Engine + gist already here. Persist is `copilot-hosts`. |
| `min_cacheable_tokens` / `estimate_prompt_tokens` | `IrCache` |
| Unknown name + Anthropic protocol | `load_profile_for_wire(Wire::Messages)` |
| Dialect maps / `ChatRequest` conversions | `wiremux::{decode,encode}` |

## Host follow-up (Bline, after this crate has the APIs)

Bline deletes its parallel TokenProviders and dialect conversions
once it pins a SHA that exports them. That is a Bline PR. The APIs
must exist here first.

Canact consume of `wiremux-auth` is a later canact PR, not a wiremux
PR and not part of this spike.

## Out of scope

- No public Claude-Pro-in-Codex, Cline, or OpenCode preset. Fingerprint
  pack is data; do not ship the spoof. Consume must not add that
  preset in Bline either.
- No dest-parent-copy of Bline sources into this repo.
- No third published crate.
- No launch pitch. README remains `Not ready.`
- Do not start the Bline path-dep in this repo.

## Rollback

This workspace is still `publish = false`. Bline stays on the last
good path-dep SHA. Rollback of the consume spike is revert the Bline
commit. Bline adapters remain.

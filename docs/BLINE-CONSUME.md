# Bline consume spike notes

Written plan only. No Bline code lands in this repository. The consume
work is a later Bline PR against `blineai/bline`.

## Status

This is the extract-side plan for design K13. It does not add a path
dependency, a wrapper, or a request map in either repo.

`wiremux-auth` is ready to pin (`Static` + `Profile`, IsolatedHome
behind `test-util`, shipped `anthropic-oauth` and
`openai-codex-oauth`). Dialect maps already exist on main:
`wiremux::{decode,encode}` for Chat Completions, Messages, Responses,
and Gemini (`wire = "gemini"`). Stream maps include thoughtSignature
and a Chat id-then-name assembler.

Auth-only is still a valid first attach (`wiremux-auth` alone). Maps
no longer have to wait.

Bline consume is not started. `blineai/bline` `Cargo.toml` has no
`wiremux` dep.

The wiremux README stays:

```
# wiremux

Not ready.
```

Do not add a launch pitch, GitHub topics, or an About string when this
plan is executed.

## K13: convert at the crate boundary

Bline keeps its own types and control plane:

- `ChatRequest` in `bline-types`
- the agent loop
- the factory, including router and failover
- the wire logger
- `bline diagnose`

Do not `pub use wiremux::IrRequest as ChatRequest`. Do not re-export
wiremux request types from `bline-types`. Call through and keep host
types (craftbag consume lesson).

`wiremux-auth` must not depend on `bline-types`. `AuthError` stays
independent of `LlmError`.

## Order

Path-dep `wiremux-auth` first, then maps at the adapter boundary.

1. Bline path-deps `wiremux-auth` (local path for dogfood, then a
   pinned git SHA).
2. Wrap `wiremux_auth::TokenProvider` inside `bline_auth::TokenProvider`.
3. Map `AuthError` to `LlmError::Auth`.
4. Map `ChatRequest` at the `bline-llm` adapter boundary
   (`wiremux::{decode,encode}`). Use `default-features = false` so
   clap, tokio, and reqwest stay off the maps crate.
5. Ship the Bline change behind a feature flag or a single adapter
   call site so rollback is one Bline revert.

This workspace has `publish = false`. crates.io is not the attach path.

## Suggested attach (Bline crate, not this repo)

Auth-only (valid from
[`8630a7f`](https://github.com/wiremuxhq/wiremux/commit/8630a7f0aa82d2343bfc4ff930ee2f3b52ceb4a3)):

```toml
[dependencies]
wiremux-auth = { git = "https://github.com/wiremuxhq/wiremux", package = "wiremux-auth", rev = "8630a7f0aa82d2343bfc4ff930ee2f3b52ceb4a3" }
```

Maps (optional clap/tokio/reqwest;
[`0977970a33e1`](https://github.com/wiremuxhq/wiremux/commit/0977970a33e1cba2869589bce35fa484c45c6a48)
or later on `main`):

```toml
wiremux = { git = "https://github.com/wiremuxhq/wiremux", package = "wiremux", rev = "0977970a33e1cba2869589bce35fa484c45c6a48", default-features = false }
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

Gemini is `wire = "gemini"` in wiremux. Bline still owns the
`ChatRequest` wrap and the adapter that calls decode/encode.

## Stay in Bline

These were leftovers in the extract design. They are not missing
wiremux APIs.

| Stay in Bline | Why |
|---------------|-----|
| `GcpTokenProvider`, `AzureTokenProvider`, `AwsStsTokenProvider` | DESIGN non-goal for v1. Same crate later. |
| `install_wake_monitor` / wake flag on `get_token` | Host laptop-wake policy. Wiremux has `mark_stale` only. The Bline wrapper can call `mark_stale` when the host wake flag is set. |
| Dedicated Copilot device-flow login | Gist + `copilot-hosts` store exist. Product ToS. Device login stays later. |
| `ChatRequest` and factory | K13. Adapter maps at the `bline-llm` boundary. |
| Unknown name + `protocol = "anthropic"` is `FactoryError::UnknownProvider` | Host factory change. Optional later: resolve a wiremux profile by `wire = "messages"`. |
| `PromptCacheConfig.min_cacheable_tokens` and `estimate_prompt_tokens` | Host floor (`chars / 4`). `IrCache` is only `enabled` + `retention`. |
| Router, failover, wire logger, diagnose, `repair.rs` | Out of scope. |

Also stay in Bline: agent loop, `bline auth login` presets (optionally
add a wiremux profile path later), account and provider config on
`ProviderConfig`.

## Bline follow-ups (not this extract)

Unknown name plus `protocol = "anthropic"` is still
`FactoryError::UnknownProvider` in Bline. The host may later resolve a
wiremux profile by `wire = "messages"`. That is a Bline change, not a
wiremux PR.

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

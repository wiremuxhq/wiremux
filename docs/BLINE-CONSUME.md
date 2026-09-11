# Bline consume spike notes

Written plan only. No Bline code lands in this repository. The consume
work is a later Bline PR against `blineai/bline`.

## Status

This is the extract-side plan for design K13. It does not add a path
dependency, a wrapper, or a request map in either repo.

`wiremux-auth` is the first attach surface (profile AST plus
`TokenProvider`). Dialect maps in `wiremux` come later in the extract.
Bline maps `ChatRequest` only after those maps exist.

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

Path-dep `wiremux-auth` first, then maps. Refresh was the first pain;
do not start with IR.

1. Bline path-deps `wiremux-auth` (local path for dogfood, then a
   pinned git SHA).
2. Wrap `wiremux_auth::TokenProvider` inside `bline_auth::TokenProvider`.
3. Map `AuthError` to `LlmError::Auth`.
4. After maps exist, map `ChatRequest` at the `bline-llm` adapter
   boundary (`wiremux::{decode,encode}`).
5. Ship the Bline change behind a feature flag or a single adapter
   call site so rollback is one Bline revert.

This workspace has `publish = false`. crates.io is not the attach path.

## Auth first

Today Bline's Claude Code path hardcodes client id, token URLs, and
credential load. After consume:

| Bline today | After consume |
|-------------|----------------|
| `ClaudeCodeTokenProvider` plus hardcoded URLs | `provider_from_oauth` / `provider_from_profile` plus shipped or overlay profile |
| Hardcoded OAuth beta on `sk-ant-oat` | Profile `[oauth]` / betas data |
| Token trait coupled to host errors | Wrap; map `AuthError` at the `bline-auth` facade |

Suggested attach (Bline crate, not this repo):

```toml
[dependencies]
wiremux-auth = { git = "https://github.com/wiremuxhq/wiremux", package = "wiremux-auth" }
```

Pin a SHA once the extract is stable. Local dogfood may use a path
dependency on `crates/wiremux-auth` instead.

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

After auth is path-dep'd and wrapped, Bline maps requests at the
`bline-llm` adapter boundary only:

| Bline today | After consume |
|-------------|----------------|
| In-tree `to_resp_message` / Anthropic conversions | `wiremux::{decode,encode}` inside the adapter |
| `ChatRequest` in `bline-types` | Unchanged. Adapter maps in `bline-llm` only |
| Host IR is function tools only | Still host IR. Loss and namespace policy live in wiremux; the adapter reports `LossReport` |

Do not `pub use` `IrRequest` as `ChatRequest`. The factory continues
to construct Bline adapters and still owns router and failover.
Wiremux does not become the router.

Gemini stays a Bline adapter. It is not a v1 `wire` value.

## What stays in Bline

These do not move into wiremux:

- Agent loop
- Factory, router, failover
- Wire logger
- `bline diagnose`
- `bline auth login` presets (optionally add a wiremux profile path later)
- Account and provider config on `ProviderConfig`

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

## Rollback

This workspace is still `publish = false`. Bline stays on the last
good path-dep SHA. Rollback of the consume spike is revert the Bline
commit. Bline adapters remain.

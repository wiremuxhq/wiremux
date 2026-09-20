# Architecture

Wiremux is two embeddable crates. A host depends on them in-process
and keeps its own agent loop, router, failover, and diagnostics.

## Crates

```
host application
  ├── wiremux          decode / encode / stream maps, optional CLI + proxy
  └── wiremux-auth     profile AST, catalog load, TokenProvider
```

`wiremux` may depend on `wiremux-auth`. Auth tests never compile IR.
Maps-only (`default-features = false`) keeps clap, tokio, reqwest,
and aws-lc off the maps crate.

There is no third published crate. The CLI binary lives in `wiremux`.

## IR

`IrRequest` is item-centered: model, items, tools, sampling. Hosts
build with `IrRequest::new(model, items)`. They must not
`pub use` Wiremux IR as the host request type. Map at the adapter
boundary and keep the host type unchanged.

Wires today:

| `Wire` | Catalog spelling | Typical path |
| --- | --- | --- |
| `ChatCompletions` | `chat-completions` | `/v1/chat/completions` |
| `Messages` | `messages` | `/v1/messages` |
| `Responses` | `responses` | `/v1/responses` |
| `Gemini` | `gemini` | `generateContent` |
| `Converse` | `converse` | `/converse` (`stream=true` rewrites to `/converse-stream`, AWS Event Stream) |

New dialects may land in a minor release. `Wire` is
`#[non_exhaustive]`. Hosts need a `_` match arm.

## Loss

Every map returns a `LossReport`. Actions are `preserve`, `degrade`,
`drop`, and `hard-error`. Policy lives on the profile
(`tool_type_policy`, `forbidden_body_fields`). A hard-error is never
a silent strip.

Thinking, citations, audio, and usage have official slots on some
wires and official drops on others. Consume notes keep the adapter
table.

## Profiles

The same TOML/JSON schema is used for shipped presets and user
files. Overlay merges layers that share `id` (shipped, then user
dir, then an explicit file). Different ids never merge.

Data only. Refuse functions, `!command`, and URL-as-script. Allow
`$VAR` / `${VAR}` / `{env:VAR}`.

OAuth is a profile, not a new enum variant. A gist can restore a
yanked vendor pack (token URL, client id, betas, fingerprint)
without a crate release.

## TokenProvider

`get_token`, `mark_stale`, and `wake`. Write-back is fail-closed
(0600, file lock, no empty-stub overwrite). Cloud providers
(GCP, Azure, AWS STS) and Copilot device login live here, not in
the host.

`IsolatedHome` is a `test-util` helper, not a runtime API.

## Proxy and CLI

`wiremux proxy` binds loopback only. It maps the incoming `--from`
dialect onto the profile target and forwards. Converse streaming is
AWS Event Stream, not SSE.

The CLI also lists, validates, and ingests profiles, and runs
`auth login` / `auth status`.

## Stays in the host

Agent loop, router, failover, wire logger, diagnose, account UI,
and OS wake watchers. The host must call `TokenProvider::wake` (or
`mark_stale`) on laptop wake. Do not keep a second TokenProvider
just for that.

## Non-goals

- Not a multi-tenant HTTP gateway.
- Not a desktop config switcher.
- Not a capability probe suite.
- No public Claude-Pro-in-Codex, Cline, or OpenCode preset.

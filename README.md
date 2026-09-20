# Wiremux

Map Chat Completions, Messages, Responses, Gemini, and Converse
through one in-process IR. Refresh tokens from the same pasteable
vendor profiles. The host keeps the agent loop.

## Crates

| Crate | Job |
| --- | --- |
| [`wiremux`](https://crates.io/crates/wiremux) | Dialect maps (`decode` / `encode` / stream) plus optional CLI and loopback proxy |
| [`wiremux-auth`](https://crates.io/crates/wiremux-auth) | Profile catalog and `TokenProvider` (static, OAuth, GCP, Azure, AWS) |

Maps-only hosts depend on `wiremux` with `default-features = false`.
That path does not pull clap, tokio, reqwest, or aws-lc.

This repository may be ahead of crates.io. Host attach notes in
[`docs/CONSUME.md`](docs/CONSUME.md) pin the published cut.

## Install

MSRV is 1.95 (see `rust-toolchain.toml`).

```bash
cargo add wiremux-auth
cargo add wiremux --no-default-features
```

CLI and proxy:

```bash
cargo install wiremux --locked
```

## Maps

Decode a vendor JSON body into `IrRequest`, then encode another wire.
Loss is typed (`preserve` / `degrade` / `drop` / `hard-error`), never
a silent strip.

```rust
use wiremux::{Wire, decode, encode, parse_profile_str};

let src = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"ping"}]}"#;
let (ir, _loss) = decode(Wire::ChatCompletions, src).unwrap();
let profile = parse_profile_str(
    r#"
schema_version = 1
id = "example-messages"
wire = "messages"
"#,
)
.unwrap();
let (_out, _loss) = encode(Wire::Messages, &ir, &profile).unwrap();
```

Run the same program from this tree:

```bash
cargo run -p wiremux --example remap --no-default-features
```

Wires: `chat-completions` (CLI also accepts `chat`), `messages`,
`responses`, `gemini`, `converse`. Profile TOML uses
`wire = "chat-completions"`, not `wire = "chat"`.

## Auth and profiles

Shipped vendors are ordinary TOML. A gist uses the same schema, so a
rotated token URL, client id, or beta header does not need a crate
release. Profiles are data: no functions, no `!command`, no
URL-as-script. `$VAR` / `${VAR}` / `{env:VAR}` are allowed.

```bash
wiremux profile list
wiremux profile validate openai
wiremux auth status openai
```

`wiremux auth login` follows the profile `login` engine. Several
shipped OAuth files use `login = "none"` until a public Wiremux
client id exists; those exit 2 with a setup hint.

Catalog ingest writes user-dir profiles from
[models.dev](https://models.dev/api.json) (default) or LiteLLM
(`--source litellm`). Catalog misses fail closed.

## Proxy

Loopback only (`127.0.0.1`). Incoming harness dialect is `--from`.
Upstream comes from `--profile`.

```bash
wiremux proxy --from chat-completions --profile ollama --dump-loss
```

`--dump-loss` prints a `LossReport` on stderr per request.

## Host attach

How a host application should pin, wrap `TokenProvider`, and map at
the adapter boundary: [`docs/CONSUME.md`](docs/CONSUME.md).

Architecture (IR, loss, what stays in the host):
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

Copy-paste examples: [`examples/`](examples/).

## Status

0.x. Public IR and error enums are `#[non_exhaustive]`. Dual license
MIT OR Apache-2.0.

This crate is not a LiteLLM clone, not a desktop switcher, and not a
subscription pool. It does not ship a public Claude-Pro-in-Codex,
Cline, or OpenCode preset.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). `make check` is the local
gate. Commits need `git commit -s` (DCO).

Security reports go to [`SECURITY.md`](SECURITY.md), not a public
issue.

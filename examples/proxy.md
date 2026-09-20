# Loopback proxy

`wiremux proxy` listens on `127.0.0.1` only. Incoming harness
dialect is `--from`. Upstream URL, auth, and target wire come from
`--profile`.

## Run against a local Chat Completions server

Use a shipped profile whose `base_url` points at localhost, or a
user overlay file. Example with the shipped `ollama` id (Ollama
must already be serving):

```bash
cargo install wiremux --locked
wiremux profile validate ollama
wiremux proxy --from chat-completions --profile ollama --dump-loss
```

The process prints the bound address. Point the harness at that
loopback URL. `--dump-loss` writes a `LossReport` to stderr for
each request.

`--from` values: `chat-completions` (or `chat`), `messages`,
`responses`, `gemini`, `converse`.

## Cross-dialect

A Messages harness in front of a Chat Completions upstream:

```bash
wiremux proxy --from messages --profile ollama --dump-loss
```

Converse with `stream=true` uses AWS Event Stream
(`/converse-stream`), not SSE. Bedrock auth is the AWS default
credential chain on the profile, not a Bearer paste.

## Limits

The listener refuses non-loopback binds. Do not put this binary on
a public interface. Host CSRF checks apply to `Host` / `Origin` on
that loopback listener.

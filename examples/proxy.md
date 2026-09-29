# Loopback proxy

`wiremux proxy` listens on `127.0.0.1` only. `localhost` in `--listen`
is that same address. Incoming harness dialect is `--from`. Upstream
URL, auth, and target wire come from `--profile`. The request path is
ignored. The `model` field is forwarded unless `--model` is set.

## Run against a local Chat Completions server

Use a shipped profile whose `base_url` points at localhost, or a
user overlay file. Example with the shipped `ollama` id (Ollama
must already be serving):

```bash
cargo install wiremux --locked
wiremux profile validate ollama
wiremux proxy --listen 127.0.0.1:8787 --from chat-completions --profile ollama
```

Point the harness at that fixed port:

```bash
export OPENAI_BASE_URL=http://127.0.0.1:8787/v1
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787
```

`--dump-loss` writes a `LossReport` to stderr for each request.

`--from` values: `chat-completions` (or `chat`), `messages`,
`responses`, `gemini`, `converse`.

## Cross-dialect

A Messages harness in front of Ollama speaks Messages and the
upstream speaks Chat Completions. Pass `--model` with an Ollama tag,
or set the harness model to that tag. Otherwise the harness model
id is what Ollama receives.

```bash
wiremux proxy --listen 127.0.0.1:8787 --from messages --model llama3.2 --profile ollama
```

The dest stream echoes `llama3.2` because that is the model the
proxy asked the upstream to run.

Converse with `stream=true` uses AWS Event Stream
(`/converse-stream`), not SSE. Bedrock auth is the AWS default
credential chain on the profile, not a Bearer paste.

## Limits

The listener refuses non-loopback binds. Do not put this binary on
a public interface. Host CSRF checks apply to `Host` / `Origin` on
that loopback listener.

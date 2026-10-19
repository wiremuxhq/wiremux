# Loopback proxy

`wiremux proxy` listens on `127.0.0.1` only. `localhost` in `--listen`
is that same address. Incoming harness dialect is `--from`. Upstream
URL, auth, and target wire come from `--profile`. A generation
POST ignores the request path. The `model` field is forwarded
unless `--model` is set. A path ending in `/count_tokens` or
`:countTokens` is a token count. The proxy forwards that count
only when `--from` and the profile wire are both Messages, or
both Gemini. A count across dialects is HTTP 501
`token count is not mapped across dialects`. A count URL for
the other of those two wires is HTTP 501
`this count URL does not match the profile wire`.

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

## Credentials

`auth status` and the proxy accept the same credential sources: an
`[oauth]` table, `access_env`, a `[headers]` value (`Authorization`,
`x-api-key`, or `auth_scheme = "header:..."`), `gcp_key_env`, or
`auth_scheme = "none"`. A header key such as the shipped
`openrouter-codex` profile does not need `[oauth]`.

A credential file that is missing when the proxy starts is tried again
on the next request, so `auth login` does not require a restart. A
missing credential is HTTP 401. A token endpoint that cannot be reached
is HTTP 502.

## Limits

The listener refuses non-loopback binds. Do not put this binary on
a public interface. Host CSRF checks apply to `Host` / `Origin` on
that loopback listener.

On a same-wire Chat Completions proxy, a generation HTTP 200 whose
body is only a vendor error is HTTP 400 with that JSON. When the
request set `stream` to true, the status stays 200 and the body is
one `event: error` frame. The `data` line is that same JSON, so
`error.type` and `error.code` both stay.

A POST larger than 8388608 bytes is refused with 413
`request body too large` before the proxy forwards it. When the
request included `Content-Length`, that 413 names the declared
length and the maximum.

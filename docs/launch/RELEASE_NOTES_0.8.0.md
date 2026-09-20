# Wiremux 0.8.0

Paste onto the GitHub Release after merging the 0.8.0 release PR.
Do not land this file as `RELEASE_NOTES.md` on `main`. At cut time,
push the orphan `release-note-0.8.0` branch as described in
CONTRIBUTING.md.

## Tagline

Wiremux maps Chat Completions, Messages, Responses, Gemini, and
Converse in-process, and refreshes tokens from pasteable vendor
profiles.

## For hosts

- `openai-codex` shipped as an API-key Responses profile
  (`OPENAI_API_KEY`). `openai-codex-oauth` stays `login = none`.
- `ClientError::Transient` exposes `TransientKind`. Match
  `is_connect()` / `is_timeout()` instead of parsing Display.
- Dest remap coverage on complete and stream paths: citations,
  audio, logprobs, moderation, `service_tier`, usage details,
  finish reasons, and Gemini / Converse / Messages / Responses
  identity slots that Chat Completions already listed.
- CLI `auth login` / `auth status` accept a positional profile id.

## Install

```toml
[dependencies]
wiremux-auth = "0.8.0"
wiremux = { version = "0.8.0", default-features = false }
```

Maps-only is `default-features = false`. Host attach:
https://github.com/wiremuxhq/wiremux/blob/v0.8.0/docs/CONSUME.md

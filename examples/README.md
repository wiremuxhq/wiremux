# Examples

Copy-paste programs for a first attach. They do not talk to a live
vendor.

## Maps-only remap

Source of truth: [`crates/wiremux/examples/remap.rs`](../crates/wiremux/examples/remap.rs).

Decode a Chat Completions body and encode Anthropic Messages:

```bash
cargo run -p wiremux --example remap --no-default-features
```

`default-features = false` is the maps-only path (no clap, tokio,
reqwest, or aws-lc).

Messages encode records `sampling.max_tokens` as Preserve because
Messages requires a budget. That is not a drop.

Gemini generateContent to Chat Completions:

```bash
cargo run -p wiremux --example remap_gemini --no-default-features
```

Source: [`crates/wiremux/examples/remap_gemini.rs`](../crates/wiremux/examples/remap_gemini.rs).

Gemini `responseModalities` IMAGE has no Chat image modality and
Drops. Gemini usage `promptTokensDetails` with modality IMAGE has
no Chat `image_tokens` (official Drop). AUDIO next to IMAGE still
remaps.

A host Cargo.toml looks like the pin in
[`docs/CONSUME.md`](../docs/CONSUME.md) (`default-features = false`
on `wiremux`).

Then wrap `TokenProvider` and call `decode` / `encode` at the
adapter boundary. Full attach order: [`docs/CONSUME.md`](../docs/CONSUME.md).

## Loopback proxy

See [`proxy.md`](proxy.md).

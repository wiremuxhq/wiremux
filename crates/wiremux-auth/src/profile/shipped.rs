//! Compiled-in shipped preset documents.
//!
//! Deleting an entry yanks that profile from a release. It does not remove
//! `load_profile` or `TokenProvider`.

/// TOML documents compiled from workspace `presets/`.
pub(crate) fn documents() -> &'static [&'static str] {
    &[
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../presets/anthropic-oauth.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../presets/openai-codex-oauth.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../presets/openrouter-codex.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../presets/grok-ollama.toml"
        )),
    ]
}

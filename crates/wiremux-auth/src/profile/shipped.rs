//! Compiled-in shipped preset documents.
//!
//! Deleting an entry yanks that profile from a release. It does not remove
//! `load_profile` or `TokenProvider`.

/// TOML documents compiled from crate-local `presets/`.
///
/// Workspace `presets/` stays the edit surface. A test fails if the
/// copies drift. `include_str!` cannot reach outside the package root
/// on crates.io.
pub(crate) fn documents() -> &'static [&'static str] {
    &[
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/anthropic-oauth.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/anthropic.toml"
        )),
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/presets/gemini.toml")),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/grok-ollama.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/lmstudio.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/openai-codex-oauth.toml"
        )),
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/presets/openai.toml")),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/openrouter-codex.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/openrouter.toml"
        )),
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/presets/vllm.toml")),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/xai-grok-build.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/presets/xai-oauth.toml"
        )),
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/presets/xai.toml")),
    ]
}

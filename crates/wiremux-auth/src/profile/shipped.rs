//! Compiled-in shipped preset documents.
//!
//! Deleting an entry yanks that profile from a release. It does not remove
//! `load_profile` or `TokenProvider`.

/// One shipped catalog row. `id` and `document` come from the same table
/// [`load_profile`] walks.
pub(crate) struct ShippedDocument {
    pub id: &'static str,
    pub document: &'static str,
}

macro_rules! shipped_presets {
    ($(($id:literal, $file:literal)),+ $(,)?) => {
        const DOCUMENTS: &[ShippedDocument] = &[
            $(
                ShippedDocument {
                    id: $id,
                    document: include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/presets/",
                        $file
                    )),
                }
            ),+
        ];

        /// Catalog ids from the same table [`load_profile`] walks.
        pub const SHIPPED_PROFILE_IDS: &[&str] = &[$($id),+];
    };
}

// Workspace presets/ stays the edit surface. A test fails if the
// crate-local copies drift. include_str! cannot reach outside the
// package root on crates.io.
shipped_presets! {
    ("anthropic-oauth", "anthropic-oauth.toml"),
    ("anthropic", "anthropic.toml"),
    ("gemini", "gemini.toml"),
    ("grok-ollama", "grok-ollama.toml"),
    ("lmstudio", "lmstudio.toml"),
    ("openai-codex-oauth", "openai-codex-oauth.toml"),
    ("openai", "openai.toml"),
    ("openrouter-codex", "openrouter-codex.toml"),
    ("openrouter", "openrouter.toml"),
    ("vllm", "vllm.toml"),
    ("xai-grok-build", "xai-grok-build.toml"),
    ("xai-grok-build-messages", "xai-grok-build-messages.toml"),
    ("xai-oauth", "xai-oauth.toml"),
    ("xai", "xai.toml"),
}

pub(crate) fn documents() -> &'static [ShippedDocument] {
    DOCUMENTS
}

/// Every shipped catalog id, in the same order `load_profile` walks.
///
/// Hosts pin-lock this list on a version bump instead of copying names
/// by hand.
pub fn shipped_profile_ids() -> &'static [&'static str] {
    SHIPPED_PROFILE_IDS
}

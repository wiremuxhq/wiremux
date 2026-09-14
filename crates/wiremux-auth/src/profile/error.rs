//! Profile parse, refuse, and catalog errors.

use std::io;
use std::path::PathBuf;

/// Failure from parse, refuse, or catalog load.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// Filesystem read failed.
    #[error("failed to read {}: {source}", path.display())]
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// TOML/JSON syntax or type error.
    #[error("failed to parse profile: {0}")]
    Parse(String),
    /// `schema_version` is greater than this crate understands.
    #[error(
        "unknown schema_version {found} (crate max {max}); upgrade wiremux or pin schema_version"
    )]
    SchemaVersion {
        /// Version found in the document.
        found: u32,
        /// Highest version this crate accepts.
        max: u32,
    },
    /// A required field is missing after env substitution.
    #[error("missing required field {0}")]
    MissingField(&'static str),
    /// A key name is reserved for code-exec (data-only profiles).
    #[error("refused: forbidden key name `{0}`")]
    ForbiddenKey(String),
    /// Command interpolation in a non-hint field value.
    #[error("refused: command interpolation in {field} ({trigger})")]
    Interpolation {
        /// Dotted field path.
        field: String,
        /// Syntax that triggered the refuse (`!`, `$(...)`, or backticks).
        trigger: &'static str,
    },
    /// Disallowed scheme or non-loopback `http://` URL.
    #[error(
        "refused: disallowed URL in {field}: {} (https, or http only on 127.0.0.1 / localhost / ::1)",
        crate::helpers::redact_url_origin(url)
    )]
    DisallowedUrl {
        /// Dotted field path.
        field: String,
        /// The refused URL text.
        url: String,
    },
    /// WASM or native-library path in a field value.
    #[error("refused: native module path in {field}")]
    NativeModule {
        /// Dotted field path.
        field: String,
    },
    /// No document in the catalog has this `id`.
    #[error("{}; a .toml or .json path also works", not_found_message(id, known))]
    NotFound {
        /// Requested document id.
        id: String,
        /// Catalog ids from the same load (shipped ∪ user dir ∪ explicit file).
        known: Vec<String>,
    },
}

/// Catalog miss: known ids, plus a close-match hint when unique.
pub fn not_found_message(id: &str, known: &[String]) -> String {
    let listed = if known.is_empty() {
        "(none)".to_string()
    } else {
        known.join(", ")
    };
    let mut msg = format!("profile `{id}` not found (known: {listed})");
    let refs: Vec<&str> = known.iter().map(String::as_str).collect();
    if let Some(suggest) = super::types::suggest_kebab(id, &refs) {
        msg.push_str(&format!("; did you mean `{suggest}`"));
    }
    msg
}

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
    #[error("refused: command interpolation in {field}")]
    Interpolation {
        /// Dotted field path.
        field: String,
    },
    /// Disallowed scheme or non-loopback `http://` URL.
    #[error("refused: disallowed URL in {field}: {url}")]
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
    #[error("profile `{0}` not found")]
    NotFound(String),
}

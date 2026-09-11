//! TokenProvider errors. Independent of any host `LlmError`.

use std::io;
use std::path::{Path, PathBuf};

/// Failure from credential load, refresh, or fail-closed write-back.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// A required field or credential source is missing.
    #[error("missing {0}")]
    MissingField(String),
    /// Filesystem read or write failed.
    #[error("I/O error{}: {source}", path.as_ref().map(|p| format!(" ({})", p.display())).unwrap_or_default())]
    Io {
        /// Path that failed, when known.
        path: Option<PathBuf>,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// JSON parse or serialize failed.
    #[error("JSON error{}: {source}", path.as_ref().map(|p| format!(" ({})", p.display())).unwrap_or_default())]
    Json {
        /// Path that failed, when known.
        path: Option<PathBuf>,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// Provider construction or refresh failed.
    #[error("{0}")]
    TokenProvider(String),
    /// Advisory file lock was not acquired in time.
    #[error("file lock timeout")]
    LockTimeout,
    /// Write-back refused an empty access token (or a non-object store).
    #[error("refusing to write empty or invalid credentials")]
    EmptyWriteRefused,
    /// Token endpoint rejected the refresh.
    #[error("vendor rejected token refresh (HTTP {status}): {summary}")]
    VendorRejected {
        /// HTTP status from the token endpoint.
        status: u16,
        /// Sanitized `error` / `error_description` only.
        summary: String,
    },
}

impl AuthError {
    pub(crate) fn io(path: Option<PathBuf>, source: io::Error) -> Self {
        Self::Io { path, source }
    }

    /// Label a JSON error with the store path. Path-less `?` still uses [`Self::Json`].
    pub(crate) fn json(path: impl AsRef<Path>, source: serde_json::Error) -> Self {
        Self::Json {
            path: Some(path.as_ref().to_path_buf()),
            source,
        }
    }
}

impl From<serde_json::Error> for AuthError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json { path: None, source }
    }
}

//! TokenProvider errors. Independent of any host `LlmError`.

use std::io;
#[cfg(any(feature = "net", test))]
use std::path::Path;
use std::path::PathBuf;

use crate::profile::ProfileError;

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
        Self::Io {
            path: path.map(redact_pathbuf),
            source,
        }
    }

    /// Label a JSON error with the store path. Path-less `?` still uses [`Self::Json`].
    #[cfg(any(feature = "net", test))]
    pub(crate) fn json(path: impl AsRef<Path>, source: serde_json::Error) -> Self {
        Self::Json {
            path: Some(redact_pathbuf(path.as_ref().to_path_buf())),
            source,
        }
    }
}

fn redact_pathbuf(path: PathBuf) -> PathBuf {
    PathBuf::from(crate::helpers::redact_secret_looking(
        &path.display().to_string(),
    ))
}

impl From<serde_json::Error> for AuthError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json { path: None, source }
    }
}

impl From<ProfileError> for AuthError {
    fn from(err: ProfileError) -> Self {
        match err {
            ProfileError::MissingField(s) => Self::MissingField(s.to_string()),
            ProfileError::Io { path, source } => Self::io(Some(path), source),
            other => Self::TokenProvider(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAK: &str = "ghp_ENVSUBST_LEAK_TOKEN_51";

    fn leak_path() -> PathBuf {
        PathBuf::from(format!("/tmp/creds-{LEAK}.json"))
    }

    #[test]
    fn auth_error_json_display_redacts_secret_in_path() {
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let err = AuthError::json(leak_path(), source);
        let display = err.to_string();
        assert!(
            !display.contains(LEAK),
            "JSON Display leaked token: {display}"
        );
        assert!(
            display.contains("[redacted]"),
            "JSON Display should redact: {display}"
        );
    }

    #[test]
    fn auth_error_io_display_redacts_secret_in_path() {
        let err = AuthError::io(
            Some(leak_path()),
            io::Error::new(io::ErrorKind::NotFound, "missing store"),
        );
        let display = err.to_string();
        assert!(
            !display.contains(LEAK),
            "Io Display leaked token: {display}"
        );
        assert!(
            display.contains("[redacted]"),
            "Io Display should redact: {display}"
        );
    }
}

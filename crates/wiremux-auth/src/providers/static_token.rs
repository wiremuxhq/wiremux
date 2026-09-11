//! Static API key that never expires.

use crate::TokenProvider;
use crate::error::AuthError;

/// A static API key. Used when no OAuth refresh is needed.
#[derive(Clone)]
pub struct StaticToken {
    key: String,
}

impl std::fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticToken")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl StaticToken {
    /// Wrap a raw key or access token.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

impl TokenProvider for StaticToken {
    async fn get_token(&self) -> Result<String, AuthError> {
        Ok(self.key.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_token_returns_key() {
        let provider = StaticToken::new("sk-test-key");
        assert_eq!(provider.get_token().await.unwrap(), "sk-test-key");
    }

    #[test]
    fn debug_redacts_key() {
        let provider = StaticToken::new("sk-secret-123");
        let debug = format!("{provider:?}");
        assert!(!debug.contains("sk-secret"));
        assert!(debug.contains("[REDACTED]"));
    }
}

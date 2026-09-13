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

    /// Missing or whitespace-only env is an error, not an empty key.
    pub fn from_env(var: &str) -> Result<Self, AuthError> {
        let missing = || AuthError::MissingField(format!("env `{var}`"));
        let value = std::env::var(var).map_err(|_| missing())?;
        if value.trim().is_empty() {
            return Err(missing());
        }
        Ok(Self::new(value))
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
    use crate::isolated_home::IsolatedHome;

    #[tokio::test]
    async fn static_token_returns_key() {
        let provider = StaticToken::new("sk-test-key");
        assert_eq!(provider.get_token().await.unwrap(), "sk-test-key");
    }

    #[tokio::test]
    async fn static_wake_is_noop() {
        let provider = StaticToken::new("sk-test-key");
        provider.wake();
        assert_eq!(provider.get_token().await.unwrap(), "sk-test-key");
    }

    #[test]
    fn debug_redacts_key() {
        let provider = StaticToken::new("sk-secret-123");
        let debug = format!("{provider:?}");
        assert!(!debug.contains("sk-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn from_env_reads_set_var() {
        let home = IsolatedHome::with_extra_envs(&["WIREMUX_TEST_STATIC_KEY"]);
        home.set_env("WIREMUX_TEST_STATIC_KEY", "sk-static-issue59");
        let token = StaticToken::from_env("WIREMUX_TEST_STATIC_KEY").expect("set key");
        assert_eq!(
            token.get_token().await.expect("static key"),
            "sk-static-issue59"
        );
        let debug = format!("{token:?}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(!debug.contains("sk-static-issue59"), "{debug}");
        let _ = home;
    }

    #[test]
    fn from_env_missing_var_is_error() {
        let home = IsolatedHome::with_extra_envs(&["WIREMUX_TEST_STATIC_KEY"]);
        let err = StaticToken::from_env("WIREMUX_TEST_STATIC_KEY").expect_err("missing");
        match err {
            AuthError::MissingField(name) => {
                assert!(
                    name.contains("WIREMUX_TEST_STATIC_KEY") && name.contains("env"),
                    "MissingField must name the env var, got {name}"
                );
            }
            other => panic!("expected MissingField, got {other}"),
        }
        let _ = home;
    }

    #[test]
    fn from_env_whitespace_only_is_error() {
        let home = IsolatedHome::with_extra_envs(&["WIREMUX_TEST_STATIC_KEY"]);
        home.set_env("WIREMUX_TEST_STATIC_KEY", "   \t");
        let err = StaticToken::from_env("WIREMUX_TEST_STATIC_KEY").expect_err("whitespace");
        match err {
            AuthError::MissingField(name) => {
                assert!(
                    name.contains("WIREMUX_TEST_STATIC_KEY") && name.contains("env"),
                    "MissingField must name the env var, got {name}"
                );
            }
            other => panic!("expected MissingField, got {other}"),
        }
        let _ = home;
    }
}

//! Shared OAuth token-exchange response (PKCE and device flow).

use serde::Deserialize;

/// Token response from an authorization-code or device-code exchange.
#[derive(Clone, Deserialize)]
pub struct TokenExchangeResponse {
    /// Access token.
    pub access_token: String,
    /// Refresh token, when issued.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Lifetime in seconds.
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// Token type (`Bearer`).
    #[serde(default)]
    pub token_type: Option<String>,
    /// Granted scope string.
    #[serde(default)]
    pub scope: Option<String>,
}

impl std::fmt::Debug for TokenExchangeResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenExchangeResponse")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_in", &self.expires_in)
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .finish()
    }
}

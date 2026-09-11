//! TokenProvider and profile catalog. Not ready.

mod error;
mod exchange;
mod helpers;
mod keychain_guard;
mod profile;
mod providers;
mod writeback;

#[cfg(feature = "device")]
pub mod device_flow;
#[cfg(feature = "pkce")]
pub mod pkce;

#[cfg(any(test, feature = "test-util"))]
mod isolated_home;

pub use error::AuthError;
pub use exchange::TokenExchangeResponse;
pub use helpers::{format_oauth_transport_error, sanitize_oauth_error_text};
pub use profile::{
    AuthScheme, Betas, CredsFormat, Dialect, ExpiresUnit, Fingerprint, ForbiddenFieldPolicy, Http,
    ListMerge, LoadOptions, Login, OauthPack, ProfileError, ResolvedProfile, SCHEMA_VERSION_MAX,
    StreamUnknownPolicy, TokenRequestFormat, TokenResponse, ToolNameCase, ToolTypePolicy, Wire,
    list_profiles, load_profile, load_profile_from_cli, parse_profile_str,
};
pub use providers::oauth::{
    ProfileTokenProvider, provider_from_oauth, provider_from_oauth_opts, provider_from_profile,
};
pub use providers::static_token::StaticToken;
pub use writeback::{persist_login_tokens, remove_store_entry};

#[cfg(feature = "test-util")]
pub use isolated_home::{IsolatedHome, PlantCredentials};
#[cfg(feature = "test-util")]
pub use keychain_guard::KeychainIsolation;

/// Crate version from Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A provider of short-lived Bearer tokens.
///
/// Implementations are cheaply cloneable (`Arc` state) and safe to share.
pub trait TokenProvider: Send + Sync + std::fmt::Debug {
    /// Return a valid access token, refreshing if necessary.
    fn get_token(&self) -> impl std::future::Future<Output = Result<String, AuthError>> + Send;

    /// Drop the cached token so the next [`TokenProvider::get_token`] refreshes.
    fn mark_stale(&self) {}
}

/// Type-erased token provider.
#[derive(Debug, Clone)]
pub enum AnyTokenProvider {
    /// Static key, never refreshed.
    Static(StaticToken),
    /// Driven by a loaded `[oauth]` table.
    Profile(ProfileTokenProvider),
}

impl TokenProvider for AnyTokenProvider {
    fn mark_stale(&self) {
        match self {
            Self::Static(p) => p.mark_stale(),
            Self::Profile(p) => p.mark_stale(),
        }
    }

    async fn get_token(&self) -> Result<String, AuthError> {
        match self {
            Self::Static(p) => p.get_token().await,
            Self::Profile(p) => p.get_token().await,
        }
    }
}

impl From<StaticToken> for AnyTokenProvider {
    fn from(t: StaticToken) -> Self {
        Self::Static(t)
    }
}

impl From<ProfileTokenProvider> for AnyTokenProvider {
    fn from(t: ProfileTokenProvider) -> Self {
        Self::Profile(t)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}

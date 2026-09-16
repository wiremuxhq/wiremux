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
pub use helpers::{
    format_oauth_transport_error, redact_secret_looking, redact_url_origin,
    sanitize_oauth_error_text,
};
pub use profile::{
    AuthScheme, Betas, CredsFormat, Dialect, ExpiresUnit, Fingerprint, ForbiddenFieldPolicy, Http,
    ListMerge, LoadOptions, Login, OauthPack, ProfileError, ResolvedProfile, SCHEMA_VERSION_MAX,
    StreamUnknownPolicy, TokenRequestFormat, TokenResponse, ToolNameCase, ToolTypePolicy, Wire,
    default_user_profile_dir, list_profiles, load_profile, load_profile_for_wire,
    load_profile_from_cli, not_found_message, parse_profile_str, shipped_profile_ids,
    user_profile_dirs,
};
pub use providers::aws::{
    AwsCredentials, AwsSignParams, AwsStsConfig, AwsStsTokenProvider, sign_aws_request,
};
pub use providers::azure::AzureTokenProvider;
pub use providers::gcp::{GcpTokenProvider, default_adc_path};
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

    /// Host laptop-wake hook. Default calls [`TokenProvider::mark_stale`].
    fn wake(&self) {
        self.mark_stale();
    }
}

/// Type-erased token provider.
#[derive(Debug, Clone)]
pub enum AnyTokenProvider {
    /// Static key, never refreshed.
    Static(StaticToken),
    /// Driven by a loaded `[oauth]` table.
    Profile(ProfileTokenProvider),
    /// Azure AD client credentials.
    Azure(AzureTokenProvider),
    /// GCP service-account JWT bearer.
    Gcp(GcpTokenProvider),
    /// AWS STS AssumeRole (not a Bearer API key).
    AwsSts(AwsStsTokenProvider),
}

impl AnyTokenProvider {
    /// Skip token-URL POSTs on later [`TokenProvider::get_token`] calls.
    /// No-op for static / cloud STS providers.
    pub fn without_http_refresh(&self) {
        if let Self::Profile(p) = self {
            p.without_http_refresh();
        }
    }

    /// ADC `quota_project_id` for Vertex, if this is a GCP authorized_user grant.
    #[must_use]
    pub fn quota_project_id(&self) -> Option<&str> {
        match self {
            Self::Gcp(p) => p.quota_project_id(),
            _ => None,
        }
    }
}

impl TokenProvider for AnyTokenProvider {
    fn mark_stale(&self) {
        match self {
            Self::Static(p) => p.mark_stale(),
            Self::Profile(p) => p.mark_stale(),
            Self::Azure(p) => p.mark_stale(),
            Self::Gcp(p) => p.mark_stale(),
            Self::AwsSts(p) => p.mark_stale(),
        }
    }

    fn wake(&self) {
        match self {
            Self::Static(p) => p.wake(),
            Self::Profile(p) => p.wake(),
            Self::Azure(p) => p.wake(),
            Self::Gcp(p) => p.wake(),
            Self::AwsSts(p) => p.wake(),
        }
    }

    async fn get_token(&self) -> Result<String, AuthError> {
        match self {
            Self::Static(p) => p.get_token().await,
            Self::Profile(p) => p.get_token().await,
            Self::Azure(p) => p.get_token().await,
            Self::Gcp(p) => p.get_token().await,
            Self::AwsSts(p) => p.get_token().await,
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

impl From<AzureTokenProvider> for AnyTokenProvider {
    fn from(t: AzureTokenProvider) -> Self {
        Self::Azure(t)
    }
}

impl From<GcpTokenProvider> for AnyTokenProvider {
    fn from(t: GcpTokenProvider) -> Self {
        Self::Gcp(t)
    }
}

impl From<AwsStsTokenProvider> for AnyTokenProvider {
    fn from(t: AwsStsTokenProvider) -> Self {
        Self::AwsSts(t)
    }
}

/// Load a catalog id (example `anthropic-oauth`) and return one access token.
///
/// This is not a cheap "is login present?" probe. It may block on OS
/// keychain and on one or two HTTP refreshes (`token_url`, then
/// `token_url_fallback`) when the stored access is expired. Use
/// [`token_for_profile_cached`] when the host only wants the stored
/// Bearer and will `mark_stale` on a later 401.
///
/// This helper does not accept a file path. Use [`load_profile_from_cli`] then
/// [`provider_from_profile`] for a `.toml` / `.json` profile.
pub async fn token_for_profile(id: &str) -> Result<String, AuthError> {
    token_for_profile_opts(id, &LoadOptions::default()).await
}

/// Same, with host LoadOptions (tests, IsolatedHome, no shipped).
pub async fn token_for_profile_opts(id: &str, opts: &LoadOptions<'_>) -> Result<String, AuthError> {
    TokenProvider::get_token(&provider_for_profile_opts(id, opts)?).await
}

/// Load a catalog id and return the stored Bearer without POSTing
/// `token_url`. Empty access still fails closed. [`token_for_profile`]
/// may still block on keychain plus one or two HTTP refreshes.
pub async fn token_for_profile_cached(id: &str) -> Result<String, AuthError> {
    token_for_profile_opts_cached(id, &LoadOptions::default()).await
}

/// Same as [`token_for_profile_cached`], with host [`LoadOptions`].
pub async fn token_for_profile_opts_cached(
    id: &str,
    opts: &LoadOptions<'_>,
) -> Result<String, AuthError> {
    let provider = provider_for_profile_opts(id, opts)?;
    provider.without_http_refresh();
    TokenProvider::get_token(&provider).await
}

/// Same load path as [`token_for_profile`], but keep the provider so the host
/// can `mark_stale` / `wake`. Catalog id only; paths use
/// [`load_profile_from_cli`] then [`provider_from_profile`].
pub fn provider_for_profile(id: &str) -> Result<AnyTokenProvider, AuthError> {
    provider_for_profile_opts(id, &LoadOptions::default())
}

pub fn provider_for_profile_opts(
    id: &str,
    opts: &LoadOptions<'_>,
) -> Result<AnyTokenProvider, AuthError> {
    if looks_like_profile_path(id) {
        return Err(AuthError::TokenProvider(
            "token_for_profile / provider_for_profile take a catalog id (example `anthropic-oauth`); \
             a file path should go through load_profile_from_cli then provider_from_profile"
                .into(),
        ));
    }
    let profile = load_profile(id, opts).map_err(|err| match err {
        ProfileError::NotFound { id, known } => AuthError::TokenProvider(format!(
            "{}; token_for_profile / provider_for_profile take a catalog id (example `anthropic-oauth`)",
            not_found_message(&id, &known)
        )),
        other => other.into(),
    })?;
    provider_from_profile(&profile)
}

fn looks_like_profile_path(id: &str) -> bool {
    if id.contains('/') || id.contains('\\') {
        return true;
    }
    let lower = id.to_ascii_lowercase();
    lower.ends_with(".toml") || lower.ends_with(".json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isolated_home::{IsolatedHome, PlantCredentials};

    const PLANTED_ACCESS: &str = "sk-ant-oat01-issue59";

    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn token_for_profile_reads_xai_access_env() {
        let home = IsolatedHome::new();
        home.set_env("XAI_API_KEY", "xai-lib-key");
        let token = token_for_profile("xai").await.expect("xai access_env");
        assert_eq!(token, "xai-lib-key");
        let _ = home;
    }

    #[tokio::test]
    async fn token_for_profile_returns_planted_claude_access() {
        let home = IsolatedHome::new();
        home.plant_credentials(PlantCredentials::Claude {
            access: PLANTED_ACCESS,
            refresh: Some("rt"),
            expires_at_ms: Some(4_000_000_000_000),
        });
        let token = token_for_profile("anthropic-oauth")
            .await
            .expect("planted claude access");
        assert_eq!(token, PLANTED_ACCESS);
        let _ = home;
    }

    #[tokio::test]
    async fn token_for_profile_cached_does_not_refresh_expired_access() {
        let home = IsolatedHome::new();
        let creds = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth.json",
            document: serde_json::json!({
                "tokens": {
                    "access": "expired-keep",
                    "refresh": "rt-keep",
                    "expiry_unix": 1
                }
            }),
        });
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"must-not-refresh","refresh_token":"rt2","expires_in":3600}"#,
        );
        let dir = home.path().join("profiles-cached");
        std::fs::create_dir_all(&dir).expect("mkdir extra profiles");
        let creds_unix = creds.to_string_lossy().replace('\\', "/");
        std::fs::write(
            dir.join("cached-mock.toml"),
            format!(
                r#"
schema_version = 1
id = "cached-mock"
[oauth]
token_url = "{url}"
client_id = "cached-client"
token_request_format = "form"
creds_format = "json-pointer"
creds_path = "{creds_unix}"
access_token_ptr = "/tokens/access"
refresh_token_ptr = "/tokens/refresh"
expires_ptr = "/tokens/expiry_unix"
expires_unit = "s"
login = "none"
[oauth.token_response]
access_token_ptr = "/access_token"
refresh_token_ptr = "/refresh_token"
expires_ptr = "/expires_in"
expires_unit = "s"
"#
            ),
        )
        .expect("write mock profile");
        let opts = LoadOptions {
            include_shipped: false,
            include_user_config: false,
            extra_profile_dirs: vec![dir],
            ..LoadOptions::default()
        };
        let token = token_for_profile_opts_cached("cached-mock", &opts)
            .await
            .expect("cached helper");
        assert_eq!(token, "expired-keep");
        drop(handle);
        let _ = home;
    }

    #[tokio::test]
    async fn token_for_profile_opts_without_catalog_is_not_found() {
        let home = IsolatedHome::new();
        let err = token_for_profile_opts(
            "anthropic-oauth",
            &LoadOptions {
                include_shipped: false,
                include_user_config: false,
                ..LoadOptions::default()
            },
        )
        .await
        .expect_err("empty catalog must fail closed");
        let display = err.to_string();
        match &err {
            AuthError::TokenProvider(msg) => {
                assert!(
                    msg.contains("not found"),
                    "TokenProvider Display must include not found, got {msg}"
                );
            }
            other => panic!("expected AuthError::TokenProvider, got {other}"),
        }
        assert!(
            display.contains("not found"),
            "Display must include not found, got {display}"
        );
        assert!(
            display.contains("catalog id") || display.contains("anthropic-oauth"),
            "Display must name catalog id or the anthropic-oauth example, got {display}"
        );
        assert!(
            !display.contains("path also works"),
            "typo catalog id must not inherit the CLI path hint, got {display}"
        );
        let _ = home;
    }

    #[tokio::test]
    async fn token_for_profile_typo_suggests_close_match() {
        let home = IsolatedHome::new();
        let err = token_for_profile("anthropic-oath")
            .await
            .expect_err("near-miss id must fail");
        let display = err.to_string();
        assert!(
            display.contains("did you mean") && display.contains("anthropic"),
            "token_for_profile must keep the catalog suggestion, got {display}"
        );
        assert!(
            !display.contains("path also works"),
            "typo catalog id must not inherit the CLI path hint, got {display}"
        );
        let _ = home;
    }

    #[tokio::test]
    async fn token_for_profile_path_shaped_id_is_catalog_id_error() {
        let home = IsolatedHome::new();
        for id in ["./mine.toml", "/tmp/mine.toml"] {
            let err = token_for_profile(id)
                .await
                .expect_err("path-shaped id must not load as a catalog entry");
            let display = err.to_string();
            assert!(
                display.contains("catalog id") || display.contains("load_profile_from_cli"),
                "path-shaped `{id}` must name catalog id or load_profile_from_cli, got {display}"
            );
            assert!(
                !display.contains("path also works"),
                "path-shaped `{id}` must not inherit the CLI path hint, got {display}"
            );
        }
        let _ = home;
    }

    #[tokio::test]
    async fn provider_for_profile_clone_returns_planted_access() {
        let home = IsolatedHome::new();
        home.plant_credentials(PlantCredentials::Claude {
            access: PLANTED_ACCESS,
            refresh: Some("rt"),
            expires_at_ms: Some(4_000_000_000_000),
        });
        let provider = provider_for_profile("anthropic-oauth").expect("provider");
        let cloned = provider.clone();
        assert_eq!(
            cloned.get_token().await.expect("clone get_token"),
            PLANTED_ACCESS
        );
        let _ = home;
    }

    #[tokio::test]
    async fn provider_for_profile_opts_mark_stale_refreshes_via_mock() {
        let home = IsolatedHome::new();
        let creds = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth.json",
            document: serde_json::json!({
                "tokens": {
                    "access": "first-59",
                    "refresh": "rt-59",
                    "expiry_unix": 4_102_444_800_i64
                }
            }),
        });
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"refreshed-59","refresh_token":"rt2","expires_in":3600}"#,
        );
        let dir = home.path().join("profiles-59");
        std::fs::create_dir_all(&dir).expect("mkdir extra profiles");
        let creds_unix = creds.to_string_lossy().replace('\\', "/");
        std::fs::write(
            dir.join("issue59-mock.toml"),
            format!(
                r#"
schema_version = 1
id = "issue59-mock"
[oauth]
token_url = "{url}"
client_id = "issue59-client"
token_request_format = "form"
creds_format = "json-pointer"
creds_path = "{creds_unix}"
access_token_ptr = "/tokens/access"
refresh_token_ptr = "/tokens/refresh"
expires_ptr = "/tokens/expiry_unix"
expires_unit = "s"
login = "none"
[oauth.token_response]
access_token_ptr = "/access_token"
refresh_token_ptr = "/refresh_token"
expires_ptr = "/expires_in"
expires_unit = "s"
"#
            ),
        )
        .expect("write mock profile");
        let opts = LoadOptions {
            include_shipped: false,
            include_user_config: false,
            extra_profile_dirs: vec![dir],
            ..LoadOptions::default()
        };
        let provider = provider_for_profile_opts("issue59-mock", &opts).expect("provider");
        assert_eq!(provider.get_token().await.expect("cached"), "first-59");
        provider.mark_stale();
        assert_eq!(
            provider.get_token().await.expect("mock refresh"),
            "refreshed-59"
        );
        let _ = handle.join();
        let _ = home;
    }

    fn spawn_http_server(status: u16, body: &str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("local_addr");
        let body = body.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let req = String::from_utf8_lossy(&buf).into_owned();
            let resp = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            req
        });
        (format!("http://{addr}/oauth/token"), handle)
    }
}

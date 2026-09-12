//! Profile-driven TokenProvider. Uses `[oauth]` only (no fingerprint / API headers).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::TokenProvider;
use crate::error::AuthError;
use crate::helpers::{
    AUTH_LOCK_TIMEOUT, InFlight, cached_token_on_lock_failure, duration_from_expires_in_secs,
    expand_tilde, format_oauth_http_error, format_oauth_transport_error, is_token_rotation_error,
    jail_creds_path, lead_or_follow, oauth_http_client, parse_rfc3339, read_oauth_body,
    redact_url_origin, remaining_from_system_time, resolve_creds_path, sanitize_oauth_error_body,
    try_acquire_refresh_lock,
};
use crate::keychain_guard::keychain_disabled;
use crate::profile::{
    CredsFormat, ExpiresUnit, OauthPack, ResolvedProfile, TokenRequestFormat, TokenResponse,
};
#[cfg(any(target_os = "macos", test, feature = "test-util"))]
use crate::writeback::apply_tokens;
use crate::writeback::{
    TokenWrite, copilot_store_pointers, json_string, json_u64, oidc_store_pointers,
    oidc_token_url_ptr, oidc_token_url_value, pointer_get, read_creds_string, write_tokens,
};

const DEFAULT_LIFETIME_SECS: u64 = 3600;

/// Cached access token plus refresh material.
struct CachedToken {
    access_token: String,
    refresh_token: Option<String>,
    acquired_at: Instant,
    lifetime: Duration,
}

impl std::fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedToken")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("acquired_at", &self.acquired_at)
            .field("lifetime", &self.lifetime)
            .finish()
    }
}

impl CachedToken {
    fn needs_refresh(&self) -> bool {
        self.acquired_at.elapsed() >= self.lifetime.mul_f64(0.8)
    }
}

/// Where credentials were loaded from.
#[derive(Clone, Debug)]
enum CredSource {
    File(PathBuf),
    #[cfg(any(target_os = "macos", test, feature = "test-util"))]
    Keychain {
        service: String,
        account: String,
    },
    Env,
}

enum WriteBack {
    File(PathBuf),
    #[cfg(any(target_os = "macos", test, feature = "test-util"))]
    Keychain {
        service: String,
        account: String,
    },
    None,
}

impl std::fmt::Debug for WriteBack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
            #[cfg(any(target_os = "macos", test, feature = "test-util"))]
            Self::Keychain { service, account } => f
                .debug_struct("Keychain")
                .field("service", service)
                .field("account", account)
                .finish(),
            Self::None => write!(f, "None"),
        }
    }
}

/// Resolved JSON pointers into the credential store.
#[derive(Clone, Debug)]
struct StoreLayout {
    access_ptr: String,
    refresh_ptr: Option<String>,
    expires_ptr: Option<String>,
    expires_unit: ExpiresUnit,
}

/// Profile-driven OAuth refresh.
#[derive(Clone)]
pub struct ProfileTokenProvider {
    inner: Arc<Inner>,
}

struct Inner {
    oauth: OauthPack,
    layout: StoreLayout,
    write_back: WriteBack,
    http: reqwest::Client,
    force_refresh: AtomicBool,
    lock_timeout_ms: AtomicU64,
    state: RwLock<CachedToken>,
    inflight: InFlight,
}

impl std::fmt::Debug for ProfileTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileTokenProvider")
            .field("token_url", &redact_url_origin(&self.inner.oauth.token_url))
            .field(
                "token_url_fallback",
                &self
                    .inner
                    .oauth
                    .token_url_fallback
                    .as_deref()
                    .map(redact_url_origin),
            )
            .field("write_back", &self.inner.write_back)
            .finish()
    }
}

/// Build a provider from a resolved profile. Uses `[oauth]` only.
pub fn provider_from_profile(
    profile: &ResolvedProfile,
) -> Result<crate::AnyTokenProvider, AuthError> {
    let oauth = profile.oauth.as_ref().ok_or_else(|| {
        AuthError::MissingField(format!("profile `{}` has no [oauth] table", profile.id))
    })?;
    provider_from_oauth(oauth)
}

/// Build a provider from an `[oauth]` pack (no fingerprint, no API headers).
pub fn provider_from_oauth(oauth: &OauthPack) -> Result<crate::AnyTokenProvider, AuthError> {
    provider_from_oauth_opts(oauth, None)
}

/// Same as [`provider_from_oauth`], with a host-supplied credentials path.
///
/// A missing explicit path fails closed to `access_env` only (a typo must
/// not silently pick keychain or the profile default file).
pub fn provider_from_oauth_opts(
    oauth: &OauthPack,
    explicit_creds_path: Option<&Path>,
) -> Result<crate::AnyTokenProvider, AuthError> {
    let loaded = load_credentials(oauth, explicit_creds_path)?;
    Ok(crate::AnyTokenProvider::Profile(
        ProfileTokenProvider::from_loaded(oauth.clone(), loaded)?,
    ))
}

struct Loaded {
    access_token: String,
    refresh_token: Option<String>,
    lifetime: Duration,
    source: CredSource,
    layout: StoreLayout,
    store_token_url: Option<String>,
}

impl ProfileTokenProvider {
    fn from_loaded(mut oauth: OauthPack, loaded: Loaded) -> Result<Self, AuthError> {
        if loaded.access_token.trim().is_empty() {
            return Err(empty_access_error(&oauth));
        }
        if oauth.creds_format == Some(CredsFormat::OidcAuthJson)
            && loaded
                .refresh_token
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
        {
            return Err(AuthError::TokenProvider(
                "oidc-auth-json requires a non-empty refresh_token".into(),
            ));
        }
        if let Some(url) = loaded.store_token_url.filter(|s| !s.is_empty())
            && token_url_allowed(&url)
        {
            oauth.token_url = url;
        }
        let write_back = match &loaded.source {
            CredSource::File(path) => absolute_write_back_path(path)
                .map(WriteBack::File)
                .unwrap_or(WriteBack::None),
            #[cfg(any(target_os = "macos", test, feature = "test-util"))]
            CredSource::Keychain { service, account } => WriteBack::Keychain {
                service: service.clone(),
                account: account.clone(),
            },
            CredSource::Env => WriteBack::None,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                oauth,
                layout: loaded.layout,
                write_back,
                http: oauth_http_client()?,
                force_refresh: AtomicBool::new(false),
                lock_timeout_ms: AtomicU64::new(AUTH_LOCK_TIMEOUT.as_millis() as u64),
                state: RwLock::new(CachedToken {
                    access_token: loaded.access_token,
                    refresh_token: loaded.refresh_token.filter(|s| !s.trim().is_empty()),
                    acquired_at: Instant::now(),
                    lifetime: loaded.lifetime,
                }),
                inflight: InFlight::new(),
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_lock_timeout(&self, timeout: Duration) {
        self.inner
            .lock_timeout_ms
            .store(timeout.as_millis() as u64, Ordering::SeqCst);
    }

    async fn reload_from_store(
        &self,
    ) -> Result<Option<(String, Option<String>, Duration)>, AuthError> {
        match &self.inner.write_back {
            WriteBack::File(path) => {
                let content = read_creds_string(path).await?;
                store_tokens_from_json(&content, &self.inner.oauth, &self.inner.layout, Some(path))
            }
            #[cfg(any(target_os = "macos", test, feature = "test-util"))]
            WriteBack::Keychain { service, account } => {
                let secret = read_keychain(service, account)?;
                store_tokens_from_json(&secret, &self.inner.oauth, &self.inner.layout, None)
            }
            WriteBack::None => Ok(None),
        }
    }

    async fn do_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<Result<ParsedTokenResponse, (u16, String, String)>, AuthError> {
        let body = refresh_request_body(&self.inner.oauth, refresh_token);
        let format = self
            .inner
            .oauth
            .token_request_format
            .unwrap_or(TokenRequestFormat::Form);

        let mut used_url = self.inner.oauth.token_url.clone();
        debug!("refreshing token via {}", redact_url_origin(&used_url));
        let resp = token_post(
            &self.inner.http,
            &self.inner.oauth,
            &used_url,
            &body,
            format,
        )
        .await?;

        let resp = if resp.status() == reqwest::StatusCode::NOT_FOUND {
            if let Some(fallback) = &self.inner.oauth.token_url_fallback {
                debug!("primary token URL returned 404, trying fallback");
                used_url = fallback.clone();
                token_post(
                    &self.inner.http,
                    &self.inner.oauth,
                    &used_url,
                    &body,
                    format,
                )
                .await?
            } else {
                resp
            }
        } else {
            resp
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let raw = read_oauth_body(resp).await.unwrap_or_default();
            return Ok(Err((status.as_u16(), raw, used_url)));
        }

        let raw = read_oauth_body(resp).await?;
        match parse_token_response(&raw, self.inner.oauth.token_response.as_ref()) {
            Ok(parsed) => Ok(Ok(parsed)),
            Err(AuthError::EmptyWriteRefused) => Err(AuthError::EmptyWriteRefused),
            Err(e) => {
                let hint = setup_hint(&self.inner.oauth);
                Err(AuthError::TokenProvider(format!("{e}; {hint}")))
            }
        }
    }

    async fn write_back(
        &self,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_in: u64,
    ) -> Result<(), AuthError> {
        if access_token.trim().is_empty() {
            return Err(AuthError::EmptyWriteRefused);
        }
        match &self.inner.write_back {
            WriteBack::File(path) => {
                let token_url_ptr =
                    oidc_token_url_ptr(&self.inner.oauth, &self.inner.layout.access_ptr);
                write_tokens(
                    path,
                    &TokenWrite {
                        access_ptr: &self.inner.layout.access_ptr,
                        refresh_ptr: self.inner.layout.refresh_ptr.as_deref(),
                        expires_ptr: self.inner.layout.expires_ptr.as_deref(),
                        expires_unit: self.inner.layout.expires_unit,
                        access_token,
                        refresh_token,
                        expires_in_secs: expires_in,
                        expires_rfc3339: self.inner.oauth.creds_format
                            == Some(CredsFormat::OidcAuthJson),
                        token_url_ptr: token_url_ptr.as_deref(),
                        token_url: oidc_token_url_value(&self.inner.oauth),
                    },
                )
                .await
            }
            #[cfg(any(target_os = "macos", test, feature = "test-util"))]
            WriteBack::Keychain { service, account } => {
                let current = read_keychain(service, account)?;
                let mut doc: Value = serde_json::from_str(&current)?;
                let token_url_ptr =
                    oidc_token_url_ptr(&self.inner.oauth, &self.inner.layout.access_ptr);
                apply_tokens(
                    &mut doc,
                    &TokenWrite {
                        access_ptr: &self.inner.layout.access_ptr,
                        refresh_ptr: self.inner.layout.refresh_ptr.as_deref(),
                        expires_ptr: self.inner.layout.expires_ptr.as_deref(),
                        expires_unit: self.inner.layout.expires_unit,
                        access_token,
                        refresh_token,
                        expires_in_secs: expires_in,
                        expires_rfc3339: self.inner.oauth.creds_format
                            == Some(CredsFormat::OidcAuthJson),
                        token_url_ptr: token_url_ptr.as_deref(),
                        token_url: oidc_token_url_value(&self.inner.oauth),
                    },
                )?;
                let updated = serde_json::to_string(&doc)?;
                write_keychain(service, account, &updated)
            }
            WriteBack::None => Ok(()),
        }
    }

    async fn refresh_as_leader(&self, force: bool) -> Result<String, AuthError> {
        let lock_timeout = Duration::from_millis(self.inner.lock_timeout_ms.load(Ordering::SeqCst));
        let _file_lock = match try_acquire_refresh_lock(
            refresh_lock_path(&self.inner.write_back).as_deref(),
            "token refresh",
            lock_timeout,
        )
        .await
        {
            Ok(guard) => guard,
            Err(e) => {
                let cached = self.inner.state.read().await.access_token.clone();
                return cached_token_on_lock_failure(Some(cached.as_str()), force, e);
            }
        };

        if self.inner.oauth.creds_format == Some(CredsFormat::CopilotHosts) {
            return self.adopt_copilot_hosts().await;
        }

        let refresh_token = {
            let mut state = self.inner.state.write().await;
            if !force && !state.needs_refresh() {
                return Ok(state.access_token.clone());
            }

            if let Ok(Some((store_token, store_rt, store_lifetime))) =
                self.reload_from_store().await
                && let Some(adopted) =
                    adopt_from_store(&state, &store_token, store_rt, store_lifetime)
            {
                debug!("token changed in store, adopting");
                let token = adopted.access_token.clone();
                *state = adopted;
                if !force && !state.needs_refresh() {
                    return Ok(token);
                }
            }

            if !state.needs_refresh() && !force {
                return Ok(state.access_token.clone());
            }

            let Some(rt) = state
                .refresh_token
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return Err(AuthError::TokenProvider(format!(
                    "access token expired and no refresh token is available; {}",
                    setup_hint(&self.inner.oauth)
                )));
            };
            rt.to_owned()
        };

        let token_resp = match self.do_refresh(&refresh_token).await? {
            Ok(resp) => resp,
            Err((status, body, url)) => {
                if is_token_rotation_error(&body) {
                    match self.reload_from_store().await {
                        Ok(Some((store_token, store_rt, store_lifetime))) => {
                            let mut state = self.inner.state.write().await;
                            if let Some(adopted) =
                                adopt_from_store(&state, &store_token, store_rt, store_lifetime)
                            {
                                debug!("refresh token rotated, adopting store token");
                                let token = adopted.access_token.clone();
                                *state = adopted;
                                return Ok(token);
                            }
                        }
                        Ok(None) => {
                            warn!("refresh token rotated, store reload returned nothing");
                        }
                        Err(e) => {
                            warn!("refresh token rotated, failed to reload store: {e}");
                        }
                    }
                }
                let summary = sanitize_oauth_error_body(&body);
                let msg = format_oauth_http_error("token refresh failed", status, &body, &url);
                let hint = setup_hint(&self.inner.oauth);
                return Err(AuthError::VendorRejected {
                    status,
                    summary: format!("{msg}; {hint} ({summary})"),
                });
            }
        };

        if token_resp.access_token.trim().is_empty() {
            return Err(AuthError::EmptyWriteRefused);
        }

        let lifetime =
            duration_from_expires_in_secs(token_resp.expires_in.unwrap_or(DEFAULT_LIFETIME_SECS));
        let new_refresh = {
            let mut state = self.inner.state.write().await;
            let new_refresh = token_resp
                .refresh_token
                .clone()
                .or_else(|| state.refresh_token.clone());
            *state = CachedToken {
                access_token: token_resp.access_token.clone(),
                refresh_token: new_refresh.clone(),
                acquired_at: Instant::now(),
                lifetime,
            };
            new_refresh
        };

        if let Err(e) = self
            .write_back(
                &token_resp.access_token,
                new_refresh.as_deref(),
                token_resp.expires_in.unwrap_or(DEFAULT_LIFETIME_SECS),
            )
            .await
        {
            warn!("failed to write refreshed token: {e}");
            return Err(e);
        }

        Ok(token_resp.access_token)
    }

    async fn adopt_copilot_hosts(&self) -> Result<String, AuthError> {
        match self.reload_from_store().await {
            Ok(Some((store_token, store_rt, store_lifetime))) => {
                let mut state = self.inner.state.write().await;
                if let Some(adopted) =
                    adopt_from_store(&state, &store_token, store_rt, store_lifetime)
                {
                    debug!("copilot hosts token changed, adopting");
                    let token = adopted.access_token.clone();
                    *state = adopted;
                    return Ok(token);
                }
                Ok(state.access_token.clone())
            }
            Ok(None) => {
                let state = self.inner.state.read().await;
                if state.access_token.trim().is_empty() {
                    return Err(empty_access_error(&self.inner.oauth));
                }
                warn!("copilot hosts reload returned nothing, keeping cached token");
                Ok(state.access_token.clone())
            }
            Err(e) => {
                let state = self.inner.state.read().await;
                if state.access_token.trim().is_empty() {
                    return Err(e);
                }
                warn!("copilot hosts reload failed ({e}), keeping cached token");
                Ok(state.access_token.clone())
            }
        }
    }
}

impl TokenProvider for ProfileTokenProvider {
    fn mark_stale(&self) {
        self.inner.force_refresh.store(true, Ordering::SeqCst);
    }

    async fn get_token(&self) -> Result<String, AuthError> {
        let force = self.inner.force_refresh.swap(false, Ordering::SeqCst);
        {
            let state = self.inner.state.read().await;
            if !force && !state.needs_refresh() && !self.inner.inflight.is_busy() {
                return Ok(state.access_token.clone());
            }
        }
        lead_or_follow(&self.inner.inflight, || self.refresh_as_leader(force)).await
    }
}

struct ParsedTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

fn refresh_request_body(oauth: &OauthPack, refresh_token: &str) -> BTreeMap<String, String> {
    let mut body = oauth.refresh_body.clone();
    if !body.contains_key("grant_type") {
        let grant = oauth
            .refresh_grant
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("refresh_token");
        body.insert("grant_type".into(), grant.to_owned());
    }
    if !body.contains_key("client_id")
        && let Some(id) = oauth
            .client_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        body.insert("client_id".into(), id.to_owned());
    }
    // Reserved key wins: a gist cannot omit or hardcode the refresh token.
    body.insert("refresh_token".into(), refresh_token.to_owned());
    body
}

fn https_token_url(url: &str) -> Option<reqwest::Url> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    (parsed.scheme() == "https").then_some(parsed)
}

async fn token_post(
    http: &reqwest::Client,
    oauth: &OauthPack,
    url: &str,
    body: &BTreeMap<String, String>,
    format: TokenRequestFormat,
) -> Result<reqwest::Response, AuthError> {
    let Some(https) = https_token_url(url) else {
        return token_post_non_https(http, oauth, url, body, format).await;
    };
    let mut req = http.post(https);
    for (k, v) in &oauth.token_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let req = match format {
        TokenRequestFormat::Json => req.json(body),
        TokenRequestFormat::Form => req.form(body),
    };
    req.send().await.map_err(|e| {
        AuthError::TokenProvider(format_oauth_transport_error(
            "token refresh request failed",
            &e,
            url,
        ))
    })
}

#[cfg(any(test, feature = "test-util"))]
async fn token_post_non_https(
    http: &reqwest::Client,
    oauth: &OauthPack,
    url: &str,
    body: &BTreeMap<String, String>,
    format: TokenRequestFormat,
) -> Result<reqwest::Response, AuthError> {
    if !crate::profile::is_loopback_http(url) {
        return Err(AuthError::TokenProvider(
            "token_url must be https (or loopback http)".into(),
        ));
    }
    let parsed = reqwest::Url::parse(url.trim())
        .map_err(|e| AuthError::TokenProvider(format!("token_url is not a valid URL: {e}")))?;
    let mut req = http.post(parsed);
    for (k, v) in &oauth.token_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let req = match format {
        TokenRequestFormat::Json => req.json(body),
        TokenRequestFormat::Form => req.form(body),
    };
    req.send().await.map_err(|e| {
        AuthError::TokenProvider(format_oauth_transport_error(
            "token refresh request failed",
            &e,
            url,
        ))
    })
}

#[cfg(not(any(test, feature = "test-util")))]
async fn token_post_non_https(
    _http: &reqwest::Client,
    _oauth: &OauthPack,
    _url: &str,
    _body: &BTreeMap<String, String>,
    _format: TokenRequestFormat,
) -> Result<reqwest::Response, AuthError> {
    Err(AuthError::TokenProvider("token_url must be https".into()))
}

fn parse_token_response(
    raw: &str,
    spec: Option<&TokenResponse>,
) -> Result<ParsedTokenResponse, AuthError> {
    let doc: Value = serde_json::from_str(raw)
        .map_err(|e| AuthError::TokenProvider(format!("token refresh: invalid JSON: {e}")))?;
    let access_ptr = spec
        .and_then(|s| s.access_token_ptr.as_deref())
        .unwrap_or("/access_token");
    let refresh_ptr = spec
        .and_then(|s| s.refresh_token_ptr.as_deref())
        .unwrap_or("/refresh_token");
    let expires_ptr = spec
        .and_then(|s| s.expires_ptr.as_deref())
        .unwrap_or("/expires_in");
    let access = match pointer_get(&doc, access_ptr) {
        Some(v) => json_string(v).unwrap_or_default(),
        None => {
            return Err(AuthError::TokenProvider(
                "token response has no access token".into(),
            ));
        }
    };
    if access.is_empty() {
        return Err(AuthError::EmptyWriteRefused);
    }
    let refresh = pointer_get(&doc, refresh_ptr).and_then(json_string);
    let expires_in = pointer_get(&doc, expires_ptr).and_then(json_u64);
    Ok(ParsedTokenResponse {
        access_token: access,
        refresh_token: refresh,
        expires_in,
    })
}

fn load_credentials(oauth: &OauthPack, explicit: Option<&Path>) -> Result<Loaded, AuthError> {
    if let Some(path) = explicit {
        let path = jail_creds_path(path)?;
        if path.is_file() {
            return load_from_file(oauth, &path);
        }
        return load_from_env(oauth)?.ok_or_else(|| missing_creds(oauth));
    }

    if let Some(raw) = oauth.creds_path.as_deref() {
        let path = resolve_creds_path(raw)?;
        if path.is_file() {
            return load_from_file(oauth, &path);
        }
        // Profile path set but missing: still try keychain, then env.
        // An *explicit* missing path (host override) never reaches here.
    }

    if let Some(creds) = load_from_keychain(oauth)? {
        return Ok(creds);
    }

    load_from_env(oauth)?.ok_or_else(|| missing_creds(oauth))
}

fn load_from_file(oauth: &OauthPack, path: &Path) -> Result<Loaded, AuthError> {
    let path = jail_creds_path(path)?;
    let path = path.as_path();
    let meta = std::fs::metadata(path).map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;
    if meta.len() > crate::helpers::MAX_CREDS_BYTES {
        return Err(AuthError::TokenProvider(format!(
            "credentials file too large ({} bytes)",
            meta.len()
        )));
    }
    let content =
        std::fs::read_to_string(path).map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;
    let doc: Value = serde_json::from_str(&content).map_err(|e| {
        AuthError::TokenProvider(format!(
            "parse credentials {}: {e}; {}",
            path.display(),
            setup_hint(oauth)
        ))
    })?;
    let layout = resolve_layout(oauth, Some(&doc))?;
    let parsed = tokens_from_doc(&doc, &layout).ok_or_else(|| empty_access_error(oauth))?;
    if parsed.0.trim().is_empty() {
        return Err(empty_access_error(oauth));
    }
    Ok(Loaded {
        access_token: parsed.0,
        refresh_token: parsed.1,
        lifetime: copilot_or_default_lifetime(oauth, parsed.2),
        source: CredSource::File(
            absolute_write_back_path(path).unwrap_or_else(|| path.to_path_buf()),
        ),
        layout,
        store_token_url: stored_oidc_token_url(oauth, &doc),
    })
}

fn load_from_env(oauth: &OauthPack) -> Result<Option<Loaded>, AuthError> {
    let Some(name) = oauth.access_env.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let Ok(token) = std::env::var(name) else {
        return Ok(None);
    };
    let token = token.trim();
    if token.is_empty() {
        return Ok(None);
    }
    let layout = resolve_layout(oauth, None)?;
    Ok(Some(Loaded {
        access_token: token.to_owned(),
        refresh_token: None,
        lifetime: Duration::from_secs(DEFAULT_LIFETIME_SECS),
        source: CredSource::Env,
        layout,
        store_token_url: None,
    }))
}

fn load_from_keychain(oauth: &OauthPack) -> Result<Option<Loaded>, AuthError> {
    let Some(service) = oauth
        .keychain_service
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    for account in &oauth.keychain_accounts {
        if account.is_empty() {
            continue;
        }
        let Ok(secret) = read_keychain(service, account) else {
            continue;
        };
        if secret.trim().is_empty() {
            continue;
        }
        let Ok(doc) = serde_json::from_str::<Value>(&secret) else {
            continue;
        };
        let Ok(layout) = resolve_layout(oauth, Some(&doc)) else {
            continue;
        };
        let Some(parsed) = tokens_from_doc(&doc, &layout) else {
            continue;
        };
        if parsed.0.is_empty() {
            continue;
        }
        #[cfg(any(target_os = "macos", test, feature = "test-util"))]
        {
            return Ok(Some(Loaded {
                access_token: parsed.0,
                refresh_token: parsed.1,
                lifetime: copilot_or_default_lifetime(oauth, parsed.2),
                source: CredSource::Keychain {
                    service: service.to_owned(),
                    account: account.clone(),
                },
                layout,
                store_token_url: stored_oidc_token_url(oauth, &doc),
            }));
        }
    }
    Ok(None)
}

fn store_tokens_from_json(
    secret: &str,
    oauth: &OauthPack,
    layout: &StoreLayout,
    path: Option<&Path>,
) -> Result<Option<(String, Option<String>, Duration)>, AuthError> {
    let doc: Value = match serde_json::from_str(secret) {
        Ok(doc) => doc,
        Err(e) => {
            return Err(match path {
                Some(path) => AuthError::json(path, e),
                None => AuthError::from(e),
            });
        }
    };
    let layout = resolve_layout(oauth, Some(&doc)).unwrap_or_else(|_| layout.clone());
    Ok(tokens_from_doc(&doc, &layout).filter(|(a, _, _)| !a.is_empty()))
}

fn tokens_from_doc(
    doc: &Value,
    layout: &StoreLayout,
) -> Option<(String, Option<String>, Duration)> {
    let access = pointer_get(doc, &layout.access_ptr).and_then(json_string)?;
    let refresh = layout
        .refresh_ptr
        .as_deref()
        .and_then(|p| pointer_get(doc, p))
        .and_then(json_string);
    let lifetime = layout
        .expires_ptr
        .as_deref()
        .and_then(|p| pointer_get(doc, p))
        .map(|v| lifetime_from_store_expires(v, layout.expires_unit))
        .unwrap_or(Duration::from_secs(DEFAULT_LIFETIME_SECS));
    Some((access, refresh, lifetime))
}

fn lifetime_from_store_expires(value: &Value, unit: ExpiresUnit) -> Duration {
    if let Some(s) = value.as_str() {
        return parse_rfc3339(s)
            .map(remaining_from_system_time)
            .unwrap_or(Duration::from_secs(DEFAULT_LIFETIME_SECS));
    }
    let Some(n) = json_u64(value) else {
        return Duration::from_secs(DEFAULT_LIFETIME_SECS);
    };
    // Store expiry is an absolute timestamp. `expiresAt: 1` is 1970, not 1ms TTL.
    match unit {
        ExpiresUnit::Ms => remaining_from_epoch_ms(n),
        ExpiresUnit::S => remaining_from_epoch_secs(n),
    }
}

fn remaining_from_epoch_ms(ms: u64) -> Duration {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if ms <= now_ms {
        Duration::ZERO
    } else {
        Duration::from_millis(ms - now_ms)
    }
}

fn remaining_from_epoch_secs(secs: u64) -> Duration {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if secs <= now {
        Duration::ZERO
    } else {
        Duration::from_secs(secs - now)
    }
}

fn resolve_layout(oauth: &OauthPack, doc: Option<&Value>) -> Result<StoreLayout, AuthError> {
    if oauth.creds_format == Some(CredsFormat::ClaudeCredentials) || looks_like_claude(oauth, doc) {
        return Ok(claude_layout(oauth, doc));
    }
    if let Some(access) = oauth.access_token_ptr.clone() {
        return Ok(StoreLayout {
            access_ptr: access,
            refresh_ptr: oauth.refresh_token_ptr.clone(),
            expires_ptr: oauth.expires_ptr.clone(),
            expires_unit: oauth.expires_unit.unwrap_or(ExpiresUnit::S),
        });
    }
    if let Some((access, refresh, expires)) = oidc_store_pointers(oauth, doc)? {
        return Ok(StoreLayout {
            access_ptr: access,
            refresh_ptr: Some(refresh),
            expires_ptr: Some(expires),
            expires_unit: oauth.expires_unit.unwrap_or(ExpiresUnit::S),
        });
    }
    if let Some((access, refresh)) = copilot_store_pointers(oauth, doc)? {
        return Ok(StoreLayout {
            access_ptr: access,
            refresh_ptr: refresh,
            expires_ptr: None,
            expires_unit: oauth.expires_unit.unwrap_or(ExpiresUnit::S),
        });
    }
    // Env-only / no store: pointers unused until write-back (which is None).
    Ok(StoreLayout {
        access_ptr: "/access_token".into(),
        refresh_ptr: oauth.refresh_token_ptr.clone(),
        expires_ptr: oauth.expires_ptr.clone(),
        expires_unit: oauth.expires_unit.unwrap_or(ExpiresUnit::S),
    })
}

fn looks_like_claude(oauth: &OauthPack, doc: Option<&Value>) -> bool {
    if matches!(
        oauth.creds_format,
        Some(CredsFormat::JsonPointer | CredsFormat::OidcAuthJson | CredsFormat::CopilotHosts)
    ) {
        return false;
    }
    oauth.creds_format == Some(CredsFormat::ClaudeCredentials)
        || oauth
            .access_token_ptr
            .as_deref()
            .is_some_and(|p| p.contains("claudeAiOauth"))
        || doc.is_some_and(|d| d.get("claudeAiOauth").is_some() || d.get("accessToken").is_some())
}

fn claude_layout(oauth: &OauthPack, doc: Option<&Value>) -> StoreLayout {
    let nested_ok = doc.is_some_and(|d| {
        matches!(d.get("claudeAiOauth"), Some(Value::Object(_)))
            && pointer_get(d, "/claudeAiOauth/accessToken")
                .and_then(json_string)
                .is_some()
    });
    let flat_ok = doc.is_some_and(|d| {
        pointer_get(d, "/accessToken")
            .and_then(json_string)
            .is_some()
    });
    if nested_ok || (!flat_ok && doc.is_none()) {
        StoreLayout {
            access_ptr: oauth
                .access_token_ptr
                .clone()
                .unwrap_or_else(|| "/claudeAiOauth/accessToken".into()),
            refresh_ptr: Some(
                oauth
                    .refresh_token_ptr
                    .clone()
                    .unwrap_or_else(|| "/claudeAiOauth/refreshToken".into()),
            ),
            expires_ptr: Some(
                oauth
                    .expires_ptr
                    .clone()
                    .unwrap_or_else(|| "/claudeAiOauth/expiresAt".into()),
            ),
            expires_unit: oauth.expires_unit.unwrap_or(ExpiresUnit::Ms),
        }
    } else {
        StoreLayout {
            access_ptr: "/accessToken".into(),
            refresh_ptr: Some("/refreshToken".into()),
            expires_ptr: Some("/expiresAt".into()),
            expires_unit: oauth.expires_unit.unwrap_or(ExpiresUnit::Ms),
        }
    }
}

fn copilot_or_default_lifetime(oauth: &OauthPack, lifetime: Duration) -> Duration {
    if oauth.creds_format == Some(CredsFormat::CopilotHosts)
        && lifetime == Duration::from_secs(DEFAULT_LIFETIME_SECS)
    {
        Duration::from_secs(8 * 3600)
    } else {
        lifetime
    }
}

fn stored_oidc_token_url(oauth: &OauthPack, doc: &Value) -> Option<String> {
    if oauth.creds_format != Some(CredsFormat::OidcAuthJson) {
        return None;
    }
    let (access, _, _) = oidc_store_pointers(oauth, Some(doc)).ok().flatten()?;
    let entry = access.strip_suffix("/key")?;
    let ptr = format!("{entry}/token_url");
    pointer_get(doc, &ptr).and_then(json_string)
}

fn token_url_allowed(url: &str) -> bool {
    if https_token_url(url).is_some() {
        return true;
    }
    #[cfg(any(test, feature = "test-util"))]
    {
        crate::profile::is_loopback_http(url)
    }
    #[cfg(not(any(test, feature = "test-util")))]
    {
        false
    }
}

fn adopt_from_store(
    cached: &CachedToken,
    store_access: &str,
    store_refresh: Option<String>,
    store_lifetime: Duration,
) -> Option<CachedToken> {
    if store_access.is_empty() || store_access == cached.access_token {
        return None;
    }
    Some(CachedToken {
        access_token: store_access.to_owned(),
        refresh_token: store_refresh,
        acquired_at: Instant::now(),
        lifetime: store_lifetime,
    })
}

fn refresh_lock_path(write_back: &WriteBack) -> Option<PathBuf> {
    match write_back {
        WriteBack::File(path) => Some(path.clone()),
        #[cfg(any(target_os = "macos", test, feature = "test-util"))]
        WriteBack::Keychain { service, account } => {
            Some(keychain_refresh_lock_path(service, account))
        }
        WriteBack::None => None,
    }
}

#[cfg(any(target_os = "macos", test, feature = "test-util"))]
fn keychain_refresh_lock_path(service: &str, account: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "wiremux-auth-{}-{}",
        sanitize_lock_component(service),
        sanitize_lock_component(account)
    ))
}

#[cfg(any(target_os = "macos", test, feature = "test-util"))]
fn sanitize_lock_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn absolute_write_back_path(path: &Path) -> Option<PathBuf> {
    let path = jail_creds_path(path).ok()?;
    if path.as_os_str().is_empty() {
        return None;
    }
    let has_parent = path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    if path.exists()
        && let Ok(canon) = std::fs::canonicalize(&path)
    {
        return Some(canon);
    }
    let joined = match std::env::current_dir() {
        Ok(cwd) => cwd.join(&path),
        Err(_) if path.is_absolute() && !has_parent => return Some(path),
        Err(_) => return None,
    };
    if let Ok(canon) = std::fs::canonicalize(&joined) {
        return Some(canon);
    }
    if joined.is_absolute()
        && !joined.as_os_str().is_empty()
        && !joined
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Some(joined);
    }
    None
}

fn setup_hint(oauth: &OauthPack) -> String {
    oauth
        .setup_token_hint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("re-authenticate using the profile login flow")
        .to_owned()
}

fn empty_access_error(oauth: &OauthPack) -> AuthError {
    AuthError::TokenProvider(format!(
        "credentials have an empty access token; {}",
        setup_hint(oauth)
    ))
}

fn missing_creds(oauth: &OauthPack) -> AuthError {
    AuthError::MissingField(format!(
        "no credentials ({}); {}",
        attempted_cred_stores(oauth),
        setup_hint(oauth)
    ))
}

fn attempted_cred_stores(oauth: &OauthPack) -> String {
    let mut tried = Vec::new();
    if let Some(raw) = oauth
        .creds_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        tried.push(format!("creds_path={}", expand_tilde(raw).display()));
    }
    if let Some(service) = oauth
        .keychain_service
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let accounts: Vec<&str> = oauth
            .keychain_accounts
            .iter()
            .map(|s| s.as_str().trim())
            .filter(|s| !s.is_empty())
            .collect();
        if accounts.is_empty() {
            tried.push(format!("keychain service={service}"));
        } else {
            tried.push(format!(
                "keychain service={service} accounts={}",
                accounts.join(",")
            ));
        }
    }
    if let Some(env) = oauth
        .access_env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        tried.push(format!("access_env={env}"));
    }
    if tried.is_empty() {
        "no creds_path, keychain, or access_env configured".into()
    } else {
        format!("tried {}", tried.join(", "))
    }
}

fn read_keychain(service: &str, account: &str) -> Result<String, AuthError> {
    #[cfg(any(test, feature = "test-util"))]
    if let Some(secret) = crate::keychain_guard::test_keychain_get(service, account) {
        return Ok(secret);
    }
    if keychain_disabled() {
        return Err(AuthError::TokenProvider("keychain disabled".into()));
    }
    #[cfg(target_os = "macos")]
    {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| AuthError::TokenProvider(format!("keychain: {e}")))?;
        entry
            .get_password()
            .map_err(|e| AuthError::TokenProvider(format!("keychain read: {e}")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (service, account);
        Err(AuthError::TokenProvider("keychain not available".into()))
    }
}

#[cfg(any(target_os = "macos", test, feature = "test-util"))]
fn write_keychain(service: &str, account: &str, secret: &str) -> Result<(), AuthError> {
    #[cfg(any(test, feature = "test-util"))]
    if crate::keychain_guard::test_keychain_active() {
        crate::keychain_guard::test_keychain_set(service, account, secret);
        return Ok(());
    }
    if keychain_disabled() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| AuthError::TokenProvider(format!("keychain: {e}")))?;
        entry
            .set_password(secret)
            .map_err(|e| AuthError::TokenProvider(format!("keychain write: {e}")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (service, account, secret);
        Err(AuthError::TokenProvider("keychain not available".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isolated_home::{IsolatedHome, PlantCredentials};
    use crate::parse_profile_str;
    use crate::{AnyTokenProvider, TokenProvider};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn needs_refresh_at_eighty_percent() {
        let mut tok = CachedToken {
            access_token: "tok".into(),
            refresh_token: Some("rt".into()),
            acquired_at: Instant::now() - Duration::from_secs(81),
            lifetime: Duration::from_secs(100),
        };
        assert!(tok.needs_refresh(), "81s of 100s must refresh");
        tok.acquired_at = Instant::now() - Duration::from_secs(79);
        assert!(!tok.needs_refresh(), "79s of 100s must keep cache");
    }

    #[tokio::test]
    async fn wake_forces_refresh_on_next_get() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth.json",
            document: serde_json::json!({
                "tokens": {
                    "access": "first",
                    "refresh": "rt",
                    "expiry_unix": 4_102_444_800_i64
                }
            }),
        });
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"second","refresh_token":"rt2","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&pointer_toml(&url, &path));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.expect("first"), "first");
        p.wake();
        assert_eq!(p.get_token().await.expect("forced refresh"), "second");
        let _ = handle.join();
    }

    #[tokio::test]
    async fn mark_stale_forces_refresh_on_next_get() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth.json",
            document: serde_json::json!({
                "tokens": {
                    "access": "first",
                    "refresh": "rt",
                    "expiry_unix": 4_102_444_800_i64
                }
            }),
        });
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"second","refresh_token":"rt2","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&pointer_toml(&url, &path));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.expect("first"), "first");
        p.mark_stale();
        assert_eq!(p.get_token().await.expect("forced refresh"), "second");
        let _ = handle.join();
    }

    fn keychain_toml(token_url: &str) -> String {
        format!(
            r#"
schema_version = 1
id = "kc-test"
[oauth]
token_url = "{token_url}"
client_id = "test-client"
token_request_format = "json"
creds_format = "claude-credentials"
keychain_service = "wiremux-test"
keychain_accounts = ["acct"]
login = "none"
[oauth.refresh_body]
grant_type = "refresh_token"
"#
        )
    }

    #[tokio::test]
    async fn keychain_write_back_updates_test_store() {
        let home = IsolatedHome::new();
        home.plant_keychain(
            "wiremux-test",
            "acct",
            &serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "kc-old",
                    "refreshToken": "rt-old",
                    "expiresAt": 1
                }
            }),
        );
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"kc-new","refresh_token":"rt-new","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&keychain_toml(&url));
        let p = provider(&oauth);
        assert_eq!(
            p.get_token().await.expect("refresh from keychain"),
            "kc-new"
        );
        let _ = handle.join();
        let stored = crate::keychain_guard::test_keychain_get("wiremux-test", "acct")
            .expect("test keychain still planted");
        let doc: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            doc["claudeAiOauth"]["accessToken"].as_str(),
            Some("kc-new"),
            "write-back must update the keychain document, got {doc}"
        );
        assert_eq!(
            doc["claudeAiOauth"]["refreshToken"].as_str(),
            Some("rt-new")
        );
    }

    fn spawn_counting_http_server(
        status: u16,
        body: &str,
        window: Duration,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("local_addr");
        let body = body.to_owned();
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let count_thread = count.clone();
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            while start.elapsed() < window {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        count_thread.fetch_add(1, Ordering::SeqCst);
                        stream.set_nonblocking(false).ok();
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);
                        let resp = format!(
                            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        (format!("http://{addr}/oauth/token"), count, handle)
    }

    #[tokio::test]
    async fn parallel_get_token_issues_one_refresh_http() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth.json",
            document: serde_json::json!({
                "tokens": {
                    "access": "stale",
                    "refresh": "rt",
                    "expiry_unix": 1
                }
            }),
        });
        let (url, count, handle) = spawn_counting_http_server(
            200,
            r#"{"access_token":"shared","refresh_token":"rt2","expires_in":3600}"#,
            Duration::from_millis(800),
        );
        let oauth = pack_from_toml(&pointer_toml(&url, &path));
        let p = provider(&oauth);
        let a = p.clone();
        let b = p.clone();
        let (left, right) = tokio::join!(a.get_token(), b.get_token());
        assert_eq!(left.expect("left"), "shared");
        assert_eq!(right.expect("right"), "shared");
        let _ = handle.join();
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "single-flight must POST once"
        );
    }

    fn pack_from_toml(toml: &str) -> OauthPack {
        parse_profile_str(toml)
            .expect("parse test profile")
            .oauth
            .expect("oauth table")
    }

    fn claude_toml(token_url: &str, fallback: Option<&str>) -> String {
        let fb = match fallback {
            Some(u) => format!("token_url_fallback = \"{u}\"\n"),
            None => String::new(),
        };
        format!(
            r#"
schema_version = 1
id = "t"
[oauth]
token_url = "{token_url}"
{fb}client_id = "test-client"
token_request_format = "json"
creds_path = "~/.claude/.credentials.json"
creds_format = "claude-credentials"
access_env = "CLAUDE_CODE_OAUTH_TOKEN"
login = "setup-token"
setup_token_hint = "run `claude setup-token`"
[oauth.refresh_body]
grant_type = "refresh_token"
"#
        )
    }

    fn oidc_toml(token_url: &str, creds: &std::path::Path, client_id: &str) -> String {
        let creds = creds.to_string_lossy().replace('\\', "/");
        format!(
            r#"
schema_version = 1
id = "openai-codex-oauth"
[oauth]
token_url = "{token_url}"
authorize_url = "https://auth.openai.com/oauth/authorize"
client_id = "{client_id}"
token_request_format = "form"
creds_format = "oidc-auth-json"
creds_path = "{creds}"
login = "none"
"#
        )
    }

    fn pointer_toml(token_url: &str, creds: &std::path::Path) -> String {
        let creds = creds.to_string_lossy().replace('\\', "/");
        format!(
            r#"
schema_version = 1
id = "other-vendor-oauth"
[headers]
X-Api = "must-not-be-on-token-post"
[oauth]
token_url = "{token_url}"
client_id = "example-public-client"
token_request_format = "form"
creds_format = "json-pointer"
creds_path = "{creds}"
access_token_ptr = "/tokens/access"
refresh_token_ptr = "/tokens/refresh"
expires_ptr = "/tokens/expiry_unix"
expires_unit = "s"
login = "none"
[oauth.token_headers]
X-Client = "official-app"
[oauth.token_response]
access_token_ptr = "/access_token"
refresh_token_ptr = "/refresh_token"
expires_ptr = "/expires_in"
expires_unit = "s"
"#
        )
    }

    /// Bind-and-close so a mistaken refresh fails with ECONNREFUSED, not a hang.
    fn closed_http_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        format!("http://{addr}/oauth/token")
    }

    fn spawn_http_server(status: u16, body: &str) -> (String, std::thread::JoinHandle<String>) {
        spawn_http_server_on_accept(status, body, || {})
    }

    fn spawn_http_server_on_accept(
        status: u16,
        body: &str,
        on_accept: impl FnOnce() + Send + 'static,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("local_addr");
        let body = body.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            on_accept();
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&buf[..pos]);
                            let content_len = header
                                .lines()
                                .find_map(|line| {
                                    line.split_once(':').and_then(|(k, v)| {
                                        k.eq_ignore_ascii_case("content-length")
                                            .then_some(v.trim().parse::<usize>().unwrap_or(0))
                                    })
                                })
                                .unwrap_or(0);
                            let header_end = pos + 4;
                            while buf.len() < header_end + content_len {
                                match stream.read(&mut tmp) {
                                    Ok(0) => break,
                                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                                    Err(_) => break,
                                }
                            }
                            break;
                        }
                        if buf.len() > 32_768 {
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
            let _ = stream.flush();
            req
        });
        (format!("http://{addr}/oauth/token"), handle)
    }

    fn provider(oauth: &OauthPack) -> ProfileTokenProvider {
        match provider_from_oauth(oauth).expect("provider") {
            AnyTokenProvider::Profile(p) => p,
            _ => panic!("expected Profile"),
        }
    }

    #[tokio::test]
    async fn fresh_token_returns_cached_without_http() {
        let home = IsolatedHome::new();
        home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-fresh",
            refresh: Some("rt"),
            expires_at_ms: Some(4_000_000_000_000),
        });
        let oauth = pack_from_toml(&claude_toml(&closed_http_url(), None));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.unwrap(), "sk-ant-oat01-fresh");
    }

    #[tokio::test]
    async fn primary_http_404_falls_back_and_writes() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-old",
            refresh: Some("rt-old"),
            expires_at_ms: Some(1),
        });
        let (primary, primary_h) = spawn_http_server(404, r#"{"error":"not_found"}"#);
        let (fallback, fallback_h) = spawn_http_server(
            200,
            r#"{"access_token":"sk-ant-oat01-refreshed","refresh_token":"rt-new","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&claude_toml(&primary, Some(&fallback)));
        let p = provider(&oauth);
        let token = p.get_token().await.expect("fallback refresh");
        assert_eq!(token, "sk-ant-oat01-refreshed");
        let _ = primary_h.join();
        let req = fallback_h.join().expect("fallback");
        assert!(
            req.contains("\"grant_type\":\"refresh_token\""),
            "JSON grant, got: {req}"
        );
        assert!(
            req.contains("\"client_id\":\"test-client\""),
            "client_id from pack, got: {req}"
        );
        assert!(
            req.contains("\"refresh_token\":\"rt-old\""),
            "engine injects store refresh, got: {req}"
        );
        let written = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(
            doc["claudeAiOauth"]["accessToken"].as_str(),
            Some("sk-ant-oat01-refreshed")
        );
        assert_eq!(
            doc["claudeAiOauth"]["refreshToken"].as_str(),
            Some("rt-new")
        );
        assert_eq!(doc["otherField"].as_str(), Some("keep-me"));
    }

    #[tokio::test]
    async fn empty_access_from_vendor_refuses_write() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-old",
            refresh: Some("rt-old"),
            expires_at_ms: Some(1),
        });
        let before = std::fs::read_to_string(&path).unwrap();
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"","refresh_token":"rt-new","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&claude_toml(&url, None));
        let p = provider(&oauth);
        let err = p.get_token().await.expect_err("empty write refuse");
        let _ = handle.join();
        assert!(matches!(err, AuthError::EmptyWriteRefused), "got {err:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_back_sets_0600() {
        use std::os::unix::fs::PermissionsExt;
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-old",
            refresh: Some("rt-old"),
            expires_at_ms: Some(1),
        });
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"sk-ant-oat01-new","refresh_token":"rt-new","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&claude_toml(&url, None));
        let p = provider(&oauth);
        let _ = p.get_token().await.expect("refresh");
        let _ = handle.join();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persist_io_failure_fails_closed_and_keeps_store() {
        use std::os::unix::fs::PermissionsExt;
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-old",
            refresh: Some("rt-old"),
            expires_at_ms: Some(1),
        });
        let before = std::fs::read(&path).unwrap();
        let parent = path.parent().expect("creds parent").to_path_buf();
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"sk-ant-oat01-new","refresh_token":"rt-new","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&claude_toml(&url, None));
        let p = provider(&oauth);
        // Pre-create the lock sibling so 0555 still lets us lock, but not
        // create `{creds}.tmp`. Otherwise lock-file create fails first and
        // we never reach write-back (and the mock accept hangs).
        let lock_path = crate::helpers::lock_sibling(&path);
        std::fs::write(&lock_path, b"").unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = p.get_token().await;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = handle.join();
        result.expect_err("persist I/O must fail closed");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "live credentials must stay intact when the sibling write cannot be created"
        );
    }

    #[tokio::test]
    async fn adopt_on_rotation_before_http() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-old",
            refresh: Some("rt-old"),
            expires_at_ms: Some(1),
        });
        let oauth = pack_from_toml(&claude_toml(&closed_http_url(), None));
        let p = provider(&oauth);
        home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-from-peer",
            refresh: Some("rt-peer"),
            expires_at_ms: Some(4_000_000_000_000),
        });
        let _ = path;
        assert_eq!(p.get_token().await.unwrap(), "sk-ant-oat01-from-peer");
    }

    #[tokio::test]
    async fn invalid_grant_adopts_peer_then_errors_if_still_stale() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-old",
            refresh: Some("rt-old"),
            expires_at_ms: Some(1),
        });
        let path_for_peer = path.clone();
        let (url, handle) = spawn_http_server_on_accept(
            400,
            r#"{"error":"invalid_grant","error_description":"revoked","refresh_token":"rt-LEAK"}"#,
            move || {
                std::fs::write(
                    &path_for_peer,
                    serde_json::json!({
                        "claudeAiOauth": {
                            "accessToken": "sk-ant-oat01-from-peer",
                            "refreshToken": "rt-peer",
                            "expiresAt": 4_000_000_000_000u64
                        },
                        "otherField": "keep-me"
                    })
                    .to_string(),
                )
                .expect("peer write");
            },
        );
        let oauth = pack_from_toml(&claude_toml(&url, None));
        let p = provider(&oauth);
        let token = p.get_token().await.expect("adopt after invalid_grant");
        let _ = handle.join();
        assert_eq!(token, "sk-ant-oat01-from-peer");
    }

    #[tokio::test]
    async fn invalid_grant_sanitizes_and_includes_hint() {
        let home = IsolatedHome::new();
        home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-old",
            refresh: Some("rt-old"),
            expires_at_ms: Some(1),
        });
        let (url, handle) = spawn_http_server(
            401,
            r#"{"error":"invalid_grant","refresh_token":"rt-LEAK","access_token":"sk-ant-oat01-LEAK"}"#,
        );
        let oauth = pack_from_toml(&claude_toml(&url, None));
        let p = provider(&oauth);
        let err = p.get_token().await.expect_err("401");
        let _ = handle.join();
        let msg = err.to_string();
        assert!(
            !msg.contains("sk-ant-oat01-LEAK") && !msg.contains("rt-LEAK"),
            "{msg}"
        );
        assert!(msg.to_ascii_lowercase().contains("setup-token"), "{msg}");
    }

    #[tokio::test]
    async fn json_pointer_store_refreshes_without_claude_alias() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/other-vendor.json",
            document: serde_json::json!({
                "tokens": {
                    "access": "old-access",
                    "refresh": "old-refresh",
                    "expiry_unix": 1
                },
                "keep": true
            }),
        });
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&pointer_toml(&url, &path));
        let p = provider(&oauth);
        let token = p.get_token().await.expect("json-pointer refresh");
        assert_eq!(token, "new-access");
        let req = handle.join().expect("server");
        assert!(
            req.contains("X-Client: official-app") || req.contains("x-client: official-app"),
            "token_headers must be sent, got: {req}"
        );
        assert!(
            !req.to_ascii_lowercase().contains("x-api"),
            "API headers must not be on the token POST, got: {req}"
        );
        assert!(
            req.contains("grant_type=refresh_token") || req.contains("grant_type\":"),
            "form grant, got: {req}"
        );
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["tokens"]["access"].as_str(), Some("new-access"));
        assert_eq!(doc["tokens"]["refresh"].as_str(), Some("new-refresh"));
        assert_eq!(doc["keep"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn oidc_auth_json_reads_matching_issuer_client_entry() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth-openai.json",
            document: serde_json::json!({
                "https://aaa.example::other": {
                    "key": "wrong-access",
                    "refresh_token": "wrong-rt",
                    "expires_at": "2099-01-01T00:00:00Z"
                },
                "https://auth.openai.com::wiremux-cli": {
                    "key": "oidc-access",
                    "refresh_token": "oidc-rt",
                    "expires_at": "2099-01-01T00:00:00Z"
                }
            }),
        });
        let oauth = pack_from_toml(&oidc_toml(&closed_http_url(), &path, "wiremux-cli"));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.expect("oidc entry"), "oidc-access");
    }

    #[tokio::test]
    async fn oidc_auth_json_matches_named_suffix() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth-work.json",
            document: serde_json::json!({
                "https://auth.openai.com::wiremux-cli@work": {
                    "key": "WORK_TOKEN",
                    "refresh_token": "work-rt",
                    "expires_at": "2099-01-01T00:00:00Z"
                }
            }),
        });
        let oauth = pack_from_toml(&oidc_toml(&closed_http_url(), &path, "wiremux-cli"));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.expect("named suffix"), "WORK_TOKEN");
    }

    #[tokio::test]
    async fn oidc_auth_json_refresh_preserves_sibling_entries() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth-openai.json",
            document: serde_json::json!({
                "https://aaa.example::other": {
                    "key": "keep-me",
                    "refresh_token": "keep-rt",
                    "expires_at": "2099-01-01T00:00:00Z"
                },
                "https://auth.openai.com::wiremux-cli": {
                    "key": "old-access",
                    "refresh_token": "old-rt",
                    "expires_at": "2020-01-01T00:00:00Z"
                }
            }),
        });
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"new-oidc","refresh_token":"new-rt","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&oidc_toml(&url, &path, "wiremux-cli"));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.expect("oidc refresh"), "new-oidc");
        let _ = handle.join();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc["https://aaa.example::other"]["key"].as_str(),
            Some("keep-me")
        );
        assert_eq!(
            doc["https://auth.openai.com::wiremux-cli"]["key"].as_str(),
            Some("new-oidc")
        );
        assert_eq!(
            doc["https://auth.openai.com::wiremux-cli"]["refresh_token"].as_str(),
            Some("new-rt")
        );
        let expires = doc["https://auth.openai.com::wiremux-cli"]["expires_at"]
            .as_str()
            .expect("rfc3339 expires_at");
        assert!(
            expires.contains('T'),
            "expires_at must stay RFC 3339: {expires}"
        );
    }

    #[test]
    fn oidc_auth_json_empty_client_id_fails_closed() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth-openai.json",
            document: serde_json::json!({
                "https://auth.openai.com::someone": {
                    "key": "must-not-guess",
                    "refresh_token": "rt",
                    "expires_at": "2099-01-01T00:00:00Z"
                }
            }),
        });
        let oauth = pack_from_toml(&oidc_toml(&closed_http_url(), &path, ""));
        let err = provider_from_oauth(&oauth).expect_err("empty client_id");
        let msg = err.to_string();
        assert!(
            msg.contains("client_id"),
            "must fail closed on empty client_id, got {msg}"
        );
        assert!(
            !msg.contains("must-not-guess"),
            "must not leak a guessed token: {msg}"
        );
    }

    fn copilot_toml(creds: &std::path::Path) -> String {
        let creds = creds.to_string_lossy().replace('\\', "/");
        format!(
            r#"
schema_version = 1
id = "gist-copilot"
[oauth]
token_url = "https://github.com/login/oauth/access_token"
creds_format = "copilot-hosts"
creds_path = "{creds}"
login = "none"
"#
        )
    }

    #[tokio::test]
    async fn copilot_hosts_reads_oauth_token() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/github-copilot/hosts.json",
            document: serde_json::json!({
                "github.com": { "oauth_token": "ghu_from_hosts" },
                "other": { "oauth_token": "" }
            }),
        });
        let oauth = pack_from_toml(&copilot_toml(&path));
        let p = provider(&oauth);
        assert_eq!(
            p.get_token().await.expect("copilot token"),
            "ghu_from_hosts"
        );
    }

    #[tokio::test]
    async fn copilot_expired_path_adopts_from_file() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/github-copilot/hosts.json",
            document: serde_json::json!({
                "github.com": { "oauth_token": "ghu_old" }
            }),
        });
        let oauth = pack_from_toml(&copilot_toml(&path));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.expect("first"), "ghu_old");
        std::fs::write(
            &path,
            serde_json::to_string(&serde_json::json!({
                "github.com": { "oauth_token": "ghu_rotated" }
            }))
            .unwrap(),
        )
        .unwrap();
        p.mark_stale();
        assert_eq!(
            p.get_token().await.expect("adopt rotated hosts.json"),
            "ghu_rotated"
        );
    }

    #[tokio::test]
    async fn copilot_stale_without_rotation_keeps_cached() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/github-copilot/hosts.json",
            document: serde_json::json!({
                "github.com": { "oauth_token": "ghu_same" }
            }),
        });
        let oauth = pack_from_toml(&copilot_toml(&path));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.expect("first"), "ghu_same");
        p.mark_stale();
        assert_eq!(
            p.get_token().await.expect("no refresh token must not fail"),
            "ghu_same"
        );
    }

    #[test]
    fn oidc_new_rejects_empty_refresh_token() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth-openai.json",
            document: serde_json::json!({
                "https://auth.openai.com::wiremux-cli": {
                    "key": "access-only",
                    "expires_at": "2099-01-01T00:00:00Z"
                }
            }),
        });
        let oauth = pack_from_toml(&oidc_toml(&closed_http_url(), &path, "wiremux-cli"));
        let err = provider_from_oauth(&oauth).expect_err("empty refresh");
        let msg = err.to_string();
        assert!(
            msg.contains("refresh"),
            "oidc-auth-json must refuse empty refresh at construction, got {msg}"
        );
    }

    #[tokio::test]
    async fn oidc_refresh_persists_token_url() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth-openai.json",
            document: serde_json::json!({
                "https://auth.openai.com::wiremux-cli": {
                    "key": "old-access",
                    "refresh_token": "old-rt",
                    "expires_at": "2020-01-01T00:00:00Z"
                }
            }),
        });
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"new-oidc","refresh_token":"new-rt","expires_in":3600}"#,
        );
        let oauth = pack_from_toml(&oidc_toml(&url, &path, "wiremux-cli"));
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.expect("refresh"), "new-oidc");
        let _ = handle.join();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc["https://auth.openai.com::wiremux-cli"]["token_url"].as_str(),
            Some(url.as_str()),
            "refresh must persist token_url, got {doc}"
        );
    }

    #[tokio::test]
    async fn persisted_token_url_wins_over_profile() {
        let home = IsolatedHome::new();
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"from-store-url","refresh_token":"new-rt","expires_in":3600}"#,
        );
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/auth-openai.json",
            document: serde_json::json!({
                "https://auth.openai.com::wiremux-cli": {
                    "key": "old-access",
                    "refresh_token": "old-rt",
                    "expires_at": "2020-01-01T00:00:00Z",
                    "token_url": url
                }
            }),
        });
        let oauth = pack_from_toml(&oidc_toml(&closed_http_url(), &path, "wiremux-cli"));
        let p = provider(&oauth);
        assert_eq!(
            p.get_token().await.expect("refresh via stored token_url"),
            "from-store-url"
        );
        let _ = handle.join();
    }

    #[tokio::test]
    async fn copilot_hosts_empty_token_fails_closed() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/github-copilot/hosts.json",
            document: serde_json::json!({
                "github.com": { "user": "x" }
            }),
        });
        let oauth = pack_from_toml(&copilot_toml(&path));
        let err = provider_from_oauth(&oauth).expect_err("missing oauth_token");
        assert!(
            err.to_string().contains("empty access token")
                || err.to_string().contains("credentials"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn env_only_never_writes() {
        let home = IsolatedHome::with_extra_envs(&["WIREMUX_TEST_ACCESS"]);
        home.set_env("WIREMUX_TEST_ACCESS", "env-access-token");
        let toml = format!(
            r#"
schema_version = 1
id = "env-only"
[oauth]
token_url = "{}"
access_env = "WIREMUX_TEST_ACCESS"
"#,
            closed_http_url()
        );
        let oauth = pack_from_toml(&toml);
        let p = provider(&oauth);
        assert_eq!(p.get_token().await.unwrap(), "env-access-token");
        assert!(
            matches!(p.inner.write_back, WriteBack::None),
            "env-only must not write"
        );
    }

    #[test]
    fn token_endpoint_https_or_loopback_only() {
        assert!(https_token_url("https://auth.example.invalid/token").is_some());
        assert!(https_token_url("HTTPS://auth.example.invalid/token").is_some());
        assert!(https_token_url("http://127.0.0.1:9/token").is_none());
        assert!(crate::profile::is_loopback_http("http://127.0.0.1:9/token"));
        assert!(crate::profile::is_loopback_http("http://localhost/token"));
        assert!(!crate::profile::is_loopback_http("http://192.0.2.1/token"));
        assert!(!crate::profile::is_loopback_http(
            "http://example.invalid/token"
        ));
    }

    #[test]
    fn missing_explicit_path_does_not_use_home_file() {
        let home = IsolatedHome::new();
        home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-planted-home",
            refresh: Some("rt-home"),
            expires_at_ms: None,
        });
        let oauth = pack_from_toml(&claude_toml(&closed_http_url(), None));
        let missing = home.path().join("no-such-creds.json");
        let err = provider_from_oauth_opts(&oauth, Some(&missing)).expect_err("typo path");
        let msg = err.to_string();
        assert!(
            msg.contains("no credentials") || msg.contains("missing"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn lock_timeout_returns_cached_when_not_forced() {
        let home = IsolatedHome::new();
        let path = home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-cached",
            refresh: Some("rt"),
            expires_at_ms: Some(1),
        });
        let oauth = pack_from_toml(&claude_toml(&closed_http_url(), None));
        let p = provider(&oauth);
        p.set_lock_timeout(Duration::from_millis(80));
        let lock_path = crate::helpers::lock_sibling(&path);
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        fs4::FileExt::try_lock(&lock_file).expect("try lock");
        let token = p.get_token().await.expect("cached on lock timeout");
        assert_eq!(token, "sk-ant-oat01-cached");
        p.mark_stale();
        let err = p.get_token().await.expect_err("forced lock fail");
        assert!(matches!(err, AuthError::LockTimeout), "got {err:?}");
        drop(lock_file);
    }

    #[test]
    fn provider_from_profile_requires_oauth() {
        let profile = parse_profile_str("schema_version = 1\nid = \"no-oauth\"\n").unwrap();
        let err = provider_from_profile(&profile).unwrap_err();
        assert!(err.to_string().contains("[oauth]"));
    }

    #[test]
    fn debug_redacts_tokens() {
        let home = IsolatedHome::new();
        home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-secret",
            refresh: Some("rt-secret"),
            expires_at_ms: None,
        });
        let oauth = pack_from_toml(&claude_toml(&closed_http_url(), None));
        let p = provider(&oauth);
        let debug = format!("{p:?}");
        assert!(!debug.contains("sk-ant-oat01-secret"));
        assert!(!debug.contains("rt-secret"));
    }

    #[test]
    fn debug_redacts_token_url_userinfo_and_query() {
        let home = IsolatedHome::new();
        home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-secret",
            refresh: Some("rt-secret"),
            expires_at_ms: None,
        });
        let leaky =
            "https://user:s3cret@auth.example.invalid/oauth/token?client_secret=supersecret";
        let oauth = pack_from_toml(&claude_toml(leaky, Some(leaky)));
        let p = provider(&oauth);
        let debug = format!("{p:?}");
        assert!(!debug.contains("s3cret"), "Debug leaked userinfo: {debug}");
        assert!(
            !debug.contains("client_secret="),
            "Debug leaked query: {debug}"
        );
        assert!(
            !debug.contains("supersecret"),
            "Debug leaked secret: {debug}"
        );
        assert!(!debug.contains("/oauth"), "Debug leaked path: {debug}");
        assert!(
            debug.contains("https://auth.example.invalid"),
            "Debug must keep redacted origin: {debug}"
        );
    }
}

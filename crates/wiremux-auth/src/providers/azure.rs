//! Azure AD client-credentials TokenProvider.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use crate::TokenProvider;
use crate::error::AuthError;
use crate::helpers::{
    InFlight, duration_from_expires_in_secs, lead_or_follow, oauth_http_client, post_form_url,
    vendor_rejected_summary,
};

const DEFAULT_LIFETIME_SECS: u64 = 3600;
const DEFAULT_SCOPE: &str = "https://management.azure.com/.default";

struct CachedToken {
    access_token: String,
    acquired_at: Instant,
    lifetime: Duration,
}

impl CachedToken {
    fn needs_refresh(&self) -> bool {
        self.acquired_at.elapsed() >= self.lifetime.mul_f64(0.8)
    }
}

/// Azure AD client-credentials grant.
#[derive(Clone)]
pub struct AzureTokenProvider {
    inner: Arc<Inner>,
}

struct Inner {
    tenant_id: String,
    client_id: String,
    client_secret: String,
    scope: String,
    token_url: String,
    http: reqwest::Client,
    force_refresh: AtomicBool,
    state: RwLock<Option<CachedToken>>,
    inflight: InFlight,
}

impl std::fmt::Debug for AzureTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureTokenProvider")
            .field("tenant_id", &self.inner.tenant_id)
            .field("client_id", &self.inner.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("scope", &self.inner.scope)
            .field("token_url", &self.inner.token_url)
            .finish()
    }
}

impl AzureTokenProvider {
    /// Client-credentials provider for `login.microsoftonline.com/{tenant}/oauth2/v2.0/token`.
    pub fn new(
        tenant_id: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        scope: impl Into<String>,
    ) -> Result<Self, AuthError> {
        let tenant_id = tenant_id.into();
        let client_id = client_id.into();
        let client_secret = client_secret.into();
        let mut scope = scope.into();
        if scope.trim().is_empty() {
            scope = DEFAULT_SCOPE.to_string();
        }
        if tenant_id.trim().is_empty() {
            return Err(AuthError::MissingField("Azure tenant_id".into()));
        }
        if client_id.trim().is_empty() {
            return Err(AuthError::MissingField("Azure client_id".into()));
        }
        if client_secret.trim().is_empty() {
            return Err(AuthError::MissingField("Azure client_secret".into()));
        }
        let token_url = format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token");
        Ok(Self {
            inner: Arc::new(Inner {
                tenant_id,
                client_id,
                client_secret,
                scope,
                token_url,
                http: oauth_http_client()?,
                force_refresh: AtomicBool::new(false),
                state: RwLock::new(None),
                inflight: InFlight::new(),
            }),
        })
    }

    /// Override the token URL (tests, national clouds).
    #[must_use]
    pub fn with_token_url(self, token_url: impl Into<String>) -> Self {
        let mut inner = Arc::try_unwrap(self.inner).unwrap_or_else(|arc| (*arc).clone_inner());
        inner.token_url = token_url.into();
        Self {
            inner: Arc::new(inner),
        }
    }

    async fn refresh(&self, force: bool) -> Result<String, AuthError> {
        {
            let state = self.inner.state.read().await;
            if !force
                && let Some(tok) = state.as_ref()
                && !tok.needs_refresh()
            {
                return Ok(tok.access_token.clone());
            }
        }
        let form = [
            ("client_id", self.inner.client_id.as_str()),
            ("client_secret", self.inner.client_secret.as_str()),
            ("scope", self.inner.scope.as_str()),
            ("grant_type", "client_credentials"),
        ];
        let (status, body) = post_form_url(&self.inner.http, &self.inner.token_url, &form).await?;
        if !(200..300).contains(&status) {
            return Err(AuthError::VendorRejected {
                status,
                summary: vendor_rejected_summary(
                    "Azure token request failed",
                    &body,
                    &self.inner.token_url,
                ),
            });
        }
        let parsed = parse_azure_token(&body)?;
        if parsed.access_token.trim().is_empty() {
            return Err(AuthError::EmptyWriteRefused);
        }
        let lifetime =
            duration_from_expires_in_secs(parsed.expires_in.unwrap_or(DEFAULT_LIFETIME_SECS));
        let token = parsed.access_token.clone();
        *self.inner.state.write().await = Some(CachedToken {
            access_token: parsed.access_token,
            acquired_at: Instant::now(),
            lifetime,
        });
        Ok(token)
    }
}

impl Inner {
    fn clone_inner(&self) -> Self {
        Self {
            tenant_id: self.tenant_id.clone(),
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            scope: self.scope.clone(),
            token_url: self.token_url.clone(),
            http: self.http.clone(),
            force_refresh: AtomicBool::new(self.force_refresh.load(Ordering::SeqCst)),
            state: RwLock::new(None),
            inflight: InFlight::new(),
        }
    }
}

struct ParsedAzureToken {
    access_token: String,
    expires_in: Option<u64>,
}

fn parse_azure_token(body: &str) -> Result<ParsedAzureToken, AuthError> {
    let doc: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AuthError::TokenProvider(format!("Azure token: invalid JSON: {e}")))?;
    let access_token = doc
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if access_token.is_empty() {
        return Err(AuthError::TokenProvider(
            "Azure token response has no access_token".into(),
        ));
    }
    let expires_in = doc.get("expires_in").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });
    Ok(ParsedAzureToken {
        access_token,
        expires_in,
    })
}

impl TokenProvider for AzureTokenProvider {
    fn mark_stale(&self) {
        self.inner.force_refresh.store(true, Ordering::SeqCst);
    }

    async fn get_token(&self) -> Result<String, AuthError> {
        // load() not swap: a concurrent get_token must not see a cleared force
        // flag before the InFlight claim.
        {
            let state = self.inner.state.read().await;
            if !self.inner.force_refresh.load(Ordering::SeqCst)
                && let Some(tok) = state.as_ref()
                && !tok.needs_refresh()
                && !self.inner.inflight.is_busy()
            {
                return Ok(tok.access_token.clone());
            }
        }
        lead_or_follow(&self.inner.inflight, || async {
            let force = self.inner.force_refresh.swap(false, Ordering::SeqCst);
            let result = self.refresh(force).await;
            if result.is_err() && force {
                self.inner.force_refresh.store(true, Ordering::SeqCst);
            }
            result
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    fn read_http_headers(stream: &mut impl Read) -> Vec<u8> {
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
        buf
    }

    fn write_http_json(stream: &mut impl Write, status: u16, body: &str) {
        let resp = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    }

    fn spawn_http_server(status: u16, body: &str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("local_addr");
        let body = body.to_owned();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let buf = read_http_headers(&mut stream);
            write_http_json(&mut stream, status, &body);
            String::from_utf8_lossy(&buf).into_owned()
        });
        (format!("http://{addr}/oauth/token"), handle)
    }

    fn spawn_http_script(responses: &[(u16, &str)]) -> (String, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("local_addr");
        let responses: Vec<(u16, String)> = responses
            .iter()
            .map(|(status, body)| (*status, (*body).to_owned()))
            .collect();
        let handle = thread::spawn(move || {
            let mut served = 0usize;
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let _ = read_http_headers(&mut stream);
                write_http_json(&mut stream, status, &body);
                served += 1;
            }
            served
        });
        (format!("http://{addr}/oauth/token"), handle)
    }

    /// Cache-fill 200, then up to two delayed refresh 200s (leader + extra POST).
    fn spawn_http_cache_then_refresh(
        first: &str,
        refreshed: &str,
        refresh_delay: Duration,
        extra_idle: Duration,
    ) -> (String, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("local_addr");
        let first = first.to_owned();
        let refreshed = refreshed.to_owned();
        let handle = thread::spawn(move || {
            let mut served = 0usize;
            let first_deadline = Instant::now() + Duration::from_secs(5);
            let mut extra_deadline: Option<Instant> = None;
            while served < 3 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                        let _ = read_http_headers(&mut stream);
                        if served == 0 {
                            write_http_json(&mut stream, 200, &first);
                        } else {
                            if served == 1 && !refresh_delay.is_zero() {
                                thread::sleep(refresh_delay);
                            }
                            write_http_json(&mut stream, 200, &refreshed);
                            extra_deadline = Some(Instant::now() + extra_idle);
                        }
                        served += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        let limit = extra_deadline.unwrap_or(first_deadline);
                        if Instant::now() >= limit {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            served
        });
        (format!("http://{addr}/oauth/token"), handle)
    }

    #[tokio::test]
    async fn azure_client_credentials_returns_access_token() {
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"az-tok","expires_in":3600,"token_type":"Bearer"}"#,
        );
        let p = AzureTokenProvider::new("tenant", "client", "secret", "https://example/.default")
            .expect("new")
            .with_token_url(url);
        assert_eq!(p.get_token().await.expect("token"), "az-tok");
        let req = handle.join().expect("join");
        assert!(req.contains("grant_type=client_credentials"), "{req}");
        assert!(req.contains("client_id=client"), "{req}");
        assert!(req.contains("client_secret=secret"), "{req}");
    }

    #[tokio::test]
    async fn azure_cached_token_skips_second_http() {
        let (url, handle) =
            spawn_http_server(200, r#"{"access_token":"az-tok","expires_in":3600}"#);
        let p = AzureTokenProvider::new("tenant", "client", "secret", "scope")
            .expect("new")
            .with_token_url(url);
        assert_eq!(p.get_token().await.expect("first"), "az-tok");
        assert_eq!(p.get_token().await.expect("cached"), "az-tok");
        let _ = handle.join();
    }

    #[test]
    fn azure_debug_redacts_secret() {
        let p = AzureTokenProvider::new("tenant", "client", "super-secret", "scope").expect("new");
        let debug = format!("{p:?}");
        assert!(!debug.contains("super-secret"), "{debug}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
    }

    #[test]
    fn azure_empty_secret_fails() {
        let err = AzureTokenProvider::new("t", "c", "", "s").expect_err("empty");
        assert!(err.to_string().contains("client_secret"), "{err}");
    }

    #[tokio::test]
    async fn forced_refresh_http_error_restores_force_flag() {
        let (url, handle) = spawn_http_script(&[
            (200, r#"{"access_token":"az-first","expires_in":3600}"#),
            (401, r#"{"error":"invalid_client"}"#),
            (200, r#"{"access_token":"az-retry","expires_in":3600}"#),
        ]);
        let p = AzureTokenProvider::new("tenant", "client", "secret", "scope")
            .expect("new")
            .with_token_url(url);
        assert_eq!(p.get_token().await.expect("cache"), "az-first");
        p.mark_stale();
        let err = p.get_token().await.expect_err("forced 401");
        let msg = err.to_string();
        assert!(msg.contains("401") || msg.contains("Azure"), "{msg}");
        assert_eq!(
            msg.matches("(HTTP ").count(),
            1,
            "VendorRejected must not re-wrap HTTP: {msg}"
        );
        assert_eq!(
            p.get_token().await.expect("retry after failed force"),
            "az-retry"
        );
        assert_eq!(handle.join().expect("join"), 3);
    }

    #[tokio::test]
    async fn concurrent_mark_stale_does_not_serve_stale_cache() {
        let first = r#"{"access_token":"az-first","expires_in":3600}"#;
        let second = r#"{"access_token":"az-second","expires_in":3600}"#;
        let (url, handle) = spawn_http_cache_then_refresh(
            first,
            second,
            Duration::from_millis(80),
            Duration::from_millis(200),
        );
        let p = AzureTokenProvider::new("tenant", "client", "secret", "scope")
            .expect("new")
            .with_token_url(url);
        assert_eq!(p.get_token().await.expect("cache"), "az-first");
        p.mark_stale();
        let a = p.clone();
        let b = p.clone();
        let (left, right) = tokio::join!(a.get_token(), b.get_token());
        let left = left.expect("left");
        let right = right.expect("right");
        assert_ne!(left, "az-first", "must not return just-staled cache");
        assert_ne!(right, "az-first", "must not return just-staled cache");
        assert_eq!(left, "az-second");
        assert_eq!(right, "az-second");
        let served = handle.join().expect("join");
        assert!(
            served >= 2,
            "cache fill plus at least one refresh, got {served}"
        );
    }
}

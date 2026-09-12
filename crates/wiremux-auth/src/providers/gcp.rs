//! Google Cloud service-account JWT TokenProvider.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::sync::RwLock;

use crate::TokenProvider;
use crate::error::AuthError;
use crate::helpers::{
    InFlight, MAX_CREDS_BYTES, duration_from_expires_in_secs, format_oauth_http_error,
    lead_or_follow, oauth_http_client, post_form_url,
};

const DEFAULT_LIFETIME_SECS: u64 = 3600;
const DEFAULT_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

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

/// Service-account JWT bearer grant.
#[derive(Clone)]
pub struct GcpTokenProvider {
    inner: Arc<Inner>,
}

struct Inner {
    client_email: String,
    private_key_pem: String,
    token_uri: String,
    scope: String,
    http: reqwest::Client,
    force_refresh: AtomicBool,
    state: RwLock<Option<CachedToken>>,
    inflight: InFlight,
}

impl std::fmt::Debug for GcpTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcpTokenProvider")
            .field("client_email", &self.inner.client_email)
            .field("private_key", &"[REDACTED]")
            .field("token_uri", &self.inner.token_uri)
            .field("scope", &self.inner.scope)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    private_key: String,
    #[serde(default)]
    token_uri: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

impl GcpTokenProvider {
    /// Build from a service-account JSON key file.
    pub fn from_key_file(path: impl AsRef<Path>) -> Result<Self, AuthError> {
        let path = path.as_ref();
        let meta =
            std::fs::metadata(path).map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;
        if meta.len() > MAX_CREDS_BYTES {
            return Err(AuthError::TokenProvider(format!(
                "GCP key file too large ({} bytes)",
                meta.len()
            )));
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;
        Self::from_key(&text)
    }

    /// Build from service-account JSON (`client_email`, `private_key`, optional `token_uri`).
    pub fn from_key(json: &str) -> Result<Self, AuthError> {
        let key: ServiceAccountKey = serde_json::from_str(json)
            .map_err(|e| AuthError::TokenProvider(format!("GCP service-account key: {e}")))?;
        if key.client_email.trim().is_empty() {
            return Err(AuthError::MissingField("client_email".into()));
        }
        if key.private_key.trim().is_empty() {
            return Err(AuthError::MissingField("private_key".into()));
        }
        let token_uri = key
            .token_uri
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_TOKEN_URI.to_string());
        let scope = key
            .scope
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SCOPE.to_string());
        Ok(Self {
            inner: Arc::new(Inner {
                client_email: key.client_email,
                private_key_pem: key.private_key,
                token_uri,
                scope,
                http: oauth_http_client()?,
                force_refresh: AtomicBool::new(false),
                state: RwLock::new(None),
                inflight: InFlight::new(),
            }),
        })
    }

    fn signed_jwt(&self) -> Result<String, AuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let claims = GcpJwtClaims {
            iss: self.inner.client_email.clone(),
            sub: self.inner.client_email.clone(),
            aud: self.inner.token_uri.clone(),
            iat: now,
            exp: now + 3600,
            scope: self.inner.scope.clone(),
        };
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.typ = Some("JWT".into());
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(self.inner.private_key_pem.as_bytes())
            .map_err(|e| AuthError::TokenProvider(format!("GCP private_key PEM: {e}")))?;
        jsonwebtoken::encode(&header, &claims, &key)
            .map_err(|e| AuthError::TokenProvider(format!("GCP JWT sign: {e}")))
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
        let assertion = self.signed_jwt()?;
        let form = [
            ("grant_type", JWT_BEARER),
            ("assertion", assertion.as_str()),
        ];
        let (status, body) = post_form_url(&self.inner.http, &self.inner.token_uri, &form).await?;
        if !(200..300).contains(&status) {
            return Err(AuthError::VendorRejected {
                status,
                summary: format_oauth_http_error(
                    "GCP token request failed",
                    status,
                    &body,
                    &self.inner.token_uri,
                ),
            });
        }
        let parsed = parse_gcp_token(&body)?;
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

#[derive(serde::Serialize)]
struct GcpJwtClaims {
    iss: String,
    sub: String,
    aud: String,
    iat: u64,
    exp: u64,
    scope: String,
}

struct ParsedGcpToken {
    access_token: String,
    expires_in: Option<u64>,
}

fn parse_gcp_token(body: &str) -> Result<ParsedGcpToken, AuthError> {
    let doc: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AuthError::TokenProvider(format!("GCP token: invalid JSON: {e}")))?;
    let access_token = doc
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if access_token.is_empty() {
        return Err(AuthError::TokenProvider(
            "GCP token response has no access_token".into(),
        ));
    }
    let expires_in = doc.get("expires_in").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });
    Ok(ParsedGcpToken {
        access_token,
        expires_in,
    })
}

impl TokenProvider for GcpTokenProvider {
    fn mark_stale(&self) {
        self.inner.force_refresh.store(true, Ordering::SeqCst);
    }

    async fn get_token(&self) -> Result<String, AuthError> {
        let force = self.inner.force_refresh.swap(false, Ordering::SeqCst);
        {
            let state = self.inner.state.read().await;
            if !force
                && let Some(tok) = state.as_ref()
                && !tok.needs_refresh()
                && !self.inner.inflight.is_busy()
            {
                return Ok(tok.access_token.clone());
            }
        }
        match lead_or_follow(&self.inner.inflight, || self.refresh(force)).await {
            Ok(token) => Ok(token),
            Err(err) => {
                if force {
                    self.inner.force_refresh.store(true, Ordering::SeqCst);
                }
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    // Test-only PKCS8 RSA key (openssl genpkey). Not a production secret.
    const TEST_PKCS8: &str = "-----BEGIN PRIVATE KEY-----
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQC8amrIB7iFClhT
MSWHQWnUlrBAL7amaNd1+LS1ja7l8o4LVhviO8i1odLnLKnwqEpDqlSenNwY4Fkx
Zb3y8IKkZ2Iy6wahHyeLNXyC7BJnGX1pm2XVRWnQB5PjGBr1Gm3ySmXf3Pq5iOyQ
NyykJtPT3Lmh7u5Nx4LkF/FLP++NRoWT31MpGHz9I0nWjyWOaDDB2Tu31+fU9p1H
bVN4WT3GAimy3c4yLMZeP6sirEBr7MPPSPcpJSNDSrQcul+wEjPeJzgv0yKYAoZD
gVCrc1eKchV3FzPwO30XBNeTS0Hg0pGb7/JbSpAU9OnyEJVMAB7TMJBqdBEIrCLj
6lEEgen1AgMBAAECggEAAIIL+UrhIKG69pEf3s8IEX/315AY4pu7ZGC7QRbJCYoa
FujyfnsFY8kwXPT0z33fVSTi/2X+3qXQzwUikXT2bXO24BhUfR/fYQcKGiS/hp7Z
EGlfqmYN2GeHfhdYCBx6OM3UqxdkfoMFTJ/RN0SR9LqcA7M9jHRRdwaTj4P8Cd/s
QAftCe7ZNgZU7Mf6F3DV7pgPwfvA0vUADKWuyawCq1Yp+9BHlMn9H39fMchW2bNR
V8Qo5dp+FUEOQM41gcHVHtyelaU/efy/GoHTOCPFgxykR3CdMB1s0ShvbfDUZDia
R03IY3cByHQJ/UDO6SeiY0Hxyycigc0pPjLJNkvldQKBgQDrxb4OKjuxuePFMHiB
G3VhoWVa8i8OhcKD4q7XoBnnItdsjFIGi6l2Mxn+yHFTTwt/6DeiTcw93kUYkG7S
XhqQEb+5Kks1E3SX4YH+s0m0W7skyR4aZZLwar6WnbHk+g+5zdWG4i1zQ+thjiiB
wZAJOTiR81q/alR9iSvmSZkKnwKBgQDMlJTECVfSYhJ1YerKssHud0I9mzYOrTEP
f9NHQvYYq4fVMriPWtUDSrL2D9TEqgx9u7WfCL+XCwnr6qHaMy6yM92m2mWunWNN
HB5y52emd/EdyB5i0FKkPQ2aBWyKDFnOD/UFWLX50UBMK7JG35Eall7JBowBBEic
UP/PD7SW6wKBgQCyhWTV5uaSONWdHolwALGNfh53kX9N+LwDDqYiwKg8WiZRm6IU
MLXcuO7K+0zLrsNfUx6k91FZ2y3oXpx7DyP/yGCqPLr7ckLLKcY7a9e4B+kY/mub
wyNShRDQjJEBdtJndtJiMmoFp/zXPkOvlDeStFAAOwqQe1uEPlQOJ9YIswKBgQCZ
z4f9z6x4n4WTPVgip71Ixd9GpEBDTpFZPtihdkXCjIxmjWjXVwpaHDpq58InTlZv
3cYSWKh7LjB6cADaJasRDg+y1/alDu3O1rpJ15NFRF5C7udxkYDgvIpSZ4uQSvLm
C3dDWswOk/WMjznNMV9OJwoCh+qRBSB2biu2CO/UmwKBgQCrLsCyMAnrB8kPtUUv
gdmsOkF73DglCWe2TXjDGVGkKe81D1SwTD+AphwKuaXCH7go5iWfHBeXZi4ZNhOP
9s6eqEDT3hWzZA9/S1xzz2pWe9J1c/gNztvP/N1oCJ+UExv0lf8cZAJl5KdpZ/GU
c+5RXVheoFNjzJpbLyOIeEEttw==
-----END PRIVATE KEY-----
";

    fn test_key_json(token_uri: &str) -> String {
        serde_json::json!({
            "type": "service_account",
            "client_email": "svc@example.iam.gserviceaccount.com",
            "private_key": TEST_PKCS8,
            "token_uri": token_uri,
        })
        .to_string()
    }

    fn read_http_headers(stream: &mut impl Read) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
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
        (format!("http://{addr}/token"), handle)
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
        (format!("http://{addr}/token"), handle)
    }

    #[tokio::test]
    async fn gcp_jwt_bearer_returns_access_token() {
        let (url, handle) = spawn_http_server(
            200,
            r#"{"access_token":"ya29.gcp","expires_in":3600,"token_type":"Bearer"}"#,
        );
        let p = GcpTokenProvider::from_key(&test_key_json(&url)).expect("from_key");
        assert_eq!(p.get_token().await.expect("token"), "ya29.gcp");
        let req = handle.join().expect("join");
        assert!(
            req.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer")
                || req.contains("grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer"),
            "{req}"
        );
        assert!(req.contains("assertion="), "{req}");
    }

    #[tokio::test]
    async fn gcp_from_key_file_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sa.json");
        let (url, handle) =
            spawn_http_server(200, r#"{"access_token":"from-file","expires_in":60}"#);
        std::fs::write(&path, test_key_json(&url)).expect("write key");
        let p = GcpTokenProvider::from_key_file(&path).expect("from_key_file");
        assert_eq!(p.get_token().await.expect("token"), "from-file");
        let _ = handle.join();
    }

    #[test]
    fn gcp_debug_redacts_private_key() {
        let json = test_key_json("https://oauth2.googleapis.com/token");
        let p = GcpTokenProvider::from_key(&json).expect("from_key");
        let debug = format!("{p:?}");
        assert!(!debug.contains("BEGIN PRIVATE"), "{debug}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
    }

    #[test]
    fn gcp_missing_email_fails() {
        let err = GcpTokenProvider::from_key(r#"{"private_key":"x"}"#).expect_err("missing");
        assert!(err.to_string().contains("client_email"), "{err}");
    }

    #[tokio::test]
    async fn forced_refresh_http_error_restores_force_flag() {
        let (url, handle) = spawn_http_script(&[
            (200, r#"{"access_token":"ya29.first","expires_in":3600}"#),
            (401, r#"{"error":"invalid_grant"}"#),
            (200, r#"{"access_token":"ya29.retry","expires_in":3600}"#),
        ]);
        let p = GcpTokenProvider::from_key(&test_key_json(&url)).expect("from_key");
        assert_eq!(p.get_token().await.expect("cache"), "ya29.first");
        p.mark_stale();
        let err = p.get_token().await.expect_err("forced 401");
        assert!(
            err.to_string().contains("401") || err.to_string().contains("GCP"),
            "{err}"
        );
        assert_eq!(
            p.get_token().await.expect("retry after failed force"),
            "ya29.retry"
        );
        assert_eq!(handle.join().expect("join"), 3);
    }
}

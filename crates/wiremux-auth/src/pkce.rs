//! Generic PKCE (RFC 7636) for profile-driven login. Login CLI is a later PR.

use std::collections::BTreeMap;

use crate::error::AuthError;
use crate::helpers::{
    format_oauth_http_error, format_oauth_transport_error, oauth_http_client, percent_encode,
    read_oauth_body,
};
use crate::profile::OauthPack;

pub use crate::exchange::TokenExchangeResponse;

/// PKCE challenge pair. `Debug` redacts the verifier.
#[derive(Clone)]
pub struct PkceChallenge {
    /// Random code verifier (base64url, no padding).
    pub code_verifier: String,
    /// S256 challenge (base64url SHA-256 of the verifier).
    pub code_challenge: String,
    /// CSRF state.
    pub state: String,
}

impl std::fmt::Debug for PkceChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkceChallenge")
            .field("code_verifier", &"[REDACTED]")
            .field("code_challenge", &self.code_challenge)
            .field("state", &self.state)
            .finish()
    }
}

/// Generate a PKCE verifier, S256 challenge, and state.
pub fn generate_pkce() -> Result<PkceChallenge, AuthError> {
    let verifier_bytes = random_bytes(32);
    let code_verifier = base64_url_encode(&verifier_bytes);
    let hash = sha256(code_verifier.as_bytes());
    let code_challenge = base64_url_encode(&hash);
    let state = base64_url_encode(&random_bytes(16));
    Ok(PkceChallenge {
        code_verifier,
        code_challenge,
        state,
    })
}

/// Build an authorize URL from an `[oauth]` pack (plus extra query params).
pub fn build_auth_url_from_oauth(
    oauth: &OauthPack,
    pkce: &PkceChallenge,
) -> Result<String, AuthError> {
    let authorize_url = oauth
        .authorize_url
        .as_deref()
        .ok_or_else(|| AuthError::MissingField("oauth.authorize_url".into()))?;
    let client_id = oauth.client_id.as_deref().unwrap_or("");
    if client_id.is_empty() {
        return Err(AuthError::MissingField("oauth.client_id".into()));
    }
    let redirect = oauth.redirect_uri.as_deref().unwrap_or("");
    let scope = if oauth.scopes.is_empty() {
        None
    } else {
        Some(oauth.scopes.join(" "))
    };
    Ok(build_auth_url(
        authorize_url,
        client_id,
        redirect,
        scope.as_deref(),
        pkce,
        &oauth.authorize_params,
    ))
}

/// Build an authorization URL for the Authorization Code + PKCE flow.
pub fn build_auth_url(
    authorize_url: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: Option<&str>,
    pkce: &PkceChallenge,
    extra_params: &BTreeMap<String, String>,
) -> String {
    let mut url = format!(
        "{authorize_url}?client_id={}\
         &response_type=code\
         &redirect_uri={}\
         &code_challenge={}\
         &code_challenge_method=S256\
         &state={}",
        percent_encode(client_id),
        percent_encode(redirect_uri),
        percent_encode(&pkce.code_challenge),
        percent_encode(&pkce.state),
    );
    if let Some(s) = scope {
        url.push_str(&format!("&scope={}", percent_encode(s)));
    }
    for (k, v) in extra_params {
        if is_reserved_authorize_param(k) {
            continue;
        }
        url.push('&');
        url.push_str(&percent_encode(k));
        url.push('=');
        url.push_str(&percent_encode(v));
    }
    url
}

/// Overlay extras must not overwrite engine OAuth fields. Case-insensitive.
fn is_reserved_authorize_param(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "redirect_uri"
            | "response_type"
            | "client_id"
            | "code_challenge"
            | "code_challenge_method"
            | "state"
            | "code_verifier"
            | "grant_type"
            | "scope"
    )
}

/// Exchange an authorization code for tokens.
pub async fn exchange_auth_code(
    token_url: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenExchangeResponse, AuthError> {
    let http = oauth_http_client()?;
    let resp = http
        .post(token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| {
            AuthError::TokenProvider(format_oauth_transport_error(
                "auth code exchange failed",
                &e,
                token_url,
            ))
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = read_oauth_body(resp).await.unwrap_or_default();
        return Err(AuthError::TokenProvider(format_oauth_http_error(
            "auth code exchange failed",
            status,
            &body,
            token_url,
        )));
    }

    let body = read_oauth_body(resp).await?;
    serde_json::from_str::<TokenExchangeResponse>(&body)
        .map_err(|e| AuthError::TokenProvider(format!("auth code exchange: invalid JSON: {e}")))
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).expect("OS entropy source unavailable");
    buf
}

fn sha256(data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).to_vec()
}

fn base64_url_encode(data: &[u8]) -> String {
    let encoded = base64_encode(data);
    encoded
        .chars()
        .filter_map(|c| match c {
            '+' => Some('-'),
            '/' => Some('_'),
            '=' => None,
            other => Some(other),
        })
        .collect()
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_pkce_redacts_verifier() {
        let pkce = generate_pkce().expect("pkce");
        assert!(!pkce.code_verifier.is_empty());
        assert!(!pkce.code_challenge.is_empty());
        assert!(!pkce.state.is_empty());
        assert!(!pkce.code_verifier.contains('='));
        let debug = format!("{pkce:?}");
        assert!(
            !debug.contains(&pkce.code_verifier),
            "Debug must redact verifier: {debug}"
        );
        assert!(debug.contains("[REDACTED]"));
        assert!(debug.contains(&pkce.code_challenge));
    }

    #[test]
    fn generate_pkce_unique() {
        let a = generate_pkce().unwrap();
        let b = generate_pkce().unwrap();
        assert_ne!(a.code_verifier, b.code_verifier);
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn build_auth_url_includes_authorize_params() {
        let pkce = PkceChallenge {
            code_verifier: "v".into(),
            code_challenge: "c".into(),
            state: "s".into(),
        };
        let mut extra = BTreeMap::new();
        extra.insert("audience".into(), "inference".into());
        let url = build_auth_url(
            "https://auth.example.invalid/authorize",
            "client-1",
            "http://localhost:9/cb",
            Some("openid"),
            &pkce,
            &extra,
        );
        assert!(url.contains("client_id=client-1"));
        assert!(url.contains("code_challenge=c"));
        assert!(url.contains("audience=inference"));
        assert!(url.contains("scope=openid"));
    }

    #[test]
    fn build_auth_url_drops_reserved_redirect_uri() {
        let pkce = PkceChallenge {
            code_verifier: "v".into(),
            code_challenge: "c".into(),
            state: "s".into(),
        };
        let mut extra = BTreeMap::new();
        extra.insert("redirect_uri".into(), "https://evil.example".into());
        extra.insert("Redirect_URI".into(), "https://evil.example/upper".into());
        extra.insert("client_id".into(), "evil-client".into());
        extra.insert("state".into(), "hijack".into());
        extra.insert("code_verifier".into(), "leak-verifier".into());
        extra.insert("audience".into(), "inference".into());
        extra.insert("resource".into(), "api".into());
        extra.insert("prompt".into(), "consent".into());
        let url = build_auth_url(
            "https://auth.example.invalid/authorize",
            "client-1",
            "http://localhost:9/cb",
            Some("openid"),
            &pkce,
            &extra,
        );
        assert!(
            !url.contains("evil.example"),
            "reserved overlay redirect_uri must be dropped: {url}"
        );
        assert!(!url.contains("evil-client"), "{url}");
        assert!(!url.contains("hijack"), "{url}");
        assert!(!url.contains("leak-verifier"), "{url}");
        assert_eq!(
            url.matches("redirect_uri=").count(),
            1,
            "engine redirect_uri must appear exactly once: {url}"
        );
        assert!(
            url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A9%2Fcb"),
            "engine loopback redirect_uri missing: {url}"
        );
        assert!(url.contains("audience=inference"), "{url}");
        assert!(url.contains("resource=api"), "{url}");
        assert!(url.contains("prompt=consent"), "{url}");
    }

    #[test]
    fn build_auth_url_drops_reserved_scope() {
        let pkce = PkceChallenge {
            code_verifier: "v".into(),
            code_challenge: "c".into(),
            state: "s".into(),
        };
        let mut extra = BTreeMap::new();
        extra.insert("scope".into(), "evil-scope".into());
        extra.insert("SCOPE".into(), "EVIL".into());
        extra.insert("audience".into(), "inference".into());
        let url = build_auth_url(
            "https://auth.example.invalid/authorize",
            "client-1",
            "http://localhost:9/cb",
            Some("openid profile"),
            &pkce,
            &extra,
        );
        assert!(
            !url.contains("evil-scope"),
            "reserved overlay scope must be dropped: {url}"
        );
        assert!(!url.contains("EVIL"), "{url}");
        assert_eq!(
            url.matches("scope=").count(),
            1,
            "engine scope must appear exactly once: {url}"
        );
        assert!(
            url.contains("scope=openid%20profile") || url.contains("scope=openid+profile"),
            "engine scope missing: {url}"
        );
        assert!(url.contains("audience=inference"), "{url}");
    }

    #[test]
    fn token_exchange_debug_redacts() {
        let resp = TokenExchangeResponse {
            access_token: "sk-secret".into(),
            refresh_token: Some("rt-secret".into()),
            expires_in: Some(60),
            token_type: Some("Bearer".into()),
            scope: None,
        };
        let debug = format!("{resp:?}");
        assert!(!debug.contains("sk-secret"));
        assert!(!debug.contains("rt-secret"));
    }

    #[tokio::test]
    async fn exchange_transport_error_redacts_userinfo_and_secret() {
        let url = "https://user:s3cret@127.0.0.1:1/oauth/token?client_secret=supersecret";
        let err = exchange_auth_code(url, "cid", "code", "http://127.0.0.1/cb", "ver")
            .await
            .expect_err("closed port must fail");
        let msg = err.to_string();
        assert!(
            !msg.contains("client_secret="),
            "transport error leaked query: {msg}"
        );
        assert!(
            !msg.contains("s3cret"),
            "transport error leaked userinfo: {msg}"
        );
        assert!(
            !msg.contains("supersecret"),
            "transport error leaked secret: {msg}"
        );
        assert!(
            !msg.contains("user:"),
            "transport error leaked userinfo: {msg}"
        );
    }
}

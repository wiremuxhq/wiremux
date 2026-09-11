//! OAuth 2.0 device authorization grant (RFC 8628). Poll is bounded.

use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::AuthError;
use crate::exchange::TokenExchangeResponse;
use crate::helpers::{format_oauth_http_error, oauth_http_client, read_oauth_body};
use crate::profile::OauthPack;

/// Response from the device authorization endpoint.
#[derive(Clone, Deserialize)]
pub struct DeviceAuthResponse {
    /// Device verification code (poll secret).
    pub device_code: String,
    /// User-facing code.
    pub user_code: String,
    /// URL where the user enters the code.
    pub verification_uri: String,
    /// Optional complete URL with the code pre-filled.
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    /// Device-code lifetime in seconds.
    #[serde(default = "default_device_expires")]
    pub expires_in: u64,
    /// Suggested poll interval in seconds.
    #[serde(default = "default_device_interval")]
    pub interval: u64,
}

impl std::fmt::Debug for DeviceAuthResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAuthResponse")
            .field("device_code", &"[REDACTED]")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("verification_uri_complete", &self.verification_uri_complete)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish()
    }
}

fn default_device_expires() -> u64 {
    900
}
fn default_device_interval() -> u64 {
    5
}

/// Hard cap so an agent-driven script cannot hang.
pub const MAX_DEVICE_POLL_INTERVAL_SECS: u64 = 60;
/// Hard cap on the whole poll loop.
pub const MAX_DEVICE_POLL_TIMEOUT_SECS: u64 = 15 * 60;

/// Cap a vendor interval at 60s (minimum 1s).
#[must_use]
pub fn device_poll_interval(interval: u64) -> Duration {
    Duration::from_secs(interval.clamp(1, MAX_DEVICE_POLL_INTERVAL_SECS))
}

/// Cap a vendor/user timeout at 15 minutes.
#[must_use]
pub fn device_poll_timeout(timeout_secs: u64) -> Duration {
    Duration::from_secs(timeout_secs.min(MAX_DEVICE_POLL_TIMEOUT_SECS))
}

#[derive(Debug, Clone, Deserialize)]
struct DevicePollError {
    error: String,
}

/// Start device authorization using `[oauth]` fields only.
pub async fn start_device_flow_from_oauth(
    oauth: &OauthPack,
) -> Result<DeviceAuthResponse, AuthError> {
    let url = oauth
        .device_auth_url
        .as_deref()
        .ok_or_else(|| AuthError::MissingField("oauth.device_auth_url".into()))?;
    let client_id = oauth.client_id.as_deref().unwrap_or("");
    if client_id.is_empty() {
        return Err(AuthError::MissingField("oauth.client_id".into()));
    }
    let scope = if oauth.scopes.is_empty() {
        None
    } else {
        Some(oauth.scopes.join(" "))
    };
    let headers: Vec<(&str, &str)> = oauth
        .token_headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    start_device_flow(
        url,
        client_id,
        scope.as_deref(),
        if headers.is_empty() {
            None
        } else {
            Some(&headers)
        },
    )
    .await
}

/// Start the device authorization flow (RFC 8628).
pub async fn start_device_flow(
    device_auth_url: &str,
    client_id: &str,
    scope: Option<&str>,
    extra_headers: Option<&[(&str, &str)]>,
) -> Result<DeviceAuthResponse, AuthError> {
    let http = oauth_http_client()?;
    let mut params = vec![("client_id", client_id.to_owned())];
    if let Some(s) = scope {
        params.push(("scope", s.to_owned()));
    }

    let mut req = http.post(device_auth_url).form(&params);
    if let Some(headers) = extra_headers {
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
    }

    let resp = req.send().await.map_err(|e| {
        AuthError::TokenProvider(format!("device authorization request failed: {e}"))
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = read_oauth_body(resp).await.unwrap_or_default();
        return Err(AuthError::TokenProvider(format_oauth_http_error(
            "device authorization failed",
            status,
            &body,
            device_auth_url,
        )));
    }

    let body = read_oauth_body(resp).await?;
    serde_json::from_str::<DeviceAuthResponse>(&body)
        .map_err(|e| AuthError::TokenProvider(format!("device authorization: invalid JSON: {e}")))
}

/// Poll the token endpoint until the user authorizes the device code.
///
/// Interval is capped at 60s. The whole loop is capped at 15 minutes.
pub async fn poll_device_token(
    token_url: &str,
    client_id: &str,
    device_code: &str,
    interval: u64,
    timeout_secs: u64,
    extra_headers: Option<&[(&str, &str)]>,
) -> Result<TokenExchangeResponse, AuthError> {
    let http = oauth_http_client()?;
    let start = Instant::now();
    let mut poll_interval = device_poll_interval(interval);
    let timeout = device_poll_timeout(timeout_secs);

    loop {
        if start.elapsed() > timeout {
            return Err(AuthError::TokenProvider(
                "device authorization timed out waiting for user approval".into(),
            ));
        }

        tokio::time::sleep(poll_interval).await;

        let mut req = http.post(token_url).form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", client_id),
        ]);
        if let Some(headers) = extra_headers {
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
        }

        let resp = req
            .send()
            .await
            .map_err(|e| AuthError::TokenProvider(format!("device token poll failed: {e}")))?;

        let status = resp.status();
        let body = read_oauth_body(resp).await?;
        if status.is_success() {
            return serde_json::from_str::<TokenExchangeResponse>(&body)
                .map_err(|e| AuthError::TokenProvider(format!("device token: invalid JSON: {e}")));
        }

        if let Ok(err) = serde_json::from_str::<DevicePollError>(&body) {
            match err.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    poll_interval = poll_interval
                        .saturating_add(Duration::from_secs(5))
                        .min(Duration::from_secs(MAX_DEVICE_POLL_INTERVAL_SECS));
                    continue;
                }
                "expired_token" => {
                    return Err(AuthError::TokenProvider(
                        "device code expired; please restart the login flow".into(),
                    ));
                }
                "access_denied" => {
                    return Err(AuthError::TokenProvider(
                        "authorization denied by user".into(),
                    ));
                }
                _ => {
                    return Err(AuthError::TokenProvider(format_oauth_http_error(
                        "device token error",
                        status,
                        &body,
                        token_url,
                    )));
                }
            }
        }

        return Err(AuthError::TokenProvider(format_oauth_http_error(
            "device token: unexpected response",
            status,
            &body,
            token_url,
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_auth_response_debug_redacts_device_code() {
        let resp = DeviceAuthResponse {
            device_code: "dc-secret-should-not-appear".into(),
            user_code: "ABCD-1234".into(),
            verification_uri: "https://auth.example.invalid/device".into(),
            verification_uri_complete: None,
            expires_in: 900,
            interval: 5,
        };
        let debug = format!("{resp:?}");
        assert!(
            !debug.contains("dc-secret-should-not-appear"),
            "device_code leaked: {debug}"
        );
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(debug.contains("ABCD-1234"), "{debug}");
    }

    #[test]
    fn device_auth_response_deserialize() {
        let json = r#"{
            "device_code": "dc-123",
            "user_code": "ABCD-1234",
            "verification_uri": "https://auth.example.invalid/device",
            "interval": 10
        }"#;
        let resp: DeviceAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.device_code, "dc-123");
        assert_eq!(resp.user_code, "ABCD-1234");
        assert_eq!(resp.interval, 10);
        assert_eq!(resp.expires_in, 900);
    }

    #[test]
    fn poll_interval_capped_at_60s() {
        assert_eq!(device_poll_interval(0), Duration::from_secs(1));
        assert_eq!(device_poll_interval(5), Duration::from_secs(5));
        assert_eq!(device_poll_interval(60), Duration::from_secs(60));
        assert_eq!(device_poll_interval(600), Duration::from_secs(60));
        assert_eq!(MAX_DEVICE_POLL_INTERVAL_SECS, 60);
    }

    #[test]
    fn poll_timeout_capped_at_15_minutes() {
        assert_eq!(device_poll_timeout(30), Duration::from_secs(30));
        assert_eq!(device_poll_timeout(u64::MAX), Duration::from_secs(15 * 60));
        assert_eq!(MAX_DEVICE_POLL_TIMEOUT_SECS, 15 * 60);
    }
}

//! Public-API tests for TokenProvider, PKCE, and device-flow caps.

use wiremux_auth::device_flow::{
    MAX_DEVICE_POLL_INTERVAL_SECS, MAX_DEVICE_POLL_TIMEOUT_SECS, device_poll_interval,
    device_poll_timeout,
};
use wiremux_auth::pkce::generate_pkce;
use wiremux_auth::{AnyTokenProvider, StaticToken, TokenProvider, parse_profile_str};

#[tokio::test]
async fn static_token_via_any_provider() {
    let provider: AnyTokenProvider = StaticToken::new("test-key").into();
    assert_eq!(provider.get_token().await.unwrap(), "test-key");
}

#[test]
fn pkce_debug_redacts_verifier() {
    let pkce = generate_pkce().expect("pkce");
    let debug = format!("{pkce:?}");
    assert!(!debug.contains(&pkce.code_verifier));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn device_poll_caps() {
    assert_eq!(MAX_DEVICE_POLL_INTERVAL_SECS, 60);
    assert_eq!(MAX_DEVICE_POLL_TIMEOUT_SECS, 15 * 60);
    assert_eq!(
        device_poll_interval(u64::MAX).as_secs(),
        MAX_DEVICE_POLL_INTERVAL_SECS
    );
    assert_eq!(
        device_poll_timeout(u64::MAX).as_secs(),
        MAX_DEVICE_POLL_TIMEOUT_SECS
    );
}

#[test]
fn provider_from_profile_requires_oauth_table() {
    let profile = parse_profile_str("schema_version = 1\nid = \"x\"\n").unwrap();
    let err = wiremux_auth::provider_from_profile(&profile).unwrap_err();
    assert!(err.to_string().contains("[oauth]"));
}

//! Public-API tests for TokenProvider, PKCE, and device-flow caps.

use wiremux_auth::device_flow::{
    MAX_DEVICE_POLL_INTERVAL_SECS, MAX_DEVICE_POLL_TIMEOUT_SECS, device_poll_interval,
    device_poll_timeout,
};
use wiremux_auth::pkce::generate_pkce;
use wiremux_auth::{AnyTokenProvider, IsolatedHome, StaticToken, TokenProvider, parse_profile_str};

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

#[test]
fn missing_creds_names_configured_path_or_env() {
    let home = IsolatedHome::with_extra_envs(&["WIREMUX_TEST_MISSING_ACCESS"]);
    let creds = home.path().join("missing-store").join("creds.json");
    let creds_unix = creds.to_string_lossy().replace('\\', "/");
    let profile = parse_profile_str(&format!(
        r#"
schema_version = 1
id = "missing-store"
[oauth]
token_url = "https://auth.example.invalid/token"
creds_path = "{creds_unix}"
access_env = "WIREMUX_TEST_MISSING_ACCESS"
"#
    ))
    .expect("test profile");
    let err = wiremux_auth::provider_from_profile(&profile)
        .expect_err("missing file and unset env must fail");
    let msg = err.to_string();
    let names_path_and_env = (msg.contains(&creds_unix)
        || msg.contains(creds.to_string_lossy().as_ref()))
        && msg.contains("access_env=WIREMUX_TEST_MISSING_ACCESS");
    let names_tried_stores =
        msg.contains("tried creds_path=") && msg.contains("access_env=WIREMUX_TEST_MISSING_ACCESS");
    assert!(
        names_path_and_env || names_tried_stores,
        "missing creds must name configured path and access_env=WIREMUX_TEST_MISSING_ACCESS, got {msg}"
    );
}

#[test]
fn creds_path_parent_dir_is_refused() {
    let _home = IsolatedHome::new();
    let profile = parse_profile_str(
        r#"
schema_version = 1
id = "escape-home"
[oauth]
token_url = "https://auth.example.invalid/token"
creds_path = "~/ok/../escaped.json"
"#,
    )
    .expect("test profile");
    let err = wiremux_auth::provider_from_profile(&profile)
        .expect_err("parent-dir creds_path must fail closed");
    let msg = err.to_string();
    assert!(
        msg.contains("oauth.creds_path"),
        "error must name oauth.creds_path, got {msg}"
    );
    assert!(
        msg.to_ascii_lowercase().contains("home"),
        "error must say path must stay under home, got {msg}"
    );
}

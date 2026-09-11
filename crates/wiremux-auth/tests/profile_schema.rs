//! Corpus tests for profile schema v1, refuse scanners, and id-keyed catalog load.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wiremux_auth::{
    LoadOptions, ProfileError, ResolvedProfile, load_profile, load_profile_from_cli,
};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/profiles")
}

fn empty_opts() -> LoadOptions<'static> {
    LoadOptions {
        id: None,
        explicit_file: None,
        extra_profile_dirs: Vec::new(),
        include_shipped: false,
        include_user_config: false,
    }
}

fn parse_via_file(name: &str, body: &str) -> Result<ResolvedProfile, ProfileError> {
    let dir = std::env::temp_dir().join(format!(
        "wiremux-auth-pr2-{}-{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    fs::write(&path, body).expect("write fixture");
    load_profile_from_cli(path.to_str().expect("utf-8 path"), &empty_opts())
}

fn load_id(id: &str, dir: &Path) -> Result<ResolvedProfile, ProfileError> {
    let opts = LoadOptions {
        id: None,
        explicit_file: None,
        extra_profile_dirs: vec![dir.to_path_buf()],
        include_shipped: false,
        include_user_config: false,
    };
    load_profile(id, &opts)
}

#[test]
fn exact_anthropic_oauth_toml_parses() {
    let path = fixtures_dir().join("anthropic-oauth.toml");
    let text = fs::read_to_string(&path)
        .expect("read anthropic-oauth.toml")
        .replace("\r\n", "\n");
    assert!(
        text.starts_with("# presets/anthropic-oauth.toml\n"),
        "fixture must be the design's complete shipped example"
    );
    assert!(text.contains("id = \"anthropic-oauth\""));
    assert!(text.contains("setup_token_hint = \"run `claude setup-token`"));

    let path_str = path.to_str().expect("utf-8 path");
    let profile = load_profile_from_cli(path_str, &empty_opts())
        .expect("exact anthropic-oauth.toml text should parse");
    assert_eq!(profile.schema_version, 1);
    assert_eq!(profile.id, "anthropic-oauth");
    assert_eq!(
        profile.display_name.as_deref(),
        Some("Anthropic OAuth (first-party host)")
    );
    assert_eq!(
        profile.oauth.as_ref().map(|o| o.token_url.as_str()),
        Some("https://platform.claude.com/v1/oauth/token")
    );
    assert_eq!(
        profile.betas.values,
        [
            "oauth-2025-04-20",
            "prompt-caching-2024-07-31",
            "extended-cache-ttl-2025-04-11"
        ]
    );
    let hint = profile
        .oauth
        .as_ref()
        .and_then(|o| o.setup_token_hint.as_deref())
        .expect("setup_token_hint");
    assert!(
        hint.contains('`'),
        "hint fields may contain backticks: {hint}"
    );
}

#[test]
fn localhost_11434_accepted_as_base_url() {
    let profile = parse_via_file(
        "localhost.toml",
        r#"
schema_version = 1
id = "localhost-ollama"
wire = "chat-completions"
base_url = "http://localhost:11434"
chat_path = "/v1/chat/completions"
auth_scheme = "none"
"#,
    )
    .expect("http://localhost:11434 must be accepted");
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("http://localhost:11434")
    );
    assert_eq!(
        profile.http.auth_scheme,
        Some(wiremux_auth::AuthScheme::None)
    );
}

#[test]
fn non_loopback_http_is_refused() {
    let err = parse_via_file(
        "non-loopback.toml",
        r#"
schema_version = 1
id = "doc-example"
base_url = "http://192.0.2.1"
"#,
    )
    .expect_err("http://192.0.2.1 must be refused");
    assert!(
        matches!(err, ProfileError::DisallowedUrl { .. }),
        "expected disallowed URL, got {err}"
    );
}

#[test]
fn load_openai_codex_does_not_inherit_anthropic_fields() {
    let dir = fixtures_dir();
    let openai = load_id("openai-codex-oauth", &dir)
        .expect("openai-codex-oauth should load from the fixture dir");
    assert_eq!(openai.id, "openai-codex-oauth");
    let token_url = openai
        .oauth
        .as_ref()
        .map(|o| o.token_url.as_str())
        .expect("openai profile has oauth.token_url");
    assert_eq!(token_url, "https://auth.openai.com/oauth/token");
    assert_ne!(token_url, "https://platform.claude.com/v1/oauth/token");
    assert!(
        openai.betas.values.is_empty(),
        "openai-codex-oauth must not inherit Anthropic betas"
    );

    let anthropic =
        load_id("anthropic-oauth", &dir).expect("anthropic-oauth should load from the same dir");
    assert_eq!(
        anthropic.oauth.as_ref().map(|o| o.token_url.as_str()),
        Some("https://platform.claude.com/v1/oauth/token")
    );
    assert!(!anthropic.betas.values.is_empty());
}

#[test]
fn missing_env_var_leaves_field_unset() {
    let var = "WIREMUX_TEST_UNSET_VAR_9f3c2e1a";
    assert!(
        std::env::var(var).is_err(),
        "test requires {var} to be unset"
    );
    let profile = parse_via_file(
        "env-missing.toml",
        &format!(
            r#"
schema_version = 1
id = "env-missing"
base_url = "{{env:{var}}}"
chat_path = "${var}"
"#
        ),
    )
    .expect("missing env vars should unset fields, not fail parse");
    assert_eq!(
        profile.http.base_url, None,
        "missing env must not become empty string"
    );
    assert_ne!(profile.http.base_url.as_deref(), Some(""));
    assert_eq!(
        profile.http.chat_path, None,
        "missing $VAR must unset the field"
    );
}

#[test]
fn refuse_forbidden_key_interpolation_and_javascript_url() {
    let key_err = parse_via_file(
        "forbidden-key.toml",
        r#"
schema_version = 1
id = "bad-key"
[oauth]
token_url = "https://auth.example.invalid/token"
fn = "evil"
"#,
    )
    .expect_err("forbidden key name must refuse");
    assert!(
        matches!(key_err, ProfileError::ForbiddenKey(ref name) if name == "fn"),
        "expected forbidden key fn, got {key_err}"
    );

    let interp_err = parse_via_file(
        "interpolation.toml",
        r#"
schema_version = 1
id = "bad-interp"
base_url = "!command curl https://evil.example"
"#,
    )
    .expect_err("!command interpolation must refuse");
    assert!(
        matches!(interp_err, ProfileError::Interpolation { .. }),
        "expected interpolation refuse, got {interp_err}"
    );

    let js_err = parse_via_file(
        "javascript-url.toml",
        r#"
schema_version = 1
id = "bad-js"
base_url = "javascript:alert(1)"
"#,
    )
    .expect_err("javascript: URL must refuse");
    assert!(
        matches!(js_err, ProfileError::DisallowedUrl { .. }),
        "expected disallowed URL, got {js_err}"
    );
}

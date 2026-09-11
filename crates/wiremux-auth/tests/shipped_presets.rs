//! Shipped catalog: `load_profile(id)` with `include_shipped`.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

use wiremux_auth::{
    AuthScheme, IsolatedHome, LoadOptions, Login, PlantCredentials, ProfileError, TokenProvider,
    TokenRequestFormat, ToolTypePolicy, Wire, list_profiles, load_profile, provider_from_profile,
};

fn presets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../presets")
}

fn shipped_opts() -> LoadOptions<'static> {
    LoadOptions {
        id: None,
        explicit_file: None,
        extra_profile_dirs: Vec::new(),
        include_shipped: true,
    }
}

fn no_shipped_opts() -> LoadOptions<'static> {
    LoadOptions {
        include_shipped: false,
        ..shipped_opts()
    }
}

#[test]
fn load_profile_anthropic_oauth_from_shipped_catalog() {
    let profile = load_profile("anthropic-oauth", &shipped_opts())
        .expect("include_shipped must expose anthropic-oauth");
    assert_eq!(profile.id, "anthropic-oauth");
    assert_eq!(profile.schema_version, 1);
    assert_eq!(
        profile.display_name.as_deref(),
        Some("Anthropic OAuth (first-party host)")
    );
    assert_eq!(profile.dialect.wire, Some(Wire::Messages));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://api.anthropic.com")
    );
    assert_eq!(profile.http.auth_scheme, Some(AuthScheme::Bearer));
    let oauth = profile.oauth.as_ref().expect("anthropic pack");
    assert_eq!(
        oauth.token_url,
        "https://platform.claude.com/v1/oauth/token"
    );
    assert_eq!(
        oauth.token_url_fallback.as_deref(),
        Some("https://console.anthropic.com/v1/oauth/token")
    );
    assert_eq!(
        oauth.client_id.as_deref(),
        Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e")
    );
    assert_eq!(oauth.token_request_format, Some(TokenRequestFormat::Json));
    assert_eq!(oauth.login, Some(Login::SetupToken));
    assert_eq!(oauth.access_env.as_deref(), Some("CLAUDE_CODE_OAUTH_TOKEN"));
    assert_eq!(
        profile.betas.values,
        [
            "oauth-2025-04-20",
            "prompt-caching-2024-07-31",
            "extended-cache-ttl-2025-04-11"
        ]
    );
}

#[test]
fn load_profile_openai_codex_oauth_from_shipped_catalog() {
    let profile = load_profile("openai-codex-oauth", &shipped_opts())
        .expect("include_shipped must expose openai-codex-oauth");
    assert_eq!(profile.id, "openai-codex-oauth");
    assert_eq!(profile.dialect.wire, Some(Wire::Responses));
    let oauth = profile.oauth.as_ref().expect("openai pack");
    assert_eq!(oauth.token_url, "https://auth.openai.com/oauth/token");
    assert_eq!(
        oauth.authorize_url.as_deref(),
        Some("https://auth.openai.com/oauth/authorize")
    );
    assert_eq!(
        oauth.device_auth_url.as_deref(),
        Some("https://auth.openai.com/oauth/device/code")
    );
    assert_eq!(oauth.client_id.as_deref(), Some(""));
    assert_eq!(oauth.login, Some(Login::None));
    assert_eq!(oauth.token_request_format, Some(TokenRequestFormat::Form));
    assert!(
        profile.betas.values.is_empty(),
        "openai-codex-oauth must not inherit Anthropic betas, got {:?}",
        profile.betas.values
    );
    assert_ne!(
        oauth.token_url, "https://platform.claude.com/v1/oauth/token",
        "openai-codex-oauth must not inherit Anthropic token_url"
    );
}

#[test]
fn load_profile_openrouter_codex_from_shipped_catalog() {
    let profile = load_profile("openrouter-codex", &shipped_opts())
        .expect("include_shipped must expose openrouter-codex");
    assert_eq!(profile.id, "openrouter-codex");
    assert_eq!(profile.dialect.wire, Some(Wire::Responses));
    assert_eq!(
        profile.dialect.tool_type_policy,
        ToolTypePolicy::FlattenNamespace
    );
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://openrouter.ai/api")
    );
    assert!(profile.oauth.is_none(), "openrouter-codex is API-key only");
    let fingerprint = profile.fingerprint.as_ref().expect("fingerprint");
    assert_eq!(fingerprint.forbidden_body_fields, ["store"]);
    let text = fs::read_to_string(presets_dir().join("openrouter-codex.toml")).expect("read");
    assert!(text.contains("{env:OPENROUTER_API_KEY}"));
    assert!(text.contains("flatten-namespace"));
}

#[test]
fn load_profile_grok_ollama_from_shipped_catalog() {
    let profile = load_profile("grok-ollama", &shipped_opts())
        .expect("include_shipped must expose grok-ollama");
    assert_eq!(profile.id, "grok-ollama");
    assert_eq!(profile.dialect.wire, Some(Wire::ChatCompletions));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("http://localhost:11434")
    );
    assert_eq!(profile.http.auth_scheme, Some(AuthScheme::None));
    assert!(profile.oauth.is_none());
}

#[test]
fn include_shipped_false_hides_catalog() {
    for id in [
        "anthropic-oauth",
        "openai-codex-oauth",
        "openrouter-codex",
        "grok-ollama",
    ] {
        let err = load_profile(id, &no_shipped_opts()).expect_err(id);
        assert!(
            matches!(err, ProfileError::NotFound(ref found) if found == id),
            "{id}: {err}"
        );
    }
}

#[test]
fn list_profiles_includes_shipped_ids() {
    let ids = list_profiles(&shipped_opts()).expect("list shipped");
    for id in [
        "anthropic-oauth",
        "openai-codex-oauth",
        "openrouter-codex",
        "grok-ollama",
    ] {
        assert!(ids.iter().any(|got| got == id), "missing {id} in {ids:?}");
    }
}

#[test]
fn no_claude_pro_in_codex_preset() {
    let dir = presets_dir();
    assert!(
        !dir.join("claude-pro-in-codex.toml").exists(),
        "must not ship a Claude-Pro-in-Codex file"
    );
    let names: Vec<String> = fs::read_dir(&dir)
        .expect("presets dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    for name in &names {
        let lower = name.to_ascii_lowercase();
        assert!(
            !lower.contains("claude-pro")
                && !lower.contains("cline")
                && !lower.contains("opencode"),
            "forbidden preset name: {name}"
        );
    }
    let ids = list_profiles(&shipped_opts()).expect("list");
    for id in &ids {
        let lower = id.to_ascii_lowercase();
        assert!(
            !lower.contains("claude-pro")
                && !lower.contains("cline")
                && !lower.contains("opencode"),
            "forbidden shipped id: {id}"
        );
    }
}

fn spawn_http_server(status: u16, body: &str) -> (String, std::thread::JoinHandle<String>) {
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

#[tokio::test]
async fn shipped_anthropic_oauth_refreshes_via_provider_from_profile() {
    let home = IsolatedHome::new();
    let path = home.plant_credentials(PlantCredentials::Claude {
        access: "sk-ant-oat01-old",
        refresh: Some("rt-old"),
        expires_at_ms: Some(1),
    });
    let (url, handle) = spawn_http_server(
        200,
        r#"{"access_token":"sk-ant-oat01-refreshed","refresh_token":"rt-new","expires_in":3600}"#,
    );

    let mut profile =
        load_profile("anthropic-oauth", &shipped_opts()).expect("load shipped anthropic-oauth");
    {
        let oauth = profile.oauth.as_mut().expect("oauth pack");
        oauth.token_url = url;
        oauth.token_url_fallback = None;
    }

    let provider = provider_from_profile(&profile).expect("load_profile → provider_from_profile");
    let token = provider.get_token().await.expect("refresh");
    assert_eq!(token, "sk-ant-oat01-refreshed");
    let req = handle.join().expect("server");
    assert!(
        req.contains("\"grant_type\":\"refresh_token\""),
        "Anthropic pack is JSON grant, got: {req}"
    );
    assert!(
        req.contains("\"client_id\":\"9d1c250a-e61b-44d9-88ed-5944d1962f5e\""),
        "shipped client_id, got: {req}"
    );
    assert!(
        req.contains("\"refresh_token\":\"rt-old\""),
        "engine injects store refresh, got: {req}"
    );
    let written = fs::read_to_string(&path).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&written).unwrap();
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

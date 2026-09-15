//! Shipped catalog: `load_profile(id)` with `include_shipped`.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

use wiremux_auth::{
    AuthScheme, IsolatedHome, LoadOptions, Login, PlantCredentials, ProfileError, TokenProvider,
    TokenRequestFormat, ToolTypePolicy, Wire, list_profiles, load_profile, provider_from_profile,
    token_for_profile,
};

fn presets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets")
}

fn workspace_presets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../presets")
}

#[test]
fn crate_presets_match_workspace_and_stay_inside_package() {
    let crate_dir = presets_dir();
    let workspace_dir = workspace_presets_dir();
    let shipped_src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/profile/shipped.rs"
    ));
    assert!(
        !shipped_src.contains("/../../presets/"),
        "include_str! must not escape the package root"
    );
    let mut crate_names: Vec<String> = fs::read_dir(&crate_dir)
        .expect("crate presets")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".toml"))
        .collect();
    let mut workspace_names: Vec<String> = fs::read_dir(&workspace_dir)
        .expect("workspace presets")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".toml"))
        .collect();
    crate_names.sort();
    workspace_names.sort();
    assert_eq!(crate_names, workspace_names, "preset file set drifted");
    for name in &crate_names {
        let crate_bytes = fs::read(crate_dir.join(name)).expect(name);
        let workspace_bytes = fs::read(workspace_dir.join(name)).expect(name);
        assert_eq!(crate_bytes, workspace_bytes, "{name} drifted");
    }
}

fn shipped_opts() -> LoadOptions<'static> {
    LoadOptions {
        id: None,
        explicit_file: None,
        extra_profile_dirs: Vec::new(),
        include_shipped: true,
        include_user_config: false,
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
    let _home = IsolatedHome::new();
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
    let _home = IsolatedHome::new();
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
    let _home = IsolatedHome::new();
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
    let _home = IsolatedHome::new();
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

const KEY_PROFILE_IDS: &[&str] = &[
    "xai",
    "openai",
    "anthropic",
    "openrouter",
    "gemini",
    "lmstudio",
    "vllm",
];

const ALL_SHIPPED_IDS: &[&str] = &[
    "anthropic-oauth",
    "openai-codex-oauth",
    "openrouter-codex",
    "grok-ollama",
    "xai-oauth",
    "xai-grok-build",
    "xai",
    "openai",
    "anthropic",
    "openrouter",
    "gemini",
    "lmstudio",
    "vllm",
];

#[test]
fn load_profile_xai_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile = load_profile("xai", &shipped_opts()).expect("include_shipped must expose xai");
    assert_eq!(profile.id, "xai");
    assert_eq!(profile.dialect.wire, Some(Wire::ChatCompletions));
    assert_eq!(profile.http.base_url.as_deref(), Some("https://api.x.ai"));
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions")
    );
    assert_eq!(profile.http.auth_scheme, Some(AuthScheme::Bearer));
    assert_eq!(profile.access_env, ["XAI_API_KEY", "GROK_API_KEY"]);
    assert!(profile.oauth.is_none(), "xai is API-key only");
}

#[test]
fn load_profile_xai_oauth_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile =
        load_profile("xai-oauth", &shipped_opts()).expect("include_shipped must expose xai-oauth");
    assert_eq!(profile.id, "xai-oauth");
    assert_eq!(profile.dialect.wire, Some(Wire::ChatCompletions));
    assert_eq!(profile.http.base_url.as_deref(), Some("https://api.x.ai"));
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions")
    );
    assert_eq!(profile.http.auth_scheme, Some(AuthScheme::Bearer));
    let oauth = profile.oauth.as_ref().expect("xai-oauth has [oauth]");
    assert_eq!(
        oauth.creds_format,
        Some(wiremux_auth::CredsFormat::OidcAuthJson)
    );
    assert_eq!(oauth.creds_path.as_deref(), Some("~/.grok/auth.json"));
    assert_eq!(oauth.token_url.as_str(), "https://auth.x.ai/oauth2/token");
    let client = oauth.client_id.as_deref().map(str::trim).unwrap_or("");
    assert!(
        client.is_empty(),
        "must not ship a product client id, got {client}"
    );
    assert_eq!(oauth.login, Some(Login::None));
}

#[test]
fn load_profile_xai_grok_build_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile = load_profile("xai-grok-build", &shipped_opts())
        .expect("include_shipped must expose xai-grok-build");
    assert_eq!(profile.id, "xai-grok-build");
    assert_eq!(profile.dialect.wire, Some(Wire::ChatCompletions));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://cli-chat-proxy.grok.com")
    );
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions")
    );
    assert_eq!(profile.http.auth_scheme, Some(AuthScheme::Bearer));
    let oauth = profile.oauth.as_ref().expect("xai-grok-build has [oauth]");
    assert_eq!(
        oauth.creds_format,
        Some(wiremux_auth::CredsFormat::OidcAuthJson)
    );
    assert_eq!(oauth.creds_path.as_deref(), Some("~/.grok/auth.json"));
    assert_eq!(oauth.token_url.as_str(), "https://auth.x.ai/oauth2/token");
    let client = oauth.client_id.as_deref().map(str::trim).unwrap_or("");
    assert!(
        client.is_empty(),
        "must not ship a product client id, got {client}"
    );
    assert_eq!(oauth.login, Some(Login::None));
}

#[test]
fn load_profile_xai_grok_build_reuses_xai_oauth_pack() {
    let _home = IsolatedHome::new();
    let grok = load_profile("xai-grok-build", &shipped_opts()).expect("xai-grok-build");
    let xai = load_profile("xai-oauth", &shipped_opts()).expect("xai-oauth");
    assert_eq!(
        grok.http.base_url.as_deref(),
        Some("https://cli-chat-proxy.grok.com")
    );
    assert_eq!(xai.http.base_url.as_deref(), Some("https://api.x.ai"));
    assert_eq!(grok.http.chat_path, xai.http.chat_path);
    assert_eq!(
        grok.oauth, xai.oauth,
        "same empty-client oidc-auth-json pack"
    );
}

#[tokio::test]
async fn token_for_profile_xai_oauth_reads_grok_auth_json() {
    let home = IsolatedHome::new();
    home.plant_credentials(PlantCredentials::JsonPointer {
        relative_path: ".grok/auth.json",
        document: serde_json::json!({
            "https://auth.x.ai::planted-test-client": {
                "key": "grok-file-access",
                "refresh_token": "grok-file-rt",
                "expires_at": "2099-01-01T00:00:00Z"
            }
        }),
    });
    let token = token_for_profile("xai-oauth")
        .await
        .expect("xai-oauth token");
    assert_eq!(token, "grok-file-access");
    let _ = home;
}

#[tokio::test]
async fn token_for_profile_xai_grok_build_reads_grok_auth_json() {
    let home = IsolatedHome::new();
    home.plant_credentials(PlantCredentials::JsonPointer {
        relative_path: ".grok/auth.json",
        document: serde_json::json!({
            "https://auth.x.ai::planted-test-client": {
                "key": "grok-build-file-access",
                "refresh_token": "grok-build-file-rt",
                "expires_at": "2099-01-01T00:00:00Z"
            }
        }),
    });
    let token = token_for_profile("xai-grok-build")
        .await
        .expect("xai-grok-build token");
    assert_eq!(token, "grok-build-file-access");
    let _ = home;
}

#[test]
fn load_profile_openai_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile =
        load_profile("openai", &shipped_opts()).expect("include_shipped must expose openai");
    assert_eq!(profile.id, "openai");
    assert_eq!(profile.dialect.wire, Some(Wire::ChatCompletions));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://api.openai.com")
    );
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions")
    );
    assert_eq!(profile.access_env, ["OPENAI_API_KEY"]);
    assert!(profile.oauth.is_none());
}

#[test]
fn load_profile_anthropic_key_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile =
        load_profile("anthropic", &shipped_opts()).expect("include_shipped must expose anthropic");
    assert_eq!(profile.id, "anthropic");
    assert_eq!(profile.dialect.wire, Some(Wire::Messages));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://api.anthropic.com")
    );
    assert_eq!(profile.http.chat_path.as_deref(), Some("/v1/messages"));
    assert_eq!(
        profile
            .http
            .headers
            .get("anthropic-version")
            .map(String::as_str),
        Some("2023-06-01")
    );
    assert!(
        !profile.betas.values.iter().any(|v| v == "oauth-2025-04-20"),
        "key profile must not ship oauth-2025-04-20, got {:?}",
        profile.betas.values
    );
    assert_eq!(
        profile.access_env,
        ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"]
    );
    assert!(profile.oauth.is_none(), "anthropic is API-key only");
}

#[test]
fn load_profile_openrouter_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile = load_profile("openrouter", &shipped_opts())
        .expect("include_shipped must expose openrouter");
    assert_eq!(profile.id, "openrouter");
    assert_eq!(profile.dialect.wire, Some(Wire::ChatCompletions));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://openrouter.ai/api")
    );
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions")
    );
    assert_eq!(profile.access_env, ["OPENROUTER_API_KEY"]);
    assert!(profile.oauth.is_none());
    assert!(
        profile
            .fingerprint
            .as_ref()
            .map(|f| f.forbidden_body_fields.is_empty())
            .unwrap_or(true),
        "openrouter Chat Completions must not forbid store"
    );
}

#[test]
fn load_profile_gemini_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile =
        load_profile("gemini", &shipped_opts()).expect("include_shipped must expose gemini");
    assert_eq!(profile.id, "gemini");
    assert_eq!(profile.dialect.wire, Some(Wire::Gemini));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://generativelanguage.googleapis.com")
    );
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some(Wire::Gemini.default_chat_path())
    );
    assert!(
        profile
            .http
            .chat_path
            .as_deref()
            .is_some_and(|p| p.contains("{model}")),
        "gemini must keep the default {{model}} path, got {:?}",
        profile.http.chat_path
    );
    assert_eq!(profile.access_env, ["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
    assert_eq!(
        profile.http.auth_scheme,
        Some(AuthScheme::Header("x-goog-api-key".into()))
    );
    assert!(profile.oauth.is_none());
}

#[test]
fn load_profile_lmstudio_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile =
        load_profile("lmstudio", &shipped_opts()).expect("include_shipped must expose lmstudio");
    assert_eq!(profile.id, "lmstudio");
    assert_eq!(profile.dialect.wire, Some(Wire::ChatCompletions));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("http://127.0.0.1:1234")
    );
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions")
    );
    assert_eq!(profile.http.auth_scheme, Some(AuthScheme::None));
    assert!(profile.oauth.is_none());
    assert!(profile.access_env.is_empty());
}

#[test]
fn load_profile_vllm_from_shipped_catalog() {
    let _home = IsolatedHome::new();
    let profile = load_profile("vllm", &shipped_opts()).expect("include_shipped must expose vllm");
    assert_eq!(profile.id, "vllm");
    assert_eq!(profile.dialect.wire, Some(Wire::ChatCompletions));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("http://127.0.0.1:8000")
    );
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions")
    );
    assert_eq!(profile.http.auth_scheme, Some(AuthScheme::None));
    assert!(profile.oauth.is_none());
}

#[tokio::test]
async fn token_for_profile_xai_reads_xai_api_key() {
    let home = IsolatedHome::new();
    home.set_env("XAI_API_KEY", "xai-shipped-key");
    let token = token_for_profile("xai").await.expect("xai token");
    assert_eq!(token, "xai-shipped-key");
    let _ = home;
}

#[tokio::test]
async fn token_for_profile_anthropic_prefers_auth_token() {
    let home = IsolatedHome::new();
    home.set_env("ANTHROPIC_AUTH_TOKEN", "auth-token-wins");
    home.set_env("ANTHROPIC_API_KEY", "api-key-loses");
    let token = token_for_profile("anthropic")
        .await
        .expect("anthropic token");
    assert_eq!(token, "auth-token-wins");
    let _ = home;
}

#[tokio::test]
async fn token_for_profile_anthropic_oauth_reads_login_keychain_account() {
    let home = IsolatedHome::new();
    let login = ["USER", "USERNAME"].into_iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|s| !s.is_empty())
    });
    let login = login.expect("USER or USERNAME must be set to plant the live login account");
    home.plant_keychain(
        "Claude Code-credentials",
        &login,
        &serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-login-acct",
                "refreshToken": "rt",
                "expiresAt": 4_102_444_800_000_i64
            }
        }),
    );
    let token = token_for_profile("anthropic-oauth")
        .await
        .expect("login keychain account");
    assert_eq!(token, "sk-ant-oat01-login-acct");
    let _ = home;
}

#[test]
fn anthropic_oauth_shipped_toml_keeps_static_keychain_accounts() {
    let toml = fs::read_to_string(presets_dir().join("anthropic-oauth.toml"))
        .expect("crate anthropic-oauth.toml");
    assert!(
        toml.contains(r#"keychain_accounts = ["Claude Code", "credentials"]"#),
        "shipped keychain_accounts must stay Claude Code and credentials"
    );
    assert!(
        !toml.contains("$USER"),
        "literal $USER is not a keychain account"
    );
    for key in ["USER", "USERNAME"] {
        if let Ok(v) = std::env::var(key) {
            let t = v.trim();
            if !t.is_empty() && t != "Claude Code" && t != "credentials" {
                assert!(
                    !toml.contains(t),
                    "shipped TOML must not contain machine username {t:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn token_for_profile_lmstudio_empty_static() {
    let _home = IsolatedHome::new();
    let token = token_for_profile("lmstudio")
        .await
        .expect("lmstudio none auth");
    assert_eq!(token, "");
}

#[test]
fn isolated_home_clears_anthropic_auth_token() {
    let home = IsolatedHome::new();
    assert!(
        std::env::var("ANTHROPIC_AUTH_TOKEN").is_err(),
        "IsolatedHome must clear ANTHROPIC_AUTH_TOKEN"
    );
    let _ = home;
}

#[test]
fn include_shipped_false_hides_catalog() {
    let _home = IsolatedHome::new();
    for id in ALL_SHIPPED_IDS {
        let err = load_profile(id, &no_shipped_opts()).expect_err(id);
        assert!(
            matches!(err, ProfileError::NotFound { id: ref found, .. } if found == *id),
            "{id}: {err}"
        );
    }
}

#[test]
fn list_profiles_includes_shipped_ids() {
    let _home = IsolatedHome::new();
    let ids = list_profiles(&shipped_opts()).expect("list shipped");
    for id in ALL_SHIPPED_IDS {
        assert!(ids.iter().any(|got| got == *id), "missing {id} in {ids:?}");
    }
    for id in KEY_PROFILE_IDS {
        assert!(ids.iter().any(|got| got == *id), "missing key id {id}");
    }
}

#[test]
fn no_claude_pro_in_codex_preset() {
    let _home = IsolatedHome::new();
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

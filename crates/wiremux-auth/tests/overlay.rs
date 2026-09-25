//! Overlay merge (shipped < user dir < explicit) and gist restore tests.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wiremux_auth::{
    CredsFormat, IsolatedHome, ListMerge, LoadOptions, PlantCredentials, ProfileError,
    TokenProvider, default_user_profile_dir, load_profile, load_profile_from_cli,
    provider_from_profile,
};

fn gists_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/gists")
}

fn hermetic_opts() -> LoadOptions<'static> {
    LoadOptions {
        id: None,
        explicit_file: None,
        extra_profile_dirs: Vec::new(),
        include_shipped: false,
        include_user_config: false,
    }
}

fn write_toml(dir: &Path, name: &str, body: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("mkdir");
    let path = dir.join(name);
    fs::write(&path, body).expect("write toml");
    path
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

#[test]
fn merge_policy_union_headers_betas_and_lists() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_toml(
        dir.path(),
        "base.toml",
        r#"
schema_version = 1
id = "merge-demo"
display_name = "base"
header_merge = "union"
list_merge = "union"
stream_events = ["e1", "e2"]
[headers]
A = "1"
B = "2"
[betas]
values = ["b1", "b2"]
merge = "verbatim"
[oauth]
token_url = "https://auth.example.invalid/token"
client_id = "base-client"
scopes = ["openid", "profile"]
list_merge = "union"
"#,
    );
    let later = write_toml(
        dir.path(),
        "later.toml",
        r#"
schema_version = 1
id = "merge-demo"
header_merge = "union"
list_merge = "union"
stream_events = ["e2", "e3"]
[headers]
B = "over"
C = "3"
[betas]
values = ["b3"]
merge = "union"
[oauth]
scopes = ["offline_access"]
list_merge = "union"
"#,
    );

    let opts = LoadOptions {
        extra_profile_dirs: vec![dir.path().to_path_buf()],
        explicit_file: Some(&later),
        ..hermetic_opts()
    };
    let profile = load_profile("merge-demo", &opts).expect("union merge");
    assert_eq!(profile.display_name.as_deref(), Some("base"));
    assert_eq!(profile.http.headers.get("A").map(String::as_str), Some("1"));
    assert_eq!(
        profile.http.headers.get("B").map(String::as_str),
        Some("over")
    );
    assert_eq!(profile.http.headers.get("C").map(String::as_str), Some("3"));
    assert_eq!(profile.betas.values, ["b1", "b2", "b3"]);
    assert_eq!(profile.betas.merge, ListMerge::Union);
    assert_eq!(profile.dialect.stream_events, ["e1", "e2", "e3"]);
    let oauth = profile.oauth.as_ref().expect("oauth");
    assert_eq!(oauth.token_url, "https://auth.example.invalid/token");
    assert_eq!(oauth.client_id.as_deref(), Some("base-client"));
    assert_eq!(oauth.scopes, ["openid", "profile", "offline_access"]);
}

#[test]
fn merge_policy_replace_headers_betas_and_lists() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_toml(
        dir.path(),
        "base.toml",
        r#"
schema_version = 1
id = "merge-replace"
header_merge = "union"
list_merge = "union"
stream_events = ["e1", "e2"]
[headers]
A = "1"
B = "2"
[betas]
values = ["b1", "b2"]
merge = "union"
[oauth]
token_url = "https://auth.example.invalid/token"
scopes = ["openid", "profile"]
list_merge = "union"
"#,
    );
    let later = write_toml(
        dir.path(),
        "later.toml",
        r#"
schema_version = 1
id = "merge-replace"
header_merge = "replace"
list_merge = "replace"
stream_events = ["only-event"]
[headers]
B = "only"
[betas]
values = ["only-beta"]
merge = "verbatim"
[oauth]
scopes = ["offline_access"]
list_merge = "replace"
"#,
    );

    let opts = LoadOptions {
        extra_profile_dirs: vec![dir.path().to_path_buf()],
        explicit_file: Some(&later),
        ..hermetic_opts()
    };
    let profile = load_profile("merge-replace", &opts).expect("replace merge");
    assert_eq!(profile.http.headers.len(), 1);
    assert_eq!(
        profile.http.headers.get("B").map(String::as_str),
        Some("only")
    );
    assert_eq!(profile.betas.values, ["only-beta"]);
    assert_eq!(profile.betas.merge, ListMerge::Replace);
    assert_eq!(profile.dialect.stream_events, ["only-event"]);
    assert_eq!(
        profile.oauth.as_ref().map(|o| o.scopes.as_slice()),
        Some(["offline_access".to_string()].as_slice())
    );
    assert_eq!(
        profile.oauth.as_ref().map(|o| o.token_url.as_str()),
        Some("https://auth.example.invalid/token")
    );
}

#[test]
fn later_omitted_fields_keep_earlier() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_toml(
        dir.path(),
        "aa.toml",
        r#"
schema_version = 1
id = "keep-earlier"
display_name = "first"
base_url = "https://a.example.invalid"
[headers]
A = "1"
[oauth]
token_url = "https://auth.example.invalid/token"
client_id = "keep-me"
creds_format = "claude-credentials"
"#,
    );
    write_toml(
        dir.path(),
        "zz.toml",
        r#"
schema_version = 1
id = "keep-earlier"
display_name = "second"
"#,
    );

    let opts = LoadOptions {
        extra_profile_dirs: vec![dir.path().to_path_buf()],
        ..hermetic_opts()
    };
    let profile = load_profile("keep-earlier", &opts).expect("field-wise keep");
    assert_eq!(profile.display_name.as_deref(), Some("second"));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://a.example.invalid")
    );
    assert_eq!(profile.http.headers.get("A").map(String::as_str), Some("1"));
    let oauth = profile.oauth.as_ref().expect("oauth kept");
    assert_eq!(oauth.token_url, "https://auth.example.invalid/token");
    assert_eq!(oauth.client_id.as_deref(), Some("keep-me"));
    assert_eq!(oauth.creds_format, Some(CredsFormat::ClaudeCredentials));
}

#[test]
fn later_wire_change_drops_earlier_wire_specific_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_toml(
        dir.path(),
        "aa.toml",
        r#"
schema_version = 1
id = "wire-path"
wire = "messages"
messages_path = "/custom/messages"
chat_path = "/keep/chat"
"#,
    );
    write_toml(
        dir.path(),
        "zz.toml",
        r#"
schema_version = 1
id = "wire-path"
wire = "chat-completions"
"#,
    );
    let opts = LoadOptions {
        extra_profile_dirs: vec![dir.path().to_path_buf()],
        ..hermetic_opts()
    };
    let profile = load_profile("wire-path", &opts).expect("overlay");
    assert_eq!(
        profile.dialect.wire,
        Some(wiremux_auth::Wire::ChatCompletions)
    );
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/keep/chat"),
        "an explicit chat_path still overlays, got {:?}",
        profile.http.chat_path
    );

    write_toml(
        dir.path(),
        "aa.toml",
        r#"
schema_version = 1
id = "wire-path"
wire = "messages"
messages_path = "/custom/messages"
"#,
    );
    let profile = load_profile("wire-path", &opts).expect("overlay without chat_path");
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions"),
        "messages_path must not stick after wire changes, got {:?}",
        profile.http.chat_path
    );
}

#[test]
fn three_layer_wire_change_uses_final_wire_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_toml(
        dir.path(),
        "aa.toml",
        r#"
schema_version = 1
id = "wire-path-3"
wire = "messages"
messages_path = "/custom/messages"
"#,
    );
    write_toml(
        dir.path(),
        "mm.toml",
        r#"
schema_version = 1
id = "wire-path-3"
display_name = "middle"
"#,
    );
    write_toml(
        dir.path(),
        "zz.toml",
        r#"
schema_version = 1
id = "wire-path-3"
wire = "chat-completions"
"#,
    );
    let opts = LoadOptions {
        extra_profile_dirs: vec![dir.path().to_path_buf()],
        ..hermetic_opts()
    };
    let profile = load_profile("wire-path-3", &opts).expect("three layers");
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/v1/chat/completions"),
        "a middle layer must not bake messages_path into chat_path, got {:?}",
        profile.http.chat_path
    );

    write_toml(
        dir.path(),
        "aa.toml",
        r#"
schema_version = 1
id = "wire-path-3"
messages_path = "/custom/messages"
"#,
    );
    write_toml(
        dir.path(),
        "zz.toml",
        r#"
schema_version = 1
id = "wire-path-3"
wire = "messages"
"#,
    );
    let profile = load_profile("wire-path-3", &opts).expect("wire arrives last");
    assert_eq!(
        profile.http.chat_path.as_deref(),
        Some("/custom/messages"),
        "messages_path must apply once the final wire is messages, got {:?}",
        profile.http.chat_path
    );
}

#[test]
fn schema_version_is_max_of_layers() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_toml(
        dir.path(),
        "old.toml",
        r#"
schema_version = 0
id = "schema-max"
base_url = "https://a.example.invalid"
"#,
    );
    write_toml(
        dir.path(),
        "new.toml",
        r#"
schema_version = 1
id = "schema-max"
display_name = "v1"
"#,
    );
    let opts = LoadOptions {
        extra_profile_dirs: vec![dir.path().to_path_buf()],
        ..hermetic_opts()
    };
    let profile = load_profile("schema-max", &opts).expect("schema max");
    assert_eq!(profile.schema_version, 1);
    assert_eq!(profile.display_name.as_deref(), Some("v1"));
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://a.example.invalid")
    );
}

#[test]
fn load_openai_codex_does_not_inherit_anthropic_from_shipped() {
    let _home = IsolatedHome::new();
    let profile = load_profile(
        "openai-codex-oauth",
        &LoadOptions {
            include_shipped: true,
            include_user_config: false,
            ..hermetic_opts()
        },
    )
    .expect("openai shipped");
    let token_url = profile
        .oauth
        .as_ref()
        .map(|o| o.token_url.as_str())
        .expect("token_url");
    assert_eq!(token_url, "https://auth.openai.com/oauth/token");
    assert_ne!(token_url, "https://platform.claude.com/v1/oauth/token");
    assert!(
        profile.betas.values.is_empty(),
        "openai-codex-oauth must not inherit Anthropic betas, got {:?}",
        profile.betas.values
    );
}

#[test]
fn different_ids_in_user_dir_never_merge() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_toml(
        dir.path(),
        "anthropic-oauth.toml",
        r#"
schema_version = 1
id = "anthropic-oauth"
[oauth]
token_url = "https://platform.claude.com/v1/oauth/token"
[betas]
values = ["oauth-2025-04-20"]
"#,
    );
    write_toml(
        dir.path(),
        "openai-codex-oauth.toml",
        r#"
schema_version = 1
id = "openai-codex-oauth"
[oauth]
token_url = "https://auth.openai.com/oauth/token"
client_id = ""
"#,
    );
    let opts = LoadOptions {
        extra_profile_dirs: vec![dir.path().to_path_buf()],
        ..hermetic_opts()
    };
    let openai = load_profile("openai-codex-oauth", &opts).expect("openai");
    assert_eq!(
        openai.oauth.as_ref().map(|o| o.token_url.as_str()),
        Some("https://auth.openai.com/oauth/token")
    );
    assert!(openai.betas.values.is_empty());
}

#[test]
fn partial_overlay_rotates_oauth_fields_and_keeps_shipped() {
    let _home = IsolatedHome::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let overlay = write_toml(
        dir.path(),
        "rotate.toml",
        r#"
schema_version = 1
id = "anthropic-oauth"
[oauth]
token_url = "https://platform.claude.com/v1/oauth/token-rotated"
client_id = "rotated-anthropic-client-id"
[betas]
values = ["oauth-2026-09-01"]
merge = "verbatim"
"#,
    );
    let opts = LoadOptions {
        include_shipped: true,
        include_user_config: false,
        explicit_file: Some(&overlay),
        extra_profile_dirs: Vec::new(),
        id: None,
    };
    let profile = load_profile("anthropic-oauth", &opts).expect("overlay shipped");
    let oauth = profile.oauth.as_ref().expect("oauth");
    assert_eq!(
        oauth.token_url,
        "https://platform.claude.com/v1/oauth/token-rotated"
    );
    assert_eq!(
        oauth.client_id.as_deref(),
        Some("rotated-anthropic-client-id")
    );
    assert_eq!(oauth.creds_format, Some(CredsFormat::ClaudeCredentials));
    assert_eq!(
        oauth.creds_path.as_deref(),
        Some("~/.claude/.credentials.json")
    );
    assert_eq!(profile.betas.values, ["oauth-2026-09-01"]);
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://api.anthropic.com")
    );
}

#[test]
fn wiremux_no_shipped_presets_hides_catalog() {
    let home = IsolatedHome::new();
    home.set_env("WIREMUX_NO_SHIPPED_PRESETS", "1");
    let err = load_profile(
        "anthropic-oauth",
        &LoadOptions {
            include_shipped: true,
            include_user_config: false,
            ..hermetic_opts()
        },
    )
    .expect_err("env must skip shipped");
    assert!(
        matches!(err, ProfileError::NotFound { ref id, .. } if id == "anthropic-oauth"),
        "{err}"
    );
    let _ = home;
}

#[test]
fn gist_restores_yanked_anthropic_with_no_shipped_presets() {
    let home = IsolatedHome::new();
    home.set_env("WIREMUX_NO_SHIPPED_PRESETS", "1");
    let dest = home
        .path()
        .join(".config/wiremux/profiles/anthropic-oauth.toml");
    fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir profiles");
    fs::copy(gists_dir().join("restore-anthropic-oauth.toml"), &dest).expect("copy gist");

    let profile = load_profile(
        "anthropic-oauth",
        &LoadOptions {
            include_shipped: true,
            include_user_config: true,
            ..hermetic_opts()
        },
    )
    .expect("yanked restore from user dir");
    assert_eq!(profile.id, "anthropic-oauth");
    let oauth = profile.oauth.as_ref().expect("oauth");
    assert_eq!(
        oauth.token_url,
        "https://platform.claude.com/v1/oauth/token-rotated"
    );
    assert_eq!(
        oauth.client_id.as_deref(),
        Some("rotated-anthropic-client-id")
    );
    assert_eq!(profile.betas.values[0], "oauth-2026-09-01");
    assert_ne!(
        oauth.token_url, "https://platform.claude.com/v1/oauth/token",
        "must not fall back to yanked shipped URL"
    );
}

#[tokio::test]
async fn gist_restores_yanked_anthropic_refresh() {
    let home = IsolatedHome::new();
    home.set_env("WIREMUX_NO_SHIPPED_PRESETS", "1");
    home.plant_credentials(PlantCredentials::Claude {
        access: "sk-ant-oat01-old",
        refresh: Some("rt-old"),
        expires_at_ms: Some(1),
    });
    let dest = home
        .path()
        .join(".config/wiremux/profiles/anthropic-oauth.toml");
    fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir profiles");
    fs::copy(gists_dir().join("restore-anthropic-oauth.toml"), &dest).expect("copy gist");

    let (url, handle) = spawn_http_server(
        200,
        r#"{"access_token":"sk-ant-oat01-gist","refresh_token":"rt-new","expires_in":3600}"#,
    );
    let mut profile = load_profile(
        "anthropic-oauth",
        &LoadOptions {
            include_shipped: true,
            include_user_config: true,
            ..hermetic_opts()
        },
    )
    .expect("load restored gist");
    {
        let oauth = profile.oauth.as_mut().expect("oauth");
        oauth.token_url = url;
        oauth.token_url_fallback = None;
    }
    let provider = provider_from_profile(&profile).expect("provider");
    let token = provider.get_token().await.expect("refresh");
    assert_eq!(token, "sk-ant-oat01-gist");
    let req = handle.join().expect("server");
    assert!(
        req.contains("\"client_id\":\"rotated-anthropic-client-id\""),
        "rotated client id, got: {req}"
    );
}

#[tokio::test]
async fn other_vendor_gist_json_pointer_token_headers_refresh() {
    let home = IsolatedHome::new();
    let creds = home.plant_credentials(PlantCredentials::JsonPointer {
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
    let dest = home
        .path()
        .join(".config/wiremux/profiles/restore-other-vendor.toml");
    fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir profiles");
    fs::copy(gists_dir().join("restore-other-vendor.toml"), &dest).expect("copy gist");

    let (url, handle) = spawn_http_server(
        200,
        r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
    );
    let mut profile = load_profile(
        "other-vendor-oauth",
        &LoadOptions {
            include_shipped: true,
            include_user_config: true,
            ..hermetic_opts()
        },
    )
    .expect("load other-vendor gist by document id");
    assert_eq!(profile.id, "other-vendor-oauth");
    assert_ne!(profile.id, "anthropic-oauth");
    let oauth = profile.oauth.as_ref().expect("oauth");
    assert_eq!(oauth.creds_format, Some(CredsFormat::JsonPointer));
    assert_eq!(oauth.access_token_ptr.as_deref(), Some("/tokens/access"));
    assert_ne!(
        oauth.access_token_ptr.as_deref(),
        Some("/claudeAiOauth/accessToken")
    );
    assert_eq!(
        oauth.token_headers.get("X-Client").map(String::as_str),
        Some("official-app")
    );
    assert!(
        profile.betas.values.is_empty(),
        "other-vendor must not inherit Anthropic betas"
    );

    profile.oauth.as_mut().expect("oauth").token_url = url;
    let provider = provider_from_profile(&profile).expect("provider from gist");
    let token = provider.get_token().await.expect("json-pointer refresh");
    assert_eq!(token, "new-access");
    let req = handle.join().expect("server");
    assert!(
        req.contains("X-Client: official-app") || req.contains("x-client: official-app"),
        "token_headers must be sent, got: {req}"
    );
    assert!(
        req.contains("grant_type=refresh_token") || req.contains("grant_type\":"),
        "form grant, got: {req}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&creds).unwrap()).unwrap();
    assert_eq!(doc["tokens"]["access"].as_str(), Some("new-access"));
    assert_eq!(doc["tokens"]["refresh"].as_str(), Some("new-refresh"));
    assert_eq!(doc["keep"].as_bool(), Some(true));
}

#[test]
fn load_profile_from_cli_path_overlays_matching_id() {
    let _home = IsolatedHome::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_toml(
        dir.path(),
        "rotate.toml",
        r#"
schema_version = 1
id = "anthropic-oauth"
[oauth]
client_id = "cli-rotated"
"#,
    );
    let opts = LoadOptions {
        include_shipped: true,
        include_user_config: false,
        ..hermetic_opts()
    };
    let profile = load_profile_from_cli(path.to_str().expect("utf8"), &opts).expect("path overlay");
    assert_eq!(profile.id, "anthropic-oauth");
    assert_eq!(
        profile.oauth.as_ref().and_then(|o| o.client_id.as_deref()),
        Some("cli-rotated")
    );
    assert_eq!(
        profile.oauth.as_ref().map(|o| o.token_url.as_str()),
        Some("https://platform.claude.com/v1/oauth/token"),
        "path form overlays shipped layers of the same id"
    );
}

#[test]
fn wiremux_profile_dir_is_user_dir_layer() {
    let home = IsolatedHome::new();
    let extra = home.path().join("extra-profiles");
    write_toml(
        &extra,
        "from-env.toml",
        r#"
schema_version = 1
id = "from-profile-dir"
base_url = "https://env.example.invalid"
"#,
    );
    home.set_env(
        "WIREMUX_PROFILE_DIR",
        extra.to_str().expect("utf8 extra dir"),
    );
    let profile = load_profile(
        "from-profile-dir",
        &LoadOptions {
            include_user_config: true,
            ..hermetic_opts()
        },
    )
    .expect("WIREMUX_PROFILE_DIR");
    assert_eq!(
        profile.http.base_url.as_deref(),
        Some("https://env.example.invalid")
    );
}

#[test]
fn default_user_profile_dir_is_last_overlay() {
    let home = IsolatedHome::new();
    let extra = home.path().join("ingest-dest");
    fs::create_dir_all(&extra).expect("dest");
    home.set_env(
        "WIREMUX_PROFILE_DIR",
        extra.to_str().expect("utf8 extra dir"),
    );
    assert_eq!(default_user_profile_dir().as_deref(), Some(extra.as_path()));
}

#[cfg(target_os = "macos")]
#[test]
fn application_support_overlay_wins_over_xdg() {
    let home = IsolatedHome::new();
    let xdg = home.path().join(".config/wiremux/profiles");
    let app_support = home
        .path()
        .join("Library/Application Support/wiremux/profiles");
    write_toml(
        &xdg,
        "anthropic-oauth.toml",
        r#"
schema_version = 1
id = "anthropic-oauth"
display_name = "from-xdg"
[oauth]
token_url = "https://xdg.example.invalid/token"
"#,
    );
    write_toml(
        &app_support,
        "anthropic-oauth.toml",
        r#"
schema_version = 1
id = "anthropic-oauth"
display_name = "from-application-support"
[oauth]
token_url = "https://appsupport.example.invalid/token"
"#,
    );
    let profile = load_profile(
        "anthropic-oauth",
        &LoadOptions {
            include_shipped: false,
            include_user_config: true,
            ..hermetic_opts()
        },
    )
    .expect("mac user-dir sibling");
    assert_eq!(
        profile.display_name.as_deref(),
        Some("from-application-support")
    );
    assert_eq!(
        profile.oauth.as_ref().map(|o| o.token_url.as_str()),
        Some("https://appsupport.example.invalid/token")
    );
}

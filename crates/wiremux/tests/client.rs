//! WireClient mock-HTTP tests. IsolatedHome for env. No live vendor calls.

#![cfg(feature = "client")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::json;
use wiremux::{
    ClientError, IrItem, IrPart, IrRequest, IrSampling, IrStreamEvent, WireClient,
    parse_profile_str,
};
use wiremux_auth::{AnyTokenProvider, IsolatedHome, PlantCredentials, StaticToken};

const PLANTED_ACCESS: &str = "sk-ant-oat01-client73";
const PLANTED_SK: &str = "sk-planted-secret73";
const PLANTED_XAI: &str = "xai-planted-secret73";

fn simple_ir(model: &str) -> IrRequest {
    IrRequest {
        model: model.to_string(),
        items: vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
        tools: vec![],
        sampling: IrSampling::default(),
    }
}

fn chat_profile(base: &str) -> wiremux::ResolvedProfile {
    parse_profile_str(&format!(
        r#"
schema_version = 1
id = "mock-chat"
wire = "chat-completions"
auth_scheme = "bearer"
base_url = "{base}"
chat_path = "/v1/chat/completions"
"#
    ))
    .expect("profile")
}

fn client_for(base: &str, token: &str) -> WireClient {
    WireClient::from_resolved(
        chat_profile(base),
        AnyTokenProvider::from(StaticToken::new(token)),
    )
    .expect("client")
}

fn read_http_request(stream: &mut TcpStream) -> String {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(header_end) = find_double_crlf(&buf) {
                    let headers = String::from_utf8_lossy(&buf[..header_end]);
                    let want = content_length(&headers).unwrap_or(0);
                    if buf.len().saturating_sub(header_end) >= want {
                        break;
                    }
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn accept_timeout(listener: &TcpListener, timeout: Duration) -> Option<TcpStream> {
    listener.set_nonblocking(true).ok()?;
    let start = std::time::Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                return Some(stream);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() > timeout {
                    return None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            return value.trim().parse().ok();
        }
    }
    None
}

fn write_http(stream: &mut TcpStream, status: u16, reason: &str, headers: &str, body: &str) {
    let extra = if headers.is_empty() {
        String::new()
    } else if headers.ends_with("\r\n") {
        headers.to_string()
    } else {
        format!("{headers}\r\n")
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn spawn_one(
    status: u16,
    reason: &'static str,
    headers: &'static str,
    body: impl Into<String>,
) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let body = body.into();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        write_http(&mut stream, status, reason, headers, &body);
        req
    });
    (format!("http://{addr}"), handle)
}

fn complete_chat_body() -> String {
    json!({
        "choices": [{
            "message": { "role": "assistant", "content": "hello from complete" },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
    })
    .to_string()
}

fn complete_tools_body() -> String {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_complete",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"SF\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
    .to_string()
}

#[tokio::test]
async fn from_profile_anthropic_oauth_applies_shipped_headers() {
    let home = IsolatedHome::new();
    home.plant_credentials(PlantCredentials::Claude {
        access: PLANTED_ACCESS,
        refresh: Some("rt"),
        expires_at_ms: Some(4_000_000_000_000),
    });

    let _constructed = WireClient::from_profile("anthropic-oauth").expect("from_profile");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let dir = home.path().join(".config/wiremux/profiles");
    std::fs::create_dir_all(&dir).expect("mkdir profiles");
    std::fs::write(
        dir.join("anthropic-oauth.toml"),
        format!(
            r#"
schema_version = 1
id = "anthropic-oauth"
base_url = "http://{addr}"
"#
        ),
    )
    .expect("overlay");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let body = r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#;
        write_http(&mut stream, 200, "OK", "", body);
        req
    });

    let client = WireClient::from_profile("anthropic-oauth").expect("overlay from_profile");
    let _ = client.send(simple_ir("claude-3-5-sonnet-latest")).await;
    let req = handle.join().expect("join");
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("anthropic-version: 2023-06-01"),
        "missing anthropic-version: {req}"
    );
    assert!(
        lower.contains("anthropic-beta:") && req.contains("oauth-2025-04-20"),
        "missing anthropic-beta from shipped profile: {req}"
    );
    assert!(
        req.contains(&format!("Bearer {PLANTED_ACCESS}")),
        "Bearer must come from planted token: {req}"
    );
    let _ = home;
}

#[tokio::test]
async fn from_profile_anthropic_auth_token_oat_is_bearer() {
    let home = IsolatedHome::new();
    home.set_env("ANTHROPIC_AUTH_TOKEN", "sk-ant-oat01-client-auth");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let dir = home.path().join(".config/wiremux/profiles");
    std::fs::create_dir_all(&dir).expect("mkdir profiles");
    std::fs::write(
        dir.join("anthropic.toml"),
        format!(
            r#"
schema_version = 1
id = "anthropic"
base_url = "http://{addr}"
"#
        ),
    )
    .expect("overlay");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let body = r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#;
        write_http(&mut stream, 200, "OK", "", body);
        req
    });

    let client = WireClient::from_profile("anthropic").expect("from_profile");
    let _ = client.send(simple_ir("claude-3-5-sonnet-latest")).await;
    let req = handle.join().expect("join");
    assert!(
        req.contains("Bearer sk-ant-oat01-client-auth"),
        "AUTH_TOKEN oat must be Authorization Bearer: {req}"
    );
    let lower = req.to_ascii_lowercase();
    assert!(
        !lower.contains("x-api-key: sk-ant-oat01-client-auth"),
        "AUTH_TOKEN oat must not be x-api-key: {req}"
    );
    let _ = home;
}

#[tokio::test]
async fn from_profile_anthropic_api_key_is_x_api_key() {
    let home = IsolatedHome::new();
    home.set_env("ANTHROPIC_API_KEY", "sk-ant-api03-key");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let dir = home.path().join(".config/wiremux/profiles");
    std::fs::create_dir_all(&dir).expect("mkdir profiles");
    std::fs::write(
        dir.join("anthropic.toml"),
        format!(
            r#"
schema_version = 1
id = "anthropic"
base_url = "http://{addr}"
"#
        ),
    )
    .expect("overlay");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let body = r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#;
        write_http(&mut stream, 200, "OK", "", body);
        req
    });

    let client = WireClient::from_profile("anthropic").expect("from_profile");
    let _ = client.send(simple_ir("claude-3-5-sonnet-latest")).await;
    let req = handle.join().expect("join");
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("x-api-key: sk-ant-api03-key"),
        "official API key must stay x-api-key: {req}"
    );
    assert!(
        !req.contains("Bearer sk-ant-api03-key"),
        "official API key must not be Bearer: {req}"
    );
    let _ = home;
}

#[tokio::test]
async fn send_chat_complete_text_finish_usage() {
    let (base, handle) = spawn_one(200, "OK", "", complete_chat_body());
    let client = client_for(&base, "sk-test");
    let (events, _loss) = client.send(simple_ir("gpt-4")).await.expect("send");
    let _ = handle.join();
    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "hello from complete")
        ),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::FinishReason { reason } if reason == "stop")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                ..
            }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn send_chat_complete_tool_calls() {
    let (base, handle) = spawn_one(200, "OK", "", complete_tools_body());
    let client = client_for(&base, "sk-test");
    let (events, _loss) = client.send(simple_ir("gpt-4")).await.expect("send");
    let _ = handle.join();
    assert!(events.iter().any(
        |ev| matches!(ev, IrStreamEvent::ToolCallStart { id, name, .. } if id == "call_complete" && name == "get_weather")
    ));
    let args: String = events
        .iter()
        .filter_map(|ev| match ev {
            IrStreamEvent::ToolCallArgDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(args, r#"{"city":"SF"}"#);
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, IrStreamEvent::ToolCallEnd))
    );
}

#[tokio::test]
async fn stream_remaps_chat_sse_text_delta() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        req
    });
    let client = client_for(&format!("http://{addr}"), "sk-test");
    let mut stream = std::pin::pin!(client.stream(simple_ir("gpt-4")));
    let mut saw_text = false;
    while let Some(item) = stream.next().await {
        if let Ok(IrStreamEvent::TextDelta { text }) = item
            && text == "hi"
        {
            saw_text = true;
            break;
        }
    }
    let _ = handle.join();
    assert!(saw_text, "stream must remap at least one text delta");
}

#[tokio::test]
async fn stream_http_200_wrapped_error_is_transient() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let sse = "data: {\"error\":{\"code\":429,\"message\":\"overloaded\"}}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        req
    });
    let client = client_for(&format!("http://{addr}"), "sk-test");
    let mut stream = std::pin::pin!(client.stream(simple_ir("gpt-4")));
    let first = stream.next().await.expect("first stream item");
    let _ = handle.join();
    match first {
        Err(ClientError::Transient { status, .. }) => assert_eq!(status, Some(200)),
        other => panic!("expected Transient status 200, got {other:?}"),
    }
}

#[tokio::test]
async fn http_401_is_auth() {
    let (base, handle) = spawn_one(401, "Unauthorized", "", r#"{"error":{"message":"nope"}}"#);
    let err = client_for(&base, "sk-test")
        .send(simple_ir("gpt-4"))
        .await
        .expect_err("401");
    let _ = handle.join();
    match err {
        ClientError::Auth { status, .. } => assert_eq!(status, Some(401)),
        other => panic!("expected Auth, got {other}"),
    }
}

#[tokio::test]
async fn xai_shaped_400_incorrect_api_key_is_auth() {
    let (base, handle) = spawn_one(
        400,
        "Bad Request",
        "",
        r#"{"error":{"message":"Incorrect API key provided"}}"#,
    );
    let err = client_for(&base, "sk-test")
        .send(simple_ir("grok-3"))
        .await
        .expect_err("400 key");
    let _ = handle.join();
    match err {
        ClientError::Auth { status, .. } => assert_eq!(status, Some(400)),
        other => panic!("expected Auth, got {other}"),
    }
}

#[tokio::test]
async fn http_404_is_not_found() {
    let (base, handle) = spawn_one(404, "Not Found", "", r#"{"error":{"message":"missing"}}"#);
    let err = client_for(&base, "sk-test")
        .send(simple_ir("gpt-4"))
        .await
        .expect_err("404");
    let _ = handle.join();
    match err {
        ClientError::NotFound { status, .. } => assert_eq!(status, Some(404)),
        other => panic!("expected NotFound, got {other}"),
    }
}

#[tokio::test]
async fn http_429_is_rate_limit() {
    let (base, handle) = spawn_one(
        429,
        "Too Many Requests",
        "Retry-After: 7",
        r#"{"error":{"message":"slow down"}}"#,
    );
    let err = client_for(&base, "sk-test")
        .send(simple_ir("gpt-4"))
        .await
        .expect_err("429");
    let _ = handle.join();
    match err {
        ClientError::RateLimit {
            status,
            retry_after,
            ..
        } => {
            assert_eq!(status, Some(429));
            assert_eq!(retry_after, Some(7));
        }
        other => panic!("expected RateLimit, got {other}"),
    }
}

#[tokio::test]
async fn http_503_is_transient() {
    let (base, handle) = spawn_one(503, "Service Unavailable", "", "down");
    let err = client_for(&base, "sk-test")
        .send(simple_ir("gpt-4"))
        .await
        .expect_err("503");
    let _ = handle.join();
    match err {
        ClientError::Transient { status, .. } => assert_eq!(status, Some(503)),
        other => panic!("expected Transient, got {other}"),
    }
}

#[tokio::test]
async fn http_200_assistant_does_not_exist_is_not_not_found() {
    let body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "The file you mentioned does not exist."
            },
            "finish_reason": "stop"
        }]
    })
    .to_string();
    let (base, handle) = spawn_one(200, "OK", "", body);
    let (events, _loss) = client_for(&base, "sk-test")
        .send(simple_ir("gpt-4"))
        .await
        .expect("assistant text mentioning does not exist is a completion");
    let _ = handle.join();
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            IrStreamEvent::TextDelta { text } if text.contains("does not exist")
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn http_200_wrapped_overload_is_transient() {
    let (base, handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"error":{"code":429,"message":"overloaded"}}"#,
    );
    let err = client_for(&base, "sk-test")
        .send(simple_ir("gpt-4"))
        .await
        .expect_err("200 error");
    let _ = handle.join();
    match err {
        ClientError::Transient { status, .. } => assert_eq!(status, Some(200)),
        other => panic!("expected Transient, not {other}"),
    }
}

#[tokio::test]
async fn http_400_model_not_found() {
    let (base, handle) = spawn_one(
        400,
        "Bad Request",
        "",
        r#"{"error":{"message":"model_not_found: does not exist"}}"#,
    );
    let err = client_for(&base, "sk-test")
        .send(simple_ir("nope"))
        .await
        .expect_err("400 model");
    let _ = handle.join();
    match err {
        ClientError::NotFound { status, .. } => assert_eq!(status, Some(400)),
        other => panic!("expected NotFound, got {other}"),
    }
}

#[tokio::test]
async fn list_models_404_is_empty() {
    let (base, handle) = spawn_one(404, "Not Found", "", "missing");
    let models = client_for(&base, "sk-test")
        .list_models()
        .await
        .expect("404 list");
    let req = handle.join().expect("join");
    assert!(models.is_empty(), "{models:?}");
    assert!(
        req.contains("GET /v1/models"),
        "list_models must GET /v1/models: {req}"
    );
}

#[tokio::test]
async fn list_models_gemini_uses_v1beta() {
    let (base, handle) = spawn_one(404, "Not Found", "", "missing");
    let profile = parse_profile_str(&format!(
        r#"
schema_version = 1
id = "mock-gemini"
wire = "gemini"
auth_scheme = "bearer"
base_url = "{base}"
chat_path = "/v1beta/models/{{model}}:generateContent"
"#
    ))
    .expect("gemini");
    let client =
        WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("sk-test")))
            .expect("client");
    let models = client.list_models().await.expect("404 list");
    let req = handle.join().expect("join");
    assert!(models.is_empty(), "{models:?}");
    assert!(
        req.contains("GET /v1beta/models"),
        "gemini list_models must GET /v1beta/models: {req}"
    );
}

#[tokio::test]
async fn list_models_no_version_prefix_gets_models() {
    let (base, handle) = spawn_one(404, "Not Found", "", "missing");
    let profile = parse_profile_str(&format!(
        r#"
schema_version = 1
id = "mock-bare"
wire = "chat-completions"
auth_scheme = "bearer"
base_url = "{base}"
chat_path = "/chat/completions"
"#
    ))
    .expect("bare");
    let client =
        WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("sk-test")))
            .expect("client");
    let models = client.list_models().await.expect("404 list");
    let req = handle.join().expect("join");
    assert!(models.is_empty(), "{models:?}");
    assert!(
        req.contains("GET /models"),
        "list_models without a version prefix must GET /models: {req}"
    );
    assert!(
        !req.contains("GET /v1/models"),
        "must not invent a version prefix: {req}"
    );
}

#[tokio::test]
async fn list_models_401_is_auth() {
    let (base, handle) = spawn_one(401, "Unauthorized", "", r#"{"error":"no"}"#);
    let err = client_for(&base, "sk-test")
        .list_models()
        .await
        .expect_err("401 list");
    let _ = handle.join();
    match err {
        ClientError::Auth { status, .. } => assert_eq!(status, Some(401)),
        other => panic!("expected Auth, got {other}"),
    }
}

#[tokio::test]
async fn ollama_show_not_called_on_loopback_test_port() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        write_http(
            &mut stream,
            200,
            "OK",
            "",
            r#"{"data":[{"id":"grok-3","context_length":131072}]}"#,
        );
        // A second accept would hang if the client POSTs /api/show. Time out instead.
        listener.set_nonblocking(true).expect("nonblocking");
        let extra = listener
            .accept()
            .ok()
            .map(|(mut s, _)| read_http_request(&mut s));
        (req, extra)
    });
    let profile = parse_profile_str(&format!(
        r#"
schema_version = 1
id = "xai"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://{addr}"
chat_path = "/v1/chat/completions"
"#
    ))
    .expect("xai-shaped");
    let client = WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("")))
        .expect("client");
    let models = client.list_models().await.expect("list");
    let (req, extra) = handle.join().expect("join");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "grok-3");
    assert!(req.contains("GET /v1/models"), "{req}");
    assert!(
        !req.contains("/api/show"),
        "xAI-shaped loopback test port must not POST /api/show: {req}"
    );
    assert!(
        extra.as_ref().is_none_or(|r| !r.contains("/api/show")),
        "must not call /api/show: {extra:?}"
    );
}

#[tokio::test]
async fn ollama_show_called_on_loopback_11434() {
    let listener = match TcpListener::bind("127.0.0.1:11434") {
        Ok(l) => l,
        Err(_) => return,
    };
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..2 {
            let Some(mut stream) = accept_timeout(&listener, Duration::from_secs(5)) else {
                break;
            };
            let req = read_http_request(&mut stream);
            if req.contains("/api/show") {
                write_http(
                    &mut stream,
                    200,
                    "OK",
                    "",
                    r#"{"model_info":{"llama.context_length":8192}}"#,
                );
            } else {
                write_http(&mut stream, 200, "OK", "", r#"{"data":[{"id":"llama3"}]}"#);
            }
            seen.push(req);
        }
        seen
    });
    let profile = parse_profile_str(
        r#"
schema_version = 1
id = "grok-ollama"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://127.0.0.1:11434"
chat_path = "/v1/chat/completions"
"#,
    )
    .expect("ollama");
    let client = WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("")))
        .expect("client");
    let models = client.list_models().await.expect("list");
    let seen = handle.join().expect("join");
    assert_eq!(models[0].id, "llama3");
    assert!(
        seen.iter().any(|r| r.contains("POST /api/show")),
        "loopback :11434 must POST /api/show, got {seen:?}"
    );
}

#[tokio::test]
async fn display_redacts_secrets_and_debug_hides_token() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let body = format!(r#"{{"error":{{"message":"bad {PLANTED_SK} and {PLANTED_XAI}"}}}}"#);
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        write_http(&mut stream, 401, "Unauthorized", "", &body);
        req
    });
    let client = client_for(&format!("http://{addr}"), PLANTED_SK);
    let dbg = format!("{client:?}");
    assert!(
        !dbg.contains(PLANTED_SK),
        "Debug must not print the token: {dbg}"
    );
    let err = client.send(simple_ir("gpt-4")).await.expect_err("401");
    let _ = handle.join();
    let display = format!("{err}");
    assert!(
        !display.contains(PLANTED_SK) && !display.contains(PLANTED_XAI),
        "Display must redact sk-/xai- secrets: {display}"
    );
    assert!(
        display.contains("[redacted]"),
        "Display should mark redaction: {display}"
    );
}

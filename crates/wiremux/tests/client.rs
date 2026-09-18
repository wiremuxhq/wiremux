//! WireClient mock-HTTP tests. IsolatedHome for env. No live vendor calls.

#![cfg(feature = "client")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::json;
use wiremux::{
    ClientError, IrItem, IrPart, IrRequest, IrStreamEvent, TransientKind, WireClient,
    parse_profile_str,
};
use wiremux_auth::{
    AnyTokenProvider, GcpTokenProvider, IsolatedHome, PlantCredentials, StaticToken,
    provider_from_profile,
};

const PLANTED_ACCESS: &str = "sk-ant-oat01-client73";
const PLANTED_SK: &str = "sk-planted-secret73";
const PLANTED_XAI: &str = "xai-planted-secret73";

fn simple_ir(model: &str) -> IrRequest {
    IrRequest::new(
        model,
        vec![IrItem::User {
            parts: vec![IrPart::Text("hi".into())],
        }],
    )
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

fn messages_client_for(base: &str, token: &str) -> WireClient {
    let profile = parse_profile_str(&format!(
        r#"
schema_version = 1
id = "mock-messages"
wire = "messages"
auth_scheme = "none"
base_url = "{base}"
chat_path = "/v1/messages"
"#
    ))
    .expect("messages profile");
    WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new(token)))
        .expect("client")
}

fn converse_client_for(base: &str) -> WireClient {
    let profile = parse_profile_str(&format!(
        r#"
schema_version = 1
id = "mock-converse"
wire = "converse"
auth_scheme = "none"
base_url = "{base}"
chat_path = "/model/{{model}}/converse"
"#
    ))
    .expect("converse profile");
    WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("none")))
        .expect("client")
}

fn write_http_bytes(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
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
async fn from_profile_gemini_api_key_is_x_goog_api_key() {
    let home = IsolatedHome::new();
    home.set_env("GEMINI_API_KEY", "AIza-test");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let dir = home.path().join(".config/wiremux/profiles");
    std::fs::create_dir_all(&dir).expect("mkdir profiles");
    std::fs::write(
        dir.join("gemini.toml"),
        format!(
            r#"
schema_version = 1
id = "gemini"
base_url = "http://{addr}"
"#
        ),
    )
    .expect("overlay");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let body =
            r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}"#;
        write_http(&mut stream, 200, "OK", "", body);
        req
    });

    let client = WireClient::from_profile("gemini").expect("from_profile");
    let _ = client.send(simple_ir("gemini-2.0-flash")).await;
    let req = handle.join().expect("join");
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("x-goog-api-key: aiza-test"),
        "Studio API key must be x-goog-api-key: {req}"
    );
    assert!(
        !req.contains("Bearer AIza-test"),
        "Studio API key must not be Bearer: {req}"
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
            IrStreamEvent::ToolCallArgDelta { delta, .. } => Some(delta.as_str()),
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
async fn stream_http_200_raw_json_error_without_data_prefix_is_transient() {
    let (base, handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"error":{"code":429,"message":"overloaded"}}"#,
    );
    let client = client_for(&base, "sk-test");
    let mut stream = std::pin::pin!(client.stream(simple_ir("gpt-4")));
    let first = stream.next().await.expect("first stream item");
    let _ = handle.join();
    match first {
        Err(ClientError::Transient { status, .. }) => assert_eq!(status, Some(200)),
        other => panic!("expected Transient status 200, got {other:?}"),
    }
}

#[tokio::test]
async fn stream_http_200_garbage_body_is_vendor() {
    let (base, handle) = spawn_one(200, "OK", "", "<html>oops</html>");
    let client = client_for(&base, "sk-test");
    let mut stream = std::pin::pin!(client.stream(simple_ir("gpt-4")));
    let first = stream.next().await.expect("first stream item");
    let _ = handle.join();
    match first {
        Err(ClientError::Vendor { status, .. }) => assert_eq!(status, Some(200)),
        other => panic!("expected Vendor status 200, got {other:?}"),
    }
}

#[tokio::test]
async fn send_forces_stream_false_even_when_ir_says_true() {
    let (base, handle) = spawn_one(200, "OK", "", complete_chat_body());
    let client = client_for(&base, "sk-test");
    let mut ir = simple_ir("gpt-4");
    ir.sampling.stream = Some(true);
    let _ = client.send(ir).await.expect("send");
    let req = handle.join().expect("join");
    assert!(
        req.contains("\"stream\":false") || req.contains("\"stream\": false"),
        "send() must force stream=false, got {req}"
    );
}

#[tokio::test]
async fn stream_forces_stream_true_even_when_ir_says_false() {
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
    let mut ir = simple_ir("gpt-4");
    ir.sampling.stream = Some(false);
    let mut stream = std::pin::pin!(client.stream(ir));
    let _ = stream.next().await;
    let req = handle.join().expect("join");
    assert!(
        req.contains("\"stream\":true") || req.contains("\"stream\": true"),
        "stream() must force stream=true, got {req}"
    );
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
async fn stream_error_after_ping_is_classified() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let sse = concat!(
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: error\ndata: {\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        req
    });
    let client = messages_client_for(&format!("http://{addr}"), "sk-test");
    let mut stream = std::pin::pin!(client.stream(simple_ir("claude-3")));
    let mut first_err = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(_) => {}
            Err(err) => {
                first_err = Some(err);
                break;
            }
        }
    }
    let _ = handle.join();
    match first_err {
        Some(ClientError::Transient { status, .. } | ClientError::Vendor { status, .. }) => {
            assert_eq!(status, Some(200));
        }
        Some(ClientError::Map(err)) => panic!("mid-stream Anthropic error must not be Map: {err}"),
        Some(other) => panic!("expected Transient or Vendor, got {other:?}"),
        None => panic!("expected Transient or Vendor, got empty success"),
    }
}

#[tokio::test]
async fn stream_error_after_message_start_is_classified() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let sse = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\"}}\n\n",
            "event: error\ndata: {\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        req
    });
    let client = messages_client_for(&format!("http://{addr}"), "sk-test");
    let mut stream = std::pin::pin!(client.stream(simple_ir("claude-3")));
    let mut first_err = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(_) => {}
            Err(err) => {
                first_err = Some(err);
                break;
            }
        }
    }
    let _ = handle.join();
    match first_err {
        Some(ClientError::Transient { status, .. } | ClientError::Vendor { status, .. }) => {
            assert_eq!(status, Some(200));
        }
        Some(ClientError::Map(err)) => panic!("mid-stream Anthropic error must not be Map: {err}"),
        Some(other) => panic!("expected Transient or Vendor, got {other:?}"),
        None => panic!("expected Transient or Vendor, got empty success"),
    }
}

#[tokio::test]
async fn stream_error_after_chat_delta_is_transient() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"error\":{\"code\":429,\"message\":\"overloaded\"}}\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        req
    });
    let client = client_for(&format!("http://{addr}"), "sk-test");
    let mut stream = std::pin::pin!(client.stream(simple_ir("gpt-4")));
    let mut first_err = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(_) => {}
            Err(err) => {
                first_err = Some(err);
                break;
            }
        }
    }
    let _ = handle.join();
    match first_err {
        Some(ClientError::Transient { status, .. }) => assert_eq!(status, Some(200)),
        Some(other) => panic!("expected Transient status 200, got {other:?}"),
        None => panic!("expected Transient status 200, got empty success"),
    }
}

#[tokio::test]
async fn stream_eventstream_validation_exception_is_vendor() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        let body = wiremux::stream::encode_eventstream_exception(
            "validationException",
            br#"{"message":"The provided model identifier is invalid."}"#,
        );
        write_http_bytes(
            &mut stream,
            200,
            "OK",
            "application/vnd.amazon.eventstream",
            &body,
        );
        req
    });
    let client = converse_client_for(&format!("http://{addr}"));
    let mut stream = std::pin::pin!(client.stream(simple_ir("amazon.nova-lite-v1:0")));
    let first = stream.next().await.expect("first stream item");
    let _ = handle.join();
    match first {
        Err(ClientError::Vendor { status, message }) => {
            assert_eq!(status, Some(200));
            assert!(
                message.contains("validationException") || message.contains("model identifier"),
                "{message}"
            );
        }
        Err(ClientError::NotFound { status, .. }) => assert_eq!(status, Some(200)),
        Err(ClientError::Transport(err)) => {
            panic!("eventstream validationException must not be Transport: {err}");
        }
        other => panic!("expected Vendor (or NotFound), got {other:?}"),
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
        ClientError::Transient { status, kind, .. } => {
            assert_eq!(status, Some(503));
            assert_eq!(kind, TransientKind::Http);
            assert!(!err.is_connect());
            assert!(!err.is_timeout());
        }
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
        ClientError::Transient { status, kind, .. } => {
            assert_eq!(status, Some(200));
            assert_eq!(kind, TransientKind::Http);
            assert!(!err.is_connect());
            assert!(!err.is_timeout());
        }
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
async fn list_models_reads_grok_build_context_window() {
    let (base, handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"data":[{"id":"grok-4.6","context_window":500000}]}"#,
    );
    let models = client_for(&base, "sk-test")
        .list_models()
        .await
        .expect("list");
    let _ = handle.join();
    assert_eq!(models.len(), 1, "{models:?}");
    assert_eq!(models[0].id, "grok-4.6");
    assert_eq!(models[0].context_tokens, Some(500_000));
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
async fn list_models_gemini_parses_models_array() {
    let (base, handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"models":[{"name":"models/gemini-2.0-flash","inputTokenLimit":1048576}]}"#,
    );
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
    let models = client.list_models().await.expect("list");
    let _ = handle.join();
    assert_eq!(
        models.len(),
        1,
        "gemini models[] must not parse as empty: {models:?}"
    );
    assert_eq!(models[0].id, "gemini-2.0-flash");
    assert_eq!(models[0].context_tokens, Some(1_048_576));
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
async fn list_models_survives_ollama_show_body_read_failure() {
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
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10000\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Both);
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
    let models = client
        .list_models()
        .await
        .expect("list after show body fail");
    let seen = handle.join().expect("join");
    assert_eq!(models.len(), 1, "{models:?}");
    assert_eq!(models[0].id, "llama3");
    assert!(
        seen.iter().any(|r| r.contains("POST /api/show")),
        "loopback :11434 must POST /api/show, got {seen:?}"
    );
}

#[tokio::test]
async fn list_models_survives_ollama_show_500() {
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
                write_http(&mut stream, 500, "Internal Server Error", "", "nope");
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
    let models = client.list_models().await.expect("list after show 500");
    let seen = handle.join().expect("join");
    assert_eq!(models.len(), 1, "{models:?}");
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
    let dbg_err = format!("{err:?}");
    assert!(
        !display.contains(PLANTED_SK) && !display.contains(PLANTED_XAI),
        "Display must redact sk-/xai- secrets: {display}"
    );
    assert!(
        !dbg_err.contains(PLANTED_SK) && !dbg_err.contains(PLANTED_XAI),
        "Debug must not print raw secrets: {dbg_err}"
    );
    assert!(
        display.contains("[redacted]"),
        "Display should mark redaction: {display}"
    );
}

fn vertex_profile(base: &str, extra_headers: &str) -> wiremux::ResolvedProfile {
    parse_profile_str(&format!(
        r#"
schema_version = 1
id = "google-vertex"
wire = "gemini"
auth_scheme = "bearer"
base_url = "{base}"
chat_path = "/v1beta/models/{{model}}:generateContent"
{extra_headers}
"#
    ))
    .expect("vertex profile")
}

fn authorized_user_adc(token_uri: &str, quota_project_id: Option<&str>) -> String {
    let mut value = json!({
        "type": "authorized_user",
        "client_id": "123.apps.googleusercontent.com",
        "client_secret": "user-secret",
        "refresh_token": "1//refresh-me",
        "token_uri": token_uri,
    });
    if let Some(quota) = quota_project_id {
        value["quota_project_id"] = json!(quota);
    }
    value.to_string()
}

const GEMINI_OK_BODY: &str =
    r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}"#;

#[tokio::test]
async fn authorized_user_adc_sends_x_goog_user_project() {
    let (token_base, token_handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"access_token":"ya29.user","expires_in":3600,"token_type":"Bearer"}"#,
    );
    let (api_base, api_handle) = spawn_one(200, "OK", "", GEMINI_OK_BODY);
    let provider = GcpTokenProvider::from_key(&authorized_user_adc(
        &format!("{token_base}/token"),
        Some("billing-proj"),
    ))
    .expect("from_key");
    let client = WireClient::from_resolved(
        vertex_profile(&api_base, ""),
        AnyTokenProvider::from(provider),
    )
    .expect("client");
    let _ = client.send(simple_ir("gemini-2.0-flash")).await;
    let req = api_handle.join().expect("join");
    let _ = token_handle.join();
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("x-goog-user-project: billing-proj"),
        "ADC quota_project_id must be sent as x-goog-user-project: {req}"
    );
}

#[tokio::test]
async fn profile_x_goog_user_project_is_not_overwritten() {
    let (token_base, token_handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"access_token":"ya29.user","expires_in":3600,"token_type":"Bearer"}"#,
    );
    let (api_base, api_handle) = spawn_one(200, "OK", "", GEMINI_OK_BODY);
    let provider = GcpTokenProvider::from_key(&authorized_user_adc(
        &format!("{token_base}/token"),
        Some("billing-proj"),
    ))
    .expect("from_key");
    let client = WireClient::from_resolved(
        vertex_profile(
            &api_base,
            r#"
[headers]
x-goog-user-project = "profile-proj"
"#,
        ),
        AnyTokenProvider::from(provider),
    )
    .expect("client");
    let _ = client.send(simple_ir("gemini-2.0-flash")).await;
    let req = api_handle.join().expect("join");
    let _ = token_handle.join();
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("x-goog-user-project: profile-proj"),
        "profile header must win: {req}"
    );
    assert!(
        !lower.contains("x-goog-user-project: billing-proj"),
        "must not overwrite profile x-goog-user-project: {req}"
    );
}

#[tokio::test]
async fn authorized_user_adc_without_quota_does_not_invent_header() {
    let (token_base, token_handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"access_token":"ya29.user","expires_in":3600,"token_type":"Bearer"}"#,
    );
    let (api_base, api_handle) = spawn_one(200, "OK", "", GEMINI_OK_BODY);
    let provider =
        GcpTokenProvider::from_key(&authorized_user_adc(&format!("{token_base}/token"), None))
            .expect("from_key");
    let client = WireClient::from_resolved(
        vertex_profile(&api_base, ""),
        AnyTokenProvider::from(provider),
    )
    .expect("client");
    let _ = client.send(simple_ir("gemini-2.0-flash")).await;
    let req = api_handle.join().expect("join");
    let _ = token_handle.join();
    assert!(
        !req.to_ascii_lowercase().contains("x-goog-user-project"),
        "ADC without quota_project_id must not invent the header: {req}"
    );
}

#[tokio::test]
async fn stream_http_200_leftover_vendor_body_redacts_secret_and_userinfo() {
    let planted_url = "https://user:s3cret@evil.test/x?k=1";
    let body = format!(r#"{{"error":{{"message":"bad {PLANTED_SK} and {planted_url}"}}}}"#);
    let (base, handle) = spawn_one(200, "OK", "", body);
    let client = client_for(&base, "sk-test");
    let mut stream = std::pin::pin!(client.stream(simple_ir("gpt-4")));
    let first = stream.next().await.expect("first stream item");
    let _ = handle.join();
    let err = first.expect_err("200 leftover JSON error");
    match &err {
        ClientError::Vendor { status, .. } | ClientError::Transient { status, .. } => {
            assert_eq!(*status, Some(200));
        }
        other => panic!("expected Vendor or Transient, got {other:?}"),
    }
    let display = format!("{err}");
    let dbg = format!("{err:?}");
    assert!(!dbg.is_empty(), "Debug must show variant: {dbg}");
    assert!(
        dbg.contains("Vendor") || dbg.contains("Transient"),
        "Debug must name Vendor or Transient: {dbg}"
    );
    for leaked in [PLANTED_SK, "s3cret", planted_url] {
        assert!(
            !display.contains(leaked),
            "Display must not leak {leaked}: {display}"
        );
        assert!(!dbg.contains(leaked), "Debug must not leak {leaked}: {dbg}");
    }
}

fn authorization_lines(req: &str) -> Vec<String> {
    req.lines()
        .filter(|line| {
            line.split_once(':')
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        })
        .map(str::to_string)
        .collect()
}

fn converse_complete_body() -> String {
    json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [{"text": "ok"}]
            }
        },
        "stopReason": "end_turn"
    })
    .to_string()
}

fn bedrock_profile(base: &str, extra: &str) -> wiremux::ResolvedProfile {
    parse_profile_str(&format!(
        r#"
schema_version = 1
id = "amazon-bedrock"
wire = "converse"
aws_service = "bedrock"
aws_region = "us-east-1"
base_url = "{base}"
chat_path = "/model/{{model}}/converse"
{extra}
"#
    ))
    .expect("bedrock profile")
}

#[tokio::test]
async fn bearer_and_iam_keys_send_one_authorization_bearer() {
    let home = IsolatedHome::with_extra_envs(&[
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_BEARER_TOKEN_BEDROCK",
        "AWS_REGION",
    ]);
    home.set_env("AWS_ACCESS_KEY_ID", "AKIATEST");
    home.set_env(
        "AWS_SECRET_ACCESS_KEY",
        "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
    );
    home.set_env("AWS_BEARER_TOKEN_BEDROCK", "bedrock-bearer-tok");

    let (base, handle) = spawn_one(200, "OK", "", converse_complete_body());
    let profile = bedrock_profile(
        &base,
        r#"
auth_scheme = "bearer"
access_env = ["AWS_BEARER_TOKEN_BEDROCK"]
"#,
    );
    let provider = provider_from_profile(&profile).expect("provider");
    let client = WireClient::from_resolved(profile, provider).expect("client");
    let _ = client.send(simple_ir("amazon.titan")).await;
    let req = handle.join().expect("join");
    let auths = authorization_lines(&req);
    assert_eq!(
        auths.len(),
        1,
        "exactly one Authorization, got {auths:?} in {req}"
    );
    assert!(
        auths[0].contains("Bearer bedrock-bearer-tok"),
        "bearer must win when token and IAM keys are both set: {auths:?}"
    );
    assert!(
        !req.to_ascii_lowercase().contains("aws4-hmac-sha256"),
        "must not SigV4 when a bearer token is present: {req}"
    );
    let _ = home;
}

#[tokio::test]
async fn aws_profile_file_credentials_sign() {
    let home = IsolatedHome::with_extra_envs(&[
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_BEARER_TOKEN_BEDROCK",
        "AWS_REGION",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
    ]);
    let creds_dir = home.path().join(".aws");
    std::fs::create_dir_all(&creds_dir).expect("mkdir .aws");
    std::fs::write(
        creds_dir.join("credentials"),
        "[dev]\naws_access_key_id = AKIAPROFILE\naws_secret_access_key = profilesecret\n",
    )
    .expect("write credentials");
    home.set_env("AWS_PROFILE", "dev");

    let (base, handle) = spawn_one(200, "OK", "", converse_complete_body());
    let profile = bedrock_profile(&base, r#"auth_scheme = "none""#);
    let client = WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("")))
        .expect("client");
    let _ = client.send(simple_ir("amazon.titan")).await;
    let req = handle.join().expect("join");
    let auths = authorization_lines(&req);
    assert_eq!(
        auths.len(),
        1,
        "IAM profile keys must produce one Authorization, got {auths:?} in {req}"
    );
    assert!(
        auths[0].to_ascii_lowercase().contains("aws4-hmac-sha256"),
        "must SigV4 from ~/.aws/credentials: {auths:?}"
    );
    assert!(
        auths[0].contains("AKIAPROFILE"),
        "credential must use the named profile access key: {auths:?}"
    );
    let _ = home;
}

#[tokio::test]
async fn aws_credential_process_signs() {
    let home = IsolatedHome::new();
    home.set_env("AWS_EC2_METADATA_DISABLED", "true");
    let script = {
        #[cfg(windows)]
        {
            let json_path = home.path().join("proc.json");
            std::fs::write(
                &json_path,
                r#"{"Version":1,"AccessKeyId":"AKIAPROC","SecretAccessKey":"procsecret"}"#,
            )
            .expect("write json");
            let cmd_path = home.path().join("credproc.cmd");
            std::fs::write(
                &cmd_path,
                format!("@echo off\r\ntype \"{}\"\r\n", json_path.display()),
            )
            .expect("write cmd");
            cmd_path
        }
        #[cfg(not(windows))]
        {
            let path = home.path().join("credproc.sh");
            std::fs::write(
                &path,
                "#!/bin/sh\nprintf '%s\\n' '{\"Version\":1,\"AccessKeyId\":\"AKIAPROC\",\"SecretAccessKey\":\"procsecret\"}'\n",
            )
            .expect("write sh");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            path
        }
    };
    let config = home.path().join("config");
    std::fs::write(
        &config,
        format!("[profile dev]\ncredential_process = {}\n", script.display()),
    )
    .expect("write config");
    home.set_env("AWS_PROFILE", "dev");
    home.set_env("AWS_CONFIG_FILE", config.to_str().expect("utf8"));

    let (base, handle) = spawn_one(200, "OK", "", converse_complete_body());
    let profile = bedrock_profile(&base, r#"auth_scheme = "none""#);
    let client = WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("")))
        .expect("client");
    let _ = client.send(simple_ir("amazon.titan")).await;
    let req = handle.join().expect("join");
    let auths = authorization_lines(&req);
    assert_eq!(
        auths.len(),
        1,
        "credential_process must produce one Authorization, got {auths:?} in {req}"
    );
    assert!(
        auths[0].contains("AKIAPROC"),
        "credential must use process access key: {auths:?}"
    );
    let _ = home;
}

#[tokio::test]
async fn aws_service_without_keys_is_local_auth_error() {
    let home = IsolatedHome::with_extra_envs(&[
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_BEARER_TOKEN_BEDROCK",
        "AWS_REGION",
    ]);
    home.set_env("AWS_EC2_METADATA_DISABLED", "true");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        accept_timeout(&listener, Duration::from_millis(200))
    });
    let profile = bedrock_profile(&format!("http://{addr}"), r#"auth_scheme = "none""#);
    let client = WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("")))
        .expect("client");
    let err = client
        .send(simple_ir("amazon.titan"))
        .await
        .expect_err("missing IAM keys");
    let accepted = handle.join().expect("join");
    assert!(
        accepted.is_none(),
        "must fail closed locally, no upstream call"
    );
    match err {
        ClientError::Auth { status, message } => {
            assert_eq!(status, None, "local auth error has no HTTP status");
            assert!(
                message.to_ascii_lowercase().contains("aws")
                    || message.contains("AWS_ACCESS_KEY_ID")
                    || message.contains("credentials"),
                "must name what was tried, got {message}"
            );
        }
        other => panic!("expected local Auth, got {other:?}"),
    }
    let _ = home;
}

fn spawn_script(responses: Vec<(u16, String)>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, body) in responses {
            let Some(mut stream) = accept_timeout(&listener, Duration::from_secs(2)) else {
                break;
            };
            seen.push(read_http_request(&mut stream));
            let reason = if status == 200 {
                "OK"
            } else if status == 401 {
                "Unauthorized"
            } else {
                "Error"
            };
            write_http(&mut stream, status, reason, "", &body);
        }
        seen
    });
    (format!("http://{addr}"), handle)
}

fn oauth_retry_profile(
    api_base: &str,
    token_url: &str,
    creds_path: &str,
) -> wiremux::ResolvedProfile {
    let creds_unix = creds_path.replace('\\', "/");
    parse_profile_str(&format!(
        r#"
schema_version = 1
id = "retry-oauth"
wire = "chat-completions"
auth_scheme = "bearer"
base_url = "{api_base}"
chat_path = "/v1/chat/completions"
[oauth]
token_url = "{token_url}"
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
    ))
    .expect("oauth retry profile")
}

#[tokio::test]
async fn send_401_oauth_retries_once() {
    let home = IsolatedHome::new();
    let creds = home.plant_credentials(PlantCredentials::JsonPointer {
        relative_path: ".config/wiremux/auth-retry.json",
        document: json!({
            "tokens": {
                "access": "first-159",
                "refresh": "rt-159",
                "expiry_unix": 4_102_444_800_i64
            }
        }),
    });
    let (token_base, token_handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"access_token":"second-159","refresh_token":"rt2","expires_in":3600}"#,
    );
    let (api_base, api_handle) = spawn_script(vec![
        (401, r#"{"error":{"message":"expired"}}"#.into()),
        (200, complete_chat_body()),
    ]);
    let profile = oauth_retry_profile(
        &api_base,
        &format!("{token_base}/token"),
        &creds.to_string_lossy(),
    );
    let provider = provider_from_profile(&profile).expect("provider");
    let client = WireClient::from_resolved(profile, provider).expect("client");
    let (events, _) = client.send(simple_ir("gpt-4")).await.expect("401 retry");
    let seen = api_handle.join().expect("join");
    let _ = token_handle.join();
    assert_eq!(seen.len(), 2, "must retry once, got {seen:?}");
    assert!(
        seen[0].contains("Bearer first-159"),
        "first try uses stored access: {}",
        seen[0]
    );
    assert!(
        seen[1].contains("Bearer second-159"),
        "retry uses refreshed access: {}",
        seen[1]
    );
    assert!(
        events.iter().any(
            |ev| matches!(ev, IrStreamEvent::TextDelta { text } if text == "hello from complete")
        ),
        "{events:?}"
    );
    let _ = home;
}

#[tokio::test]
async fn send_401_static_does_not_retry() {
    let (base, handle) = spawn_script(vec![
        (401, r#"{"error":{"message":"nope"}}"#.into()),
        (200, complete_chat_body()),
    ]);
    let err = client_for(&base, "sk-static")
        .send(simple_ir("gpt-4"))
        .await
        .expect_err("static 401");
    let seen = handle.join().expect("join");
    assert_eq!(seen.len(), 1, "static key must not retry, got {seen:?}");
    match err {
        ClientError::Auth { status, .. } => assert_eq!(status, Some(401)),
        other => panic!("expected Auth 401, got {other:?}"),
    }
}

#[tokio::test]
async fn stream_401_oauth_retries_once_before_body() {
    let home = IsolatedHome::new();
    let creds = home.plant_credentials(PlantCredentials::JsonPointer {
        relative_path: ".config/wiremux/auth-retry-stream.json",
        document: json!({
            "tokens": {
                "access": "first-stream",
                "refresh": "rt-stream",
                "expiry_unix": 4_102_444_800_i64
            }
        }),
    });
    let (token_base, token_handle) = spawn_one(
        200,
        "OK",
        "",
        r#"{"access_token":"second-stream","refresh_token":"rt2","expires_in":3600}"#,
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        let (mut stream, _) = listener.accept().expect("first");
        seen.push(read_http_request(&mut stream));
        write_http(
            &mut stream,
            401,
            "Unauthorized",
            "",
            r#"{"error":{"message":"expired"}}"#,
        );
        let (mut stream, _) = listener.accept().expect("retry");
        seen.push(read_http_request(&mut stream));
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        seen
    });
    let profile = oauth_retry_profile(
        &format!("http://{addr}"),
        &format!("{token_base}/token"),
        &creds.to_string_lossy(),
    );
    let provider = provider_from_profile(&profile).expect("provider");
    let client = WireClient::from_resolved(profile, provider).expect("client");
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
    let seen = handle.join().expect("join");
    let _ = token_handle.join();
    assert_eq!(
        seen.len(),
        2,
        "stream must retry 401 before body, got {seen:?}"
    );
    assert!(seen[0].contains("Bearer first-stream"), "{}", seen[0]);
    assert!(seen[1].contains("Bearer second-stream"), "{}", seen[1]);
    assert!(saw_text, "retry must yield remapped SSE");
    let _ = home;
}

#[tokio::test]
async fn read_timeout_secs_is_transient() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        thread::sleep(Duration::from_secs(3));
        write_http(&mut stream, 200, "OK", "", &complete_chat_body());
        req
    });
    let profile = parse_profile_str(&format!(
        r#"
schema_version = 1
id = "slow"
wire = "chat-completions"
auth_scheme = "bearer"
base_url = "http://{addr}"
chat_path = "/v1/chat/completions"
read_timeout_secs = 1
"#
    ))
    .expect("profile");
    assert_eq!(profile.http.read_timeout_secs, Some(1));
    let client =
        WireClient::from_resolved(profile, AnyTokenProvider::from(StaticToken::new("sk-test")))
            .expect("client");
    let err = client.send(simple_ir("gpt-4")).await.expect_err("timeout");
    let _ = handle.join();
    match err {
        ClientError::Transient {
            ref message, kind, ..
        } => {
            assert_eq!(kind, TransientKind::Timeout);
            assert!(err.is_timeout());
            assert!(!err.is_connect());
            let lower = message.to_ascii_lowercase();
            assert!(
                lower.contains("timed out") || lower.contains("timeout"),
                "read timeout Transient must name timeout, got {message}"
            );
            assert!(
                !lower.contains("error trying to connect") && !lower.contains("failed to connect"),
                "read timeout must not look like connect abort, got {message}"
            );
        }
        other => panic!("expected Transient timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn closed_port_transient_names_connect() {
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr")
    };
    let client = WireClient::from_resolved(
        chat_profile(&format!("http://{addr}")),
        AnyTokenProvider::from(StaticToken::new("sk-test")),
    )
    .expect("client");
    let err = client
        .send(simple_ir("gpt-4"))
        .await
        .expect_err("closed port");
    match err {
        ClientError::Transient {
            ref message, kind, ..
        } => {
            assert_eq!(kind, TransientKind::Connect);
            assert!(err.is_connect());
            assert!(!err.is_timeout());
            let lower = message.to_ascii_lowercase();
            assert!(
                lower.contains("connect") || lower.contains("connection refused"),
                "closed-port Transient must name connect, got {message}"
            );
            let display = err.to_string();
            assert!(
                display.starts_with("transient:"),
                "Display must stay 0.7.0-shaped, got {display}"
            );
            assert!(
                !display.to_ascii_lowercase().contains("kind"),
                "Display must not name TransientKind, got {display}"
            );
        }
        other => panic!("expected Transient connect, got {other:?}"),
    }
}

#[tokio::test]
async fn from_resolved_with_client_uses_host_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = read_http_request(&mut stream);
        thread::sleep(Duration::from_secs(3));
        write_http(&mut stream, 200, "OK", "", &complete_chat_body());
        req
    });
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(1))
        .build()
        .expect("host client");
    let client = WireClient::from_resolved_with_client(
        chat_profile(&format!("http://{addr}")),
        AnyTokenProvider::from(StaticToken::new("sk-test")),
        http,
    )
    .expect("client");
    let err = client
        .send(simple_ir("gpt-4"))
        .await
        .expect_err("host timeout");
    let _ = handle.join();
    match err {
        ClientError::Transient { kind, .. } => {
            assert_eq!(kind, TransientKind::Timeout);
            assert!(err.is_timeout());
            assert!(!err.is_connect());
        }
        other => panic!("expected Transient, got {other:?}"),
    }
}

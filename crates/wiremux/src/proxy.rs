//! Optional local HTTP proxy: source dialect -> IR -> profile target.

use std::convert::Infallible;
use std::io::Write;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use wiremux_auth::{AuthScheme, ResolvedProfile, Wire, format_oauth_transport_error};

use serde_json::Value;

use crate::cli::{parse_listen, proxy_token, upstream_url_for_model};
use crate::ir::{IrStreamEvent, LossReport};
use crate::map::{decode, encode};
use crate::stream::{
    RawSse, SseFrameReader, ToolCallAssembler, decode_stream_events, encode_stream_event, from_chat,
};

type ProxyBody = UnsyncBoxBody<Bytes, Infallible>;

const MAX_BODY: usize = 8 * 1024 * 1024;

/// Bind 127.0.0.1 and serve until the process is signaled.
pub async fn run(
    listen: &str,
    from: Wire,
    profile: ResolvedProfile,
    dump_loss: bool,
) -> Result<(), String> {
    let addr = parse_listen(listen)?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    if !bound.ip().is_loopback() {
        return Err("refusing to serve on a non-loopback address".into());
    }
    println!("listening on {bound}");
    let _ = std::io::stdout().flush();

    let state = Arc::new(ProxyState {
        from,
        profile,
        dump_loss,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?,
    });

    loop {
        let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |req| {
                let state = Arc::clone(&state);
                async move { handle(state, req).await }
            });
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .await
            {
                eprintln!("proxy conn: {err}");
            }
        });
    }
}

struct ProxyState {
    from: Wire,
    profile: ResolvedProfile,
    dump_loss: bool,
    client: reqwest::Client,
}

async fn handle(
    state: Arc<ProxyState>,
    req: Request<Incoming>,
) -> Result<Response<ProxyBody>, Infallible> {
    Ok(handle_inner(state, req).await)
}

async fn handle_inner(state: Arc<ProxyState>, req: Request<Incoming>) -> Response<ProxyBody> {
    if req.method() == Method::GET && matches!(req.uri().path(), "/" | "/health" | "/healthz") {
        return text(StatusCode::OK, "ok\n");
    }
    if req.method() != Method::POST {
        return text(StatusCode::METHOD_NOT_ALLOWED, "POST required\n");
    }

    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let collected = match req.collect().await {
        Ok(c) => c.to_bytes(),
        Err(err) => return text(StatusCode::BAD_REQUEST, format!("read body: {err}\n")),
    };
    if collected.len() > MAX_BODY {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "request body too large\n");
    }

    let (ir, dec_loss) = match decode(state.from, &collected) {
        Ok(v) => v,
        Err(err) => return text(StatusCode::BAD_REQUEST, format!("{err}\n")),
    };
    let target = match state.profile.dialect.wire {
        Some(w) => w,
        None => {
            return text(
                StatusCode::BAD_REQUEST,
                "profile has no wire; cannot encode\n",
            );
        }
    };
    let (encoded, enc_loss) = match encode(target, &ir, &state.profile) {
        Ok(v) => v,
        Err(err) => return text(StatusCode::BAD_REQUEST, format!("{err}\n")),
    };
    if state.dump_loss {
        eprintln!("loss.decode: {dec_loss:?}");
        eprintln!("loss.encode: {enc_loss:?}");
    }

    let url = match upstream_url_for_model(
        &state.profile,
        Some(ir.model.as_str()),
        ir.sampling.stream == Some(true),
    ) {
        Ok(u) => u,
        Err(err) => return text(StatusCode::BAD_GATEWAY, format!("{err}\n")),
    };
    let token = match proxy_token(&state.profile).await {
        Ok(t) => t,
        Err(err) => return text(StatusCode::UNAUTHORIZED, format!("{err}\n")),
    };

    let mut upstream = state.client.post(&url).body(encoded);
    upstream = apply_profile_headers(upstream, &state.profile, token.as_deref());

    let resp = match upstream.send().await {
        Ok(r) => r,
        Err(err) => {
            return text(
                StatusCode::BAD_GATEWAY,
                format!("{}\n", format_oauth_transport_error("upstream", &err, &url)),
            );
        }
    };
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let loss_summary = loss_summary(&dec_loss, &enc_loss);
    eprintln!(
        "{method} {path} profile={} upstream={} loss={loss_summary}",
        state.profile.id,
        status.as_u16()
    );

    if content_type.contains("text/event-stream") {
        return map_sse_stream(state, target, status_from_reqwest(status), resp);
    }
    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(err) => {
            return text(
                StatusCode::BAD_GATEWAY,
                format!(
                    "{}\n",
                    format_oauth_transport_error("upstream body", &err, &url)
                ),
            );
        }
    };
    if ir.sampling.stream == Some(true)
        && status.is_success()
        && let Some(sse) = json_completion_to_sse(state.from, &body)
    {
        return bytes_response(status_from_reqwest(status), "text/event-stream", sse);
    }
    if target == state.from {
        return bytes_response(status_from_reqwest(status), &content_type, body);
    }
    text(
        StatusCode::NOT_IMPLEMENTED,
        "non-stream cross-dialect responses are not mapped\n",
    )
}

/// Grok (and other always-SSE clients) still get SSE when the upstream
/// ignored `stream: true` and returned a JSON completion.
fn json_completion_to_sse(from: Wire, body: &Bytes) -> Option<Bytes> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let tools = chat_tool_calls(&value);
    let text = assistant_text(from, &value);
    if tools.is_empty() && text.is_none() {
        return None;
    }
    let mut events = Vec::new();
    if let Some(text) = text {
        events.push(IrStreamEvent::TextDelta { text });
    }
    for (id, name, args) in tools {
        events.push(IrStreamEvent::ToolCallStart {
            id,
            name,
            thought_signature: None,
        });
        if !args.is_empty() {
            events.push(IrStreamEvent::ToolCallArgDelta { delta: args });
        }
        events.push(IrStreamEvent::ToolCallEnd);
    }
    let reason = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if events
                .iter()
                .any(|ev| matches!(ev, IrStreamEvent::ToolCallStart { .. }))
            {
                "tool_calls".into()
            } else {
                "stop".into()
            }
        });
    events.push(IrStreamEvent::FinishReason { reason });
    if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
        events.push(from_chat(usage));
    }
    events.push(IrStreamEvent::Done);
    let mut out = String::new();
    for ev in events {
        let raw = encode_stream_event(from, &ev).ok()?;
        out.push_str(&format_sse(&raw));
    }
    Some(Bytes::from(out))
}

fn chat_tool_calls(value: &Value) -> Vec<(String, String, String)> {
    let Some(calls) = value
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let func = call.get("function").unwrap_or(call);
            let name = func
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())?;
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let args = func
                .get("arguments")
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default();
            Some((id, name.to_owned(), args))
        })
        .collect()
}

fn assistant_text(wire: Wire, value: &Value) -> Option<String> {
    match wire {
        Wire::ChatCompletions => value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        Wire::Messages => {
            if let Some(s) = value.get("content").and_then(Value::as_str) {
                return Some(s.to_owned()).filter(|t| !t.is_empty());
            }
            let blocks = value.get("content").and_then(Value::as_array)?;
            let text: String = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        Wire::Gemini => {
            let parts = value
                .pointer("/candidates/0/content/parts")
                .and_then(Value::as_array)?;
            let text: String = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        Wire::Responses => {
            if let Some(s) = value
                .pointer("/output_text")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Some(s.to_owned());
            }
            let output = value.get("output").and_then(Value::as_array)?;
            let mut text = String::new();
            for item in output {
                if let Some(s) = item.get("text").and_then(Value::as_str) {
                    text.push_str(s);
                    continue;
                }
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        if let Some(s) = part.get("text").and_then(Value::as_str) {
                            text.push_str(s);
                        }
                    }
                }
            }
            (!text.is_empty()).then_some(text)
        }
    }
}

fn apply_profile_headers(
    mut req: reqwest::RequestBuilder,
    profile: &ResolvedProfile,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    for (name, value) in &profile.http.headers {
        if name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key") {
            continue;
        }
        req = req.header(name, value);
    }
    if let Some(fp) = &profile.fingerprint {
        if let Some(ua) = fp.user_agent.as_deref() {
            req = req.header("user-agent", ua);
        }
        if let Some(app) = fp.x_app.as_deref() {
            req = req.header("x-app", app);
        }
    }
    if !profile.betas.values.is_empty() {
        req = req.header(&profile.betas.header, profile.betas.values.join(","));
    }
    if let Some(token) = token {
        match profile
            .http
            .auth_scheme
            .clone()
            .unwrap_or(AuthScheme::Bearer)
        {
            AuthScheme::None => {}
            AuthScheme::Bearer => {
                req = req.header("authorization", format!("Bearer {token}"));
            }
            AuthScheme::XApiKey => {
                req = req.header("x-api-key", token);
            }
            AuthScheme::Header(name) => {
                req = req.header(name, token);
            }
        }
    }
    req
}

fn map_sse_stream(
    state: Arc<ProxyState>,
    target: Wire,
    status: StatusCode,
    resp: reqwest::Response,
) -> Response<ProxyBody> {
    let url = resp.url().to_string();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(16);
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut reader = SseFrameReader::new();
        let mut assembler = ToolCallAssembler::new();
        while let Some(item) = stream.next().await {
            let bytes = match item {
                Ok(b) => b,
                Err(err) => {
                    let _ = tx
                        .send(Ok(Frame::data(Bytes::from(format!(
                            "{}\n",
                            format_oauth_transport_error("upstream stream", &err, &url)
                        )))))
                        .await;
                    return;
                }
            };
            let frames = match reader.feed(&bytes) {
                Ok(f) => f,
                Err(err) => {
                    let _ = tx
                        .send(Ok(Frame::data(Bytes::from(format!("{err}\n")))))
                        .await;
                    return;
                }
            };
            if !push_mapped_frames(&state, target, &tx, frames, &mut assembler).await {
                return;
            }
        }
        if let Some(last) = reader.drain() {
            let _ = push_mapped_frames(&state, target, &tx, vec![last], &mut assembler).await;
        }
        for ev in assembler.flush() {
            if let Ok(mapped) = encode_stream_event(state.from, &ev) {
                let _ = tx
                    .send(Ok(Frame::data(Bytes::from(format_sse(&mapped)))))
                    .await;
            }
        }
    });
    let body_stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Response::builder()
        .status(status)
        .header("content-type", "text/event-stream")
        .body(StreamBody::new(body_stream).boxed_unsync())
        .unwrap_or_else(|_| Response::new(boxed_full("{}\n")))
}

async fn push_mapped_frames(
    state: &ProxyState,
    target: Wire,
    tx: &tokio::sync::mpsc::Sender<Result<Frame<Bytes>, Infallible>>,
    frames: Vec<RawSse>,
    assembler: &mut ToolCallAssembler,
) -> bool {
    for raw in frames {
        match decode_stream_events(target, &raw, &state.profile) {
            Ok(events) => {
                for ev in events.into_iter().flat_map(|ev| assembler.push(ev)) {
                    match encode_stream_event(state.from, &ev) {
                        Ok(mapped) => {
                            if tx
                                .send(Ok(Frame::data(Bytes::from(format_sse(&mapped)))))
                                .await
                                .is_err()
                            {
                                return false;
                            }
                        }
                        Err(err) => {
                            let _ = tx
                                .send(Ok(Frame::data(Bytes::from(format!(
                                    "encode stream: {err}\n"
                                )))))
                                .await;
                            return false;
                        }
                    }
                }
            }
            Err(err) => {
                let _ = tx
                    .send(Ok(Frame::data(Bytes::from(format!(
                        "decode stream: {err}\n"
                    )))))
                    .await;
                return false;
            }
        }
    }
    true
}

fn boxed_full(bytes: impl Into<Bytes>) -> ProxyBody {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn format_sse(raw: &RawSse) -> String {
    let mut out = String::new();
    if let Some(event) = &raw.event {
        out.push_str("event: ");
        out.push_str(event);
        out.push('\n');
    }
    for line in raw.data.split('\n') {
        out.push_str("data: ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out
}

fn loss_summary(decode: &LossReport, encode: &LossReport) -> String {
    format!("dec={} enc={}", decode.events.len(), encode.events.len())
}

fn text(status: StatusCode, body: impl Into<String>) -> Response<ProxyBody> {
    let body = body.into();
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(boxed_full(body))
        .unwrap_or_else(|_| Response::new(boxed_full("error\n")))
}

fn bytes_response(status: StatusCode, content_type: &str, body: Bytes) -> Response<ProxyBody> {
    let ct = if content_type.is_empty() {
        "application/json"
    } else {
        content_type
    };
    Response::builder()
        .status(status)
        .header("content-type", ct)
        .body(boxed_full(body))
        .unwrap_or_else(|_| Response::new(boxed_full("{}\n")))
}

fn status_from_reqwest(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
}

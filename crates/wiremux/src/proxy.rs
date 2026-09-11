//! Optional local HTTP proxy: source dialect -> IR -> profile target.

use std::convert::Infallible;
use std::io::Write;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use wiremux_auth::{AuthScheme, ResolvedProfile, Wire};

use serde_json::Value;

use crate::cli::{parse_listen, proxy_token, upstream_url};
use crate::ir::{IrStreamEvent, LossReport};
use crate::map::{decode, encode};
use crate::stream::{RawSse, decode_stream_event, encode_stream_event};

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
            .timeout(std::time::Duration::from_secs(120))
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
) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(handle_inner(state, req).await)
}

async fn handle_inner(state: Arc<ProxyState>, req: Request<Incoming>) -> Response<Full<Bytes>> {
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

    let url = match upstream_url(&state.profile) {
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
        Err(err) => return text(StatusCode::BAD_GATEWAY, format!("upstream: {err}\n")),
    };
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(err) => return text(StatusCode::BAD_GATEWAY, format!("upstream body: {err}\n")),
    };

    let loss_summary = loss_summary(&dec_loss, &enc_loss);
    eprintln!(
        "{method} {path} profile={} upstream={} loss={loss_summary}",
        state.profile.id,
        status.as_u16()
    );

    if content_type.contains("text/event-stream") {
        // Thin proxy buffers the upstream SSE then remaps. Forward status
        // so a 4xx/5xx stream is not rewritten as 200.
        return map_sse(&state, target, status_from_reqwest(status), &body);
    }
    if ir.sampling.stream == Some(true)
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
    let text = assistant_text(from, &value)?;
    let mut out = String::new();
    let events = [
        IrStreamEvent::TextDelta { text },
        IrStreamEvent::FinishReason {
            reason: "stop".into(),
        },
        IrStreamEvent::Done,
    ];
    for ev in events {
        let raw = encode_stream_event(from, &ev).ok()?;
        out.push_str(&format_sse(&raw));
    }
    Some(Bytes::from(out))
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

fn map_sse(
    state: &ProxyState,
    target: Wire,
    status: StatusCode,
    body: &Bytes,
) -> Response<Full<Bytes>> {
    let payload = String::from_utf8_lossy(body);
    let frames = RawSse::parse_all(&payload);
    let mut out = String::new();
    for raw in frames {
        match decode_stream_event(target, &raw, &state.profile) {
            Ok(Some(ev)) => match encode_stream_event(state.from, &ev) {
                Ok(mapped) => out.push_str(&format_sse(&mapped)),
                Err(err) => {
                    return text(StatusCode::BAD_GATEWAY, format!("encode stream: {err}\n"));
                }
            },
            Ok(None) => {}
            Err(err) => {
                return text(StatusCode::BAD_GATEWAY, format!("decode stream: {err}\n"));
            }
        }
    }
    bytes_response(status, "text/event-stream", Bytes::from(out))
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

fn text(status: StatusCode, body: impl Into<String>) -> Response<Full<Bytes>> {
    let body = body.into();
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::from_static(b"error\n"))))
}

fn bytes_response(status: StatusCode, content_type: &str, body: Bytes) -> Response<Full<Bytes>> {
    let ct = if content_type.is_empty() {
        "application/json"
    } else {
        content_type
    };
    Response::builder()
        .status(status)
        .header("content-type", ct)
        .body(Full::new(body))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::from_static(b"{}\n"))))
}

fn status_from_reqwest(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
}

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
use wiremux_auth::{
    AnyTokenProvider, ResolvedProfile, StaticToken, TokenProvider, Wire,
    format_oauth_transport_error, provider_from_profile,
};

use crate::aws_sign::{apply_aws_sigv4, bearer_token_applied};
use crate::cli::{parse_listen, proxy_token};
use crate::headers::{apply_profile_headers, apply_provider_headers};
use crate::ir::LossReport;
use crate::map::{decode, encode};
use crate::stream::{
    RawSse, StreamEncoder, ToolCallAssembler, UpstreamFrames, decode_response,
    decode_stream_events, encode_response, event_has_slot,
};
use crate::upstream::upstream_url_for_model;

type ProxyBody = UnsyncBoxBody<Bytes, Infallible>;

const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_UPSTREAM_BODY: usize = 16 * 1024 * 1024;

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

    let provider = match provider_from_profile(&profile) {
        Ok(p) => Ok(p),
        Err(err) => {
            if profile
                .http
                .aws_service
                .as_deref()
                .is_some_and(|s| !s.is_empty())
                || matches!(
                    profile.http.auth_scheme,
                    Some(wiremux_auth::AuthScheme::None)
                )
            {
                Ok(AnyTokenProvider::from(StaticToken::new("")))
            } else {
                Err(err.to_string())
            }
        }
    };
    let client = crate::headers::default_http_client(&profile)?;
    let state = Arc::new(ProxyState {
        from,
        profile,
        dump_loss,
        provider,
        client,
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
    provider: Result<AnyTokenProvider, String>,
    client: reqwest::Client,
}

async fn handle(
    state: Arc<ProxyState>,
    req: Request<Incoming>,
) -> Result<Response<ProxyBody>, Infallible> {
    Ok(handle_inner(state, req).await)
}

async fn resolve_proxy_token(state: &ProxyState) -> Result<Option<String>, String> {
    match &state.provider {
        Ok(provider) => proxy_token(&state.profile, provider).await,
        Err(err) => {
            if state
                .profile
                .http
                .aws_service
                .as_deref()
                .is_some_and(|s| !s.is_empty())
            {
                Ok(None)
            } else {
                Err(err.clone())
            }
        }
    }
}

async fn send_upstream(
    state: &ProxyState,
    url: &str,
    encoded: &[u8],
) -> Result<reqwest::Response, Response<ProxyBody>> {
    let mut retried = false;
    loop {
        let token = match resolve_proxy_token(state).await {
            Ok(t) => t,
            Err(err) => return Err(text(StatusCode::UNAUTHORIZED, format!("{err}\n"))),
        };
        let mut upstream = state
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(encoded.to_vec());
        if url.contains("/converse-stream") {
            upstream = upstream.header("accept", "application/vnd.amazon.eventstream");
        }
        upstream = apply_profile_headers(upstream, &state.profile, token.as_deref());
        if let Ok(provider) = &state.provider {
            upstream = apply_provider_headers(upstream, &state.profile, provider);
        }
        let built = match apply_aws_sigv4(
            &state.profile,
            url,
            "POST",
            encoded,
            upstream,
            bearer_token_applied(&state.profile, token.as_deref()),
        )
        .await
        {
            Ok(req) => req,
            Err(crate::aws_sign::AwsSignError::Auth(err)) => {
                return Err(text(StatusCode::UNAUTHORIZED, format!("{err}\n")));
            }
            Err(crate::aws_sign::AwsSignError::Transport(err)) => {
                return Err(text(StatusCode::BAD_GATEWAY, format!("{err}\n")));
            }
        };
        let resp = match state.client.execute(built).await {
            Ok(r) => r,
            Err(err) => {
                return Err(text(
                    StatusCode::BAD_GATEWAY,
                    format!("{}\n", format_oauth_transport_error("upstream", &err, url)),
                ));
            }
        };
        if resp.status().as_u16() == 401
            && !retried
            && state
                .provider
                .as_ref()
                .is_ok_and(AnyTokenProvider::can_refresh)
        {
            if let Ok(provider) = &state.provider {
                provider.mark_stale();
            }
            retried = true;
            continue;
        }
        return Ok(resp);
    }
}

/// Loopback Host only. Rejects DNS-rebinding names, trailing dots,
/// decimal IPs, and unbracketed IPv6 except the exact `::1` token.
fn host_is_loopback(host: &str) -> bool {
    let host = host.trim();
    if host.is_empty() {
        return false;
    }
    if host == "::1" {
        return true;
    }
    if let Some(rest) = host.strip_prefix('[') {
        let Some((addr, after)) = rest.split_once(']') else {
            return false;
        };
        if addr != "::1" {
            return false;
        }
        return match after.strip_prefix(':') {
            Some(port) => !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()),
            None => after.is_empty(),
        };
    }
    let name = match host.rsplit_once(':') {
        Some((name, port))
            if !name.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => host,
    };
    name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost")
}

fn is_json_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json")
}

fn request_guard(req: &Request<Incoming>, post: bool) -> Option<Response<ProxyBody>> {
    let host = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !host_is_loopback(host) {
        return Some(text(StatusCode::FORBIDDEN, "forbidden host\n"));
    }
    if req.headers().contains_key(hyper::header::ORIGIN) {
        return Some(text(StatusCode::FORBIDDEN, "forbidden origin\n"));
    }
    if let Some(site) = req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        && site.eq_ignore_ascii_case("cross-site")
    {
        return Some(text(StatusCode::FORBIDDEN, "forbidden site\n"));
    }
    if post
        && !is_json_content_type(
            req.headers()
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
        )
    {
        return Some(text(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json required\n"));
    }
    None
}

async fn handle_inner(state: Arc<ProxyState>, req: Request<Incoming>) -> Response<ProxyBody> {
    let is_post = req.method() == Method::POST;
    if let Some(denied) = request_guard(&req, is_post) {
        return denied;
    }
    if req.method() == Method::GET && matches!(req.uri().path(), "/" | "/health" | "/healthz") {
        return text(StatusCode::OK, "ok\n");
    }
    if req.method() != Method::POST {
        return text(StatusCode::METHOD_NOT_ALLOWED, "POST required\n");
    }

    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    if let Some(len) = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        && len > MAX_BODY as u64
    {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "request body too large\n");
    }
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
    let resp = match send_upstream(&state, &url, &encoded).await {
        Ok(r) => r,
        Err(resp) => return resp,
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

    if content_type.contains("text/event-stream") || content_type.contains("eventstream") {
        let status = status_from_reqwest(status);
        if target == state.from {
            return passthrough_sse(status, resp);
        }
        return map_sse_stream(state, target, status, resp);
    }
    let body = match read_capped_upstream(resp, MAX_UPSTREAM_BODY).await {
        Ok(b) => b,
        Err(err) => {
            return text(StatusCode::BAD_GATEWAY, format!("{err}\n"));
        }
    };
    if ir.sampling.stream == Some(true)
        && status.is_success()
        && let Some(sse) = json_completion_to_sse(state.from, target, &body, &state.profile)
    {
        return bytes_response(status_from_reqwest(status), "text/event-stream", sse);
    }
    if target == state.from || !status.is_success() {
        return bytes_response(status_from_reqwest(status), &content_type, body);
    }
    match decode_response(target, &body, &state.profile) {
        Ok(events) => {
            if let Ok(mapped) = encode_response(state.from, &events) {
                let bytes = Bytes::from(mapped.to_string());
                return bytes_response(status_from_reqwest(status), "application/json", bytes);
            }
        }
        Err(err) => {
            return text(
                StatusCode::BAD_GATEWAY,
                format!("decode upstream body: {err}\n"),
            );
        }
    }
    text(
        StatusCode::NOT_IMPLEMENTED,
        "non-stream cross-dialect responses are not mapped\n",
    )
}

async fn read_capped_upstream(resp: reqwest::Response, cap: usize) -> Result<Bytes, String> {
    let url = resp.url().to_string();
    if let Some(len) = resp.content_length()
        && len > cap as u64
    {
        return Err(format!(
            "upstream body too large (Content-Length: {len} bytes)"
        ));
    }
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(item) = stream.next().await {
        let chunk =
            item.map_err(|err| format_oauth_transport_error("upstream body", &err, &url))?;
        if buf.len().saturating_add(chunk.len()) > cap {
            return Err(format!("upstream body exceeds {cap} bytes"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(buf))
}

/// Grok (and other always-SSE clients) still get SSE when the upstream
/// ignored `stream: true` and returned a JSON completion.
fn json_completion_to_sse(
    from: Wire,
    target: Wire,
    body: &Bytes,
    profile: &ResolvedProfile,
) -> Option<Bytes> {
    let events = decode_response(target, body, profile).ok()?;
    if events.is_empty() {
        return None;
    }
    let mut encoder = StreamEncoder::new(from);
    let mut out = String::new();
    let mut wrote = false;
    for ev in events {
        if !event_has_slot(from, &ev) {
            continue;
        }
        for raw in encoder.push(ev).ok()? {
            out.push_str(&format_sse(&raw));
            wrote = true;
        }
    }
    for raw in encoder.finish().ok()? {
        out.push_str(&format_sse(&raw));
        wrote = true;
    }
    wrote.then(|| Bytes::from(out))
}

fn passthrough_sse(status: StatusCode, resp: reqwest::Response) -> Response<ProxyBody> {
    let url = resp.url().to_string();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(16);
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        while let Some(item) = stream.next().await {
            match item {
                Ok(bytes) => {
                    if tx.send(Ok(Frame::data(bytes))).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx
                        .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                            event: Some("error".into()),
                            data: format_oauth_transport_error("upstream stream", &err, &url),
                        })))))
                        .await;
                    return;
                }
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
        let mut reader = UpstreamFrames::for_wire(target);
        let mut assembler = ToolCallAssembler::new();
        let mut encoder = StreamEncoder::new(state.from);
        while let Some(item) = stream.next().await {
            let bytes = match item {
                Ok(b) => b,
                Err(err) => {
                    let _ = tx
                        .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                            event: Some("error".into()),
                            data: format_oauth_transport_error("upstream stream", &err, &url),
                        })))))
                        .await;
                    return;
                }
            };
            let (frames, terminal) = match reader.feed(&bytes) {
                Ok(pair) => pair,
                Err(err) => {
                    let _ = tx
                        .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                            event: Some("error".into()),
                            data: err,
                        })))))
                        .await;
                    return;
                }
            };
            if !push_mapped_frames(&state, target, &tx, frames, &mut assembler, &mut encoder).await
            {
                return;
            }
            if let Some(err) = terminal {
                let _ = tx
                    .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                        event: Some("error".into()),
                        data: err,
                    })))))
                    .await;
                return;
            }
        }
        if let Some(last) = reader.drain() {
            let _ = push_mapped_frames(
                &state,
                target,
                &tx,
                vec![last],
                &mut assembler,
                &mut encoder,
            )
            .await;
        }
        for ev in assembler.flush() {
            if !event_has_slot(state.from, &ev) {
                continue;
            }
            match encoder.push(ev) {
                Ok(mapped) => {
                    for frame in mapped {
                        if tx
                            .send(Ok(Frame::data(Bytes::from(format_sse(&frame)))))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(err) => {
                    let _ = tx
                        .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                            event: Some("error".into()),
                            data: format!("encode stream: {err}"),
                        })))))
                        .await;
                    return;
                }
            }
        }
        match encoder.finish() {
            Ok(mapped) => {
                for frame in mapped {
                    if tx
                        .send(Ok(Frame::data(Bytes::from(format_sse(&frame)))))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(err) => {
                let _ = tx
                    .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                        event: Some("error".into()),
                        data: format!("encode stream: {err}"),
                    })))))
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
    encoder: &mut StreamEncoder,
) -> bool {
    for raw in frames {
        match decode_stream_events(target, &raw, &state.profile) {
            Ok(events) => {
                for ev in events.into_iter().flat_map(|ev| assembler.push(ev)) {
                    if !event_has_slot(state.from, &ev) {
                        continue;
                    }
                    match encoder.push(ev) {
                        Ok(mapped) => {
                            for frame in mapped {
                                if tx
                                    .send(Ok(Frame::data(Bytes::from(format_sse(&frame)))))
                                    .await
                                    .is_err()
                                {
                                    return false;
                                }
                            }
                        }
                        Err(err) => {
                            let _ = tx
                                .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                                    event: Some("error".into()),
                                    data: format!("encode stream: {err}"),
                                })))))
                                .await;
                            return false;
                        }
                    }
                }
            }
            Err(err) => {
                let _ = tx
                    .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                        event: Some("error".into()),
                        data: format!("decode stream: {err}"),
                    })))))
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

#[cfg(test)]
mod tests {
    use super::{host_is_loopback, is_json_content_type};

    #[test]
    fn loopback_hosts_accepted() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:0",
            "127.0.0.1:18789",
            "localhost",
            "LOCALHOST",
            "localhost:9",
            "[::1]",
            "[::1]:8080",
            "::1",
        ] {
            assert!(host_is_loopback(host), "{host}");
        }
    }

    #[test]
    fn non_loopback_hosts_rejected() {
        for host in [
            "",
            "evil.example",
            "127.0.0.1.evil.example",
            "localhost.",
            "127.0.0.1.",
            "0.0.0.0",
            "2130706433",
            "[fe80::1]",
            "127.0.0.1:abc",
            "user@127.0.0.1",
            "127.0.0.1:80:80",
        ] {
            assert!(!host_is_loopback(host), "{host}");
        }
    }

    #[test]
    fn json_content_type_allows_charset() {
        assert!(is_json_content_type("application/json"));
        assert!(is_json_content_type("Application/JSON; charset=utf-8"));
        assert!(!is_json_content_type("text/plain"));
        assert!(!is_json_content_type("application/jsonp"));
        assert!(!is_json_content_type(""));
    }
}

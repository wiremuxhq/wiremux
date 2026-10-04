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
use crate::ir::{LossAction, LossReport};
use crate::map::{decode, encode};
use crate::stream::{
    INCOMPLETE_STREAM_MESSAGE, RawSse, StreamDecoder, StreamEncoder, ToolCallAssembler,
    UpstreamFrames, decode_response, encode_eventstream_exception, encode_eventstream_message,
    encode_response_with_model, event_has_slot, frame_event_name, frame_is_terminal,
    sse_wrapped_error_message, unwrap_event_payload,
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
    model_override: Option<String>,
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
        model_override: model_override.filter(|model| !model.is_empty()),
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
    model_override: Option<String>,
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
    profile: &ResolvedProfile,
    anthropic_version: Option<&str>,
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
        upstream = apply_profile_headers(upstream, profile, token.as_deref());
        if let Some(version) = anthropic_version.filter(|value| !value.is_empty()) {
            upstream = upstream.header("anthropic-version", version);
        }
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

/// Same-wire success is passed through. An HTML status-200 page is not a
/// completion, so only a JSON object or array may take that path.
fn same_wire_success_is_json(content_type: &str, body: &[u8]) -> bool {
    if !content_type.is_empty() && !is_json_content_type(content_type) {
        return false;
    }
    let trimmed = body.trim_ascii_start();
    trimmed.starts_with(b"{") || trimmed.starts_with(b"[")
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
    if req.method() == Method::GET && req.uri().path() == "/v1/models" {
        return models_list(&state);
    }
    if req.method() != Method::POST {
        return text(StatusCode::METHOD_NOT_ALLOWED, "POST required\n");
    }

    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    if let Some(len) = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        && len > MAX_BODY as u64
    {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "request body too large\n");
    }
    let inbound_headers = req.headers().clone();
    let collected = match read_capped_body(req.into_body(), MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(err)
            if err
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some() =>
        {
            return text(StatusCode::PAYLOAD_TOO_LARGE, "request body too large\n");
        }
        Err(err) => return text(StatusCode::BAD_REQUEST, format!("read body: {err}\n")),
    };
    if collected.len() > MAX_BODY {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "request body too large\n");
    }

    let (mut ir, mut dec_loss) = match decode(state.from, &collected) {
        Ok(v) => v,
        Err(err) => return text(StatusCode::BAD_REQUEST, format!("{err}\n")),
    };
    if dest_stream_from_request(state.from, &path, &query) {
        ir.sampling.stream = Some(true);
    }
    if ir.model.is_empty()
        && let Some(model) = model_from_dest_path(state.from, &path)
    {
        ir.model = model;
    }
    if let Some(model) = state
        .model_override
        .as_deref()
        .filter(|model| !model.is_empty())
        && ir.model != model
    {
        dec_loss.record("model", LossAction::Degrade, "proxy --model");
        ir.model = model.to_string();
    }
    let target = match state.profile.dialect.wire {
        Some(w) => w,
        None => {
            return text(
                StatusCode::BAD_REQUEST,
                "profile has no wire; cannot encode\n",
            );
        }
    };
    // This request can extend betas. The shared profile stays unchanged.
    let mut header_profile = state.profile.clone();
    let anthropic_version =
        forward_inbound_headers(&mut header_profile, &mut dec_loss, target, &inbound_headers);
    let (encoded, enc_loss) = match encode(target, &ir, &state.profile) {
        Ok(v) => v,
        Err(err) => return text(StatusCode::BAD_REQUEST, format!("{err}\n")),
    };
    if state.dump_loss {
        for event in dec_loss.lossy() {
            eprintln!("loss.decode: {event}");
        }
        for event in enc_loss.lossy() {
            eprintln!("loss.encode: {event}");
        }
    }

    let url = match upstream_url_for_model(
        &state.profile,
        Some(ir.model.as_str()),
        ir.sampling.stream == Some(true),
    ) {
        Ok(u) => u,
        Err(err) => return text(StatusCode::BAD_GATEWAY, format!("{err}\n")),
    };
    let resp = match send_upstream(
        &state,
        &header_profile,
        anthropic_version.as_deref(),
        &url,
        &encoded,
    )
    .await
    {
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
            return passthrough_sse(status, resp, &content_type);
        }
        return map_sse_stream(state, target, status, resp, ir.model.clone());
    }
    let body = match read_capped_upstream(resp, MAX_UPSTREAM_BODY).await {
        Ok(b) => b,
        Err(err) => {
            return text(StatusCode::BAD_GATEWAY, format!("{err}\n"));
        }
    };
    if status.is_success()
        && let Some(mapped) =
            cross_wire_vendor_failure(state.from, target, ir.sampling.stream == Some(true), &body)
    {
        let failure_type = if ir.sampling.stream == Some(true) {
            dest_stream_content_type(state.from)
        } else {
            "application/json"
        };
        return bytes_response(status_from_reqwest(status), failure_type, mapped);
    }
    if ir.sampling.stream == Some(true)
        && status.is_success()
        && let Some(sse) =
            json_completion_to_sse(state.from, target, &body, &state.profile, &ir.model)
    {
        return bytes_response(
            status_from_reqwest(status),
            dest_stream_content_type(state.from),
            sse,
        );
    }
    if target == state.from || !status.is_success() {
        if target == state.from
            && status.is_success()
            && !same_wire_success_is_json(&content_type, &body)
        {
            return text(
                StatusCode::BAD_GATEWAY,
                "upstream success body is not JSON\n",
            );
        }
        return bytes_response(status_from_reqwest(status), &content_type, body);
    }
    match decode_response(target, &body, &state.profile) {
        Ok(events) => {
            if let Ok(mapped) = encode_response_with_model(state.from, &events, &ir.model) {
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

fn models_list(state: &ProxyState) -> Response<ProxyBody> {
    let id = state
        .model_override
        .as_deref()
        .filter(|model| !model.is_empty())
        .unwrap_or(state.profile.id.as_str());
    let body = serde_json::json!({
        "object": "list",
        "data": [{
            "id": id,
            "object": "model",
            "owned_by": "wiremux"
        }]
    });
    bytes_response(
        StatusCode::OK,
        "application/json",
        Bytes::from(body.to_string()),
    )
}

fn forward_inbound_headers(
    profile: &mut ResolvedProfile,
    loss: &mut LossReport,
    target: Wire,
    headers: &hyper::HeaderMap,
) -> Option<String> {
    let messages = target == Wire::Messages;
    let profile_has_version = profile
        .http
        .headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("anthropic-version"));
    let mut extra_betas = Vec::new();
    let mut version = None;
    for (name, value) in headers.iter() {
        let name = name.as_str();
        if matches!(
            name,
            "host" | "content-type" | "content-length" | "accept" | "connection" | "authorization"
        ) {
            continue;
        }
        if messages && name.eq_ignore_ascii_case("anthropic-beta") {
            let mut took = false;
            if let Ok(raw) = value.to_str() {
                for token in raw.split(',') {
                    let token = token.trim();
                    if !token.is_empty() {
                        extra_betas.push(token.to_string());
                        took = true;
                    }
                }
            }
            if took {
                continue;
            }
        }
        if messages
            && name.eq_ignore_ascii_case("anthropic-version")
            && !profile_has_version
            && version.is_none()
            && let Ok(raw) = value.to_str()
        {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                version = Some(trimmed.to_string());
                continue;
            }
        }
        loss.record(
            format!("header.{}", name.to_ascii_lowercase()),
            LossAction::Drop,
            "not forwarded",
        );
    }
    if messages && !extra_betas.is_empty() {
        if profile.betas.header.trim().is_empty() {
            profile.betas.header = "anthropic-beta".to_string();
        }
        // A profile that only sets [headers] anthropic-beta must keep
        // that token when the client sends its own beta list.
        if profile.betas.values.is_empty() {
            let header_name = profile.betas.header.clone();
            let seeded: Vec<String> = profile
                .http
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&header_name))
                .map(|(_, value)| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|token| !token.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            profile.betas.values.extend(seeded);
        }
        for token in extra_betas {
            if !profile
                .betas
                .values
                .iter()
                .any(|existing| existing == &token)
            {
                profile.betas.values.push(token);
            }
        }
    }
    version
}

async fn read_capped_body<B>(
    body: B,
    cap: usize,
) -> Result<Bytes, Box<dyn std::error::Error + Send + Sync>>
where
    B: hyper::body::Body,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let limited = http_body_util::Limited::new(body, cap);
    Ok(limited.collect().await?.to_bytes())
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
    model: &str,
) -> Option<Bytes> {
    let events = decode_response(target, body, profile).ok()?;
    if events.is_empty() {
        return None;
    }
    let mut encoder = StreamEncoder::new(from).with_model(model);
    let mut out = Vec::new();
    let mut wrote = false;
    for ev in events {
        if !event_has_slot(from, &ev) {
            continue;
        }
        let Ok(frames) = encoder.push(ev) else {
            continue;
        };
        for raw in frames {
            out.extend_from_slice(&dest_frame_bytes(from, &raw));
            wrote = true;
        }
    }
    if let Ok(frames) = encoder.finish() {
        for raw in frames {
            out.extend_from_slice(&dest_frame_bytes(from, &raw));
            wrote = true;
        }
    }
    wrote.then(|| Bytes::from(out))
}

fn passthrough_sse(
    status: StatusCode,
    resp: reqwest::Response,
    content_type: &str,
) -> Response<ProxyBody> {
    let url = resp.url().to_string();
    let eventstream = content_type.contains("eventstream");
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
                    let msg = format_oauth_transport_error("upstream stream", &err, &url);
                    eprintln!("{msg}");
                    if !eventstream {
                        let _ = tx
                            .send(Ok(Frame::data(Bytes::from(format_sse(&RawSse {
                                event: Some("error".into()),
                                data: msg,
                            })))))
                            .await;
                    }
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
        .header(
            "content-type",
            if content_type.is_empty() {
                "text/event-stream"
            } else {
                content_type
            },
        )
        .body(StreamBody::new(body_stream).boxed_unsync())
        .unwrap_or_else(|_| Response::new(boxed_full("{}\n")))
}

struct MappedStream {
    from: Wire,
    target: Wire,
    profile: wiremux_auth::ResolvedProfile,
    decoder: StreamDecoder,
    assembler: ToolCallAssembler,
    encoder: StreamEncoder,
    saw_frame: bool,
    saw_terminal: bool,
}

impl MappedStream {
    fn new(
        from: Wire,
        target: Wire,
        profile: wiremux_auth::ResolvedProfile,
        model: String,
    ) -> Self {
        Self {
            from,
            target,
            profile,
            decoder: StreamDecoder::new(),
            assembler: ToolCallAssembler::new(),
            encoder: StreamEncoder::new(from).with_model(model),
            saw_frame: false,
            saw_terminal: false,
        }
    }

    /// Encoded frames, then an error from a later frame in this batch.
    ///
    /// Earlier frames stay in the `Vec` so a vendor `error` event does
    /// not drop text that arrived in the same read.
    fn push_frames(&mut self, frames: Vec<RawSse>) -> (Vec<RawSse>, Option<String>) {
        let mut out = Vec::new();
        for raw in frames {
            self.saw_frame = true;
            if frame_is_terminal(self.target, &raw, &self.profile) {
                self.saw_terminal = true;
            }
            if let Some(msg) = sse_wrapped_error_message(&raw.data) {
                return (out, Some(msg));
            }
            let events = match self.decoder.decode(self.target, &raw, &self.profile) {
                Ok(events) => events,
                Err(err) => {
                    // `response.failed` is already `code: message`. A
                    // `decode stream:` prefix hides that code from
                    // `dest_error_bytes`, so Chat sees `server_error`.
                    let shown = if self.target == Wire::Responses
                        && frame_event_name(self.target, &raw) == "response.failed"
                    {
                        err.to_string()
                    } else {
                        format!("decode stream: {err}")
                    };
                    return (out, Some(shown));
                }
            };
            for ev in events.into_iter().flat_map(|ev| self.assembler.push(ev)) {
                if !event_has_slot(self.from, &ev) {
                    continue;
                }
                match self.encoder.push(ev) {
                    Ok(frames) => out.extend(frames),
                    Err(err) => return (out, Some(format!("encode stream: {err}"))),
                }
            }
        }
        (out, None)
    }

    fn take_assembler_tail(&mut self) -> Result<Vec<RawSse>, String> {
        let mut out = Vec::new();
        for ev in self.assembler.flush() {
            if !event_has_slot(self.from, &ev) {
                continue;
            }
            out.extend(
                self.encoder
                    .push(ev)
                    .map_err(|err| format!("encode stream: {err}"))?,
            );
        }
        Ok(out)
    }

    fn finish_encoder(&mut self) -> Result<Vec<RawSse>, String> {
        if self.saw_frame && !self.saw_terminal {
            return Err(INCOMPLETE_STREAM_MESSAGE.to_string());
        }
        self.encoder
            .finish()
            .map_err(|err| format!("encode stream: {err}"))
    }
}

fn map_sse_stream(
    state: Arc<ProxyState>,
    target: Wire,
    status: StatusCode,
    resp: reqwest::Response,
    model: String,
) -> Response<ProxyBody> {
    let url = resp.url().to_string();
    let dest_ct = dest_stream_content_type(state.from);
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(16);
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut reader = UpstreamFrames::for_wire(target);
        let mut mapped = MappedStream::new(state.from, target, state.profile.clone(), model);
        while let Some(item) = stream.next().await {
            let bytes = match item {
                Ok(b) => b,
                Err(err) => {
                    let _ = tx
                        .send(Ok(Frame::data(dest_error_bytes(
                            state.from,
                            format_oauth_transport_error("upstream stream", &err, &url),
                        ))))
                        .await;
                    return;
                }
            };
            let (frames, terminal) = match reader.feed(&bytes) {
                Ok(pair) => pair,
                Err(err) => {
                    let _ = tx
                        .send(Ok(Frame::data(dest_error_bytes(state.from, err))))
                        .await;
                    return;
                }
            };
            if !emit_mapped(&state, &tx, &mut mapped, frames).await {
                return;
            }
            if let Some(err) = terminal {
                let _ = tx
                    .send(Ok(Frame::data(dest_error_bytes(state.from, err))))
                    .await;
                return;
            }
        }
        match reader.finish() {
            Ok(Some(last)) => {
                if !emit_mapped(&state, &tx, &mut mapped, vec![last]).await {
                    return;
                }
            }
            Ok(None) => {}
            Err(err) => {
                let _ = tx
                    .send(Ok(Frame::data(dest_error_bytes(state.from, err))))
                    .await;
                return;
            }
        }
        match mapped.take_assembler_tail() {
            Ok(frames) => {
                if !send_frames(&state, &tx, frames).await {
                    return;
                }
            }
            Err(err) => {
                let _ = tx
                    .send(Ok(Frame::data(dest_error_bytes(state.from, err))))
                    .await;
                return;
            }
        }
        match mapped.finish_encoder() {
            Ok(frames) => {
                let _ = send_frames(&state, &tx, frames).await;
            }
            Err(err) => {
                let _ = tx
                    .send(Ok(Frame::data(dest_error_bytes(state.from, err))))
                    .await;
            }
        }
    });
    let body_stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Response::builder()
        .status(status)
        .header("content-type", dest_ct)
        .body(StreamBody::new(body_stream).boxed_unsync())
        .unwrap_or_else(|_| Response::new(boxed_full("{}\n")))
}

async fn emit_mapped(
    state: &ProxyState,
    tx: &tokio::sync::mpsc::Sender<Result<Frame<Bytes>, Infallible>>,
    mapped: &mut MappedStream,
    frames: Vec<RawSse>,
) -> bool {
    let (out, err) = mapped.push_frames(frames);
    if !send_frames(state, tx, out).await {
        return false;
    }
    if let Some(err) = err {
        let _ = tx
            .send(Ok(Frame::data(dest_error_bytes(state.from, err))))
            .await;
        return false;
    }
    true
}

async fn send_frames(
    state: &ProxyState,
    tx: &tokio::sync::mpsc::Sender<Result<Frame<Bytes>, Infallible>>,
    frames: Vec<RawSse>,
) -> bool {
    for frame in frames {
        if tx
            .send(Ok(Frame::data(dest_frame_bytes(state.from, &frame))))
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
fn map_sse_bytes(
    from: Wire,
    target: Wire,
    profile: &wiremux_auth::ResolvedProfile,
    model: &str,
    bytes: &[u8],
) -> Vec<u8> {
    let mut reader = UpstreamFrames::for_wire(target);
    let mut mapped = MappedStream::new(from, target, profile.clone(), model.to_string());
    let mut out = Vec::new();
    let push_one = |mapped: &mut MappedStream, frames: Vec<RawSse>, out: &mut Vec<u8>| {
        let (encoded, err) = mapped.push_frames(frames);
        for frame in encoded {
            out.extend(dest_frame_bytes(from, &frame));
        }
        if let Some(err) = err {
            out.extend(dest_error_bytes(from, err));
            return Err(());
        }
        Ok(())
    };
    match reader.feed(bytes) {
        Ok((frames, terminal)) => {
            if push_one(&mut mapped, frames, &mut out).is_err() {
                return out;
            }
            if let Some(err) = terminal {
                out.extend(dest_error_bytes(from, err));
                return out;
            }
        }
        Err(err) => {
            out.extend(dest_error_bytes(from, err));
            return out;
        }
    }
    match reader.finish() {
        Ok(Some(last)) => {
            if push_one(&mut mapped, vec![last], &mut out).is_err() {
                return out;
            }
        }
        Ok(None) => {}
        Err(err) => {
            out.extend(dest_error_bytes(from, err));
            return out;
        }
    }
    match mapped.take_assembler_tail() {
        Ok(frames) => {
            for frame in frames {
                out.extend(dest_frame_bytes(from, &frame));
            }
        }
        Err(err) => {
            out.extend(dest_error_bytes(from, err));
            return out;
        }
    }
    match mapped.finish_encoder() {
        Ok(frames) => {
            for frame in frames {
                out.extend(dest_frame_bytes(from, &frame));
            }
        }
        Err(err) => out.extend(dest_error_bytes(from, err)),
    }
    out
}

fn boxed_full(bytes: impl Into<Bytes>) -> ProxyBody {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn model_from_dest_path(from: Wire, path: &str) -> Option<String> {
    match from {
        Wire::Gemini => {
            let rest = path.split("/models/").nth(1)?;
            let model = rest
                .strip_suffix(":streamGenerateContent")
                .or_else(|| rest.strip_suffix(":generateContent"))
                .unwrap_or(rest);
            let model = model.split('?').next().unwrap_or(model);
            (!model.is_empty()).then(|| model.to_string())
        }
        Wire::Converse => {
            let rest = path.strip_prefix("/model/")?;
            let model = rest
                .strip_suffix("/converse-stream")
                .or_else(|| rest.strip_suffix("/converse"))?;
            (!model.is_empty()).then(|| model.to_string())
        }
        Wire::Messages => {
            let rest = path.split("/models/").nth(1)?;
            let model = rest
                .strip_suffix(":streamRawPredict")
                .or_else(|| rest.strip_suffix(":rawPredict"))
                .unwrap_or(rest);
            let model = model.split('?').next().unwrap_or(model);
            (!model.is_empty()).then(|| model.to_string())
        }
        Wire::ChatCompletions => {
            let rest = path.strip_prefix("/openai/deployments/")?;
            let model = rest.strip_suffix("/chat/completions")?;
            let model = model.split('?').next().unwrap_or(model);
            (!model.is_empty() && !model.contains('/')).then(|| model.to_string())
        }
        _ => None,
    }
}

fn dest_stream_from_request(from: Wire, path: &str, query: &str) -> bool {
    match from {
        Wire::Converse => path.ends_with("/converse-stream"),
        Wire::Gemini => {
            path.ends_with(":streamGenerateContent")
                || query
                    .split('&')
                    .any(|part| part == "alt=sse" || part.starts_with("alt=sse"))
        }
        Wire::Messages => path.ends_with(":streamRawPredict"),
        _ => false,
    }
}

fn dest_stream_content_type(from: Wire) -> &'static str {
    if matches!(from, Wire::Converse) {
        "application/vnd.amazon.eventstream"
    } else {
        "text/event-stream"
    }
}

fn dest_frame_bytes(from: Wire, raw: &RawSse) -> Bytes {
    if matches!(from, Wire::Converse) {
        let event = frame_event_name(from, raw);
        Bytes::from(encode_eventstream_message(
            &event,
            &unwrap_event_payload(&event, &raw.data),
        ))
    } else {
        Bytes::from(format_sse(raw))
    }
}

fn split_error_prefix(msg: &str) -> (Option<&str>, &str) {
    match msg.split_once(": ") {
        Some((code, rest))
            if !code.is_empty()
                && !rest.is_empty()
                && code
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_') =>
        {
            (Some(code), rest)
        }
        _ => (None, msg),
    }
}

fn dest_error_json(from: Wire, msg: &str) -> serde_json::Value {
    if matches!(from, Wire::Converse) {
        return serde_json::json!({ "message": msg });
    }
    let (code, text) = split_error_prefix(msg);
    match from {
        Wire::Messages => {
            let ty = code.unwrap_or("api_error");
            serde_json::json!({
                "type": "error",
                "error": { "type": ty, "message": text }
            })
        }
        Wire::Responses => {
            let ty = code.unwrap_or("server_error");
            serde_json::json!({
                "type": "error",
                "message": text,
                "code": ty
            })
        }
        Wire::Gemini => {
            let status = code.unwrap_or("INTERNAL");
            serde_json::json!({
                "error": { "message": text, "status": status }
            })
        }
        _ => {
            let ty = code.unwrap_or("server_error");
            serde_json::json!({
                "error": { "type": ty, "message": text }
            })
        }
    }
}

fn dest_error_bytes(from: Wire, msg: String) -> Bytes {
    if matches!(from, Wire::Converse) {
        let payload = dest_error_json(from, &msg).to_string();
        return Bytes::from(encode_eventstream_exception(
            "internalServerException",
            payload.as_bytes(),
        ));
    }
    Bytes::from(format_sse(&RawSse {
        event: Some("error".into()),
        data: dest_error_json(from, &msg).to_string(),
    }))
}

/// Cross-wire JSON body whose vendor failure has no output text.
///
/// `wiremux map` still turns that body into finish_reason stop. A Chat
/// client behind the proxy would read the blank completion as success.
fn cross_wire_vendor_failure(from: Wire, target: Wire, stream: bool, body: &[u8]) -> Option<Bytes> {
    if target == from {
        return None;
    }
    let detail = responses_complete_vendor_failure(target, body)?;
    if stream {
        Some(dest_error_bytes(from, detail))
    } else {
        Some(Bytes::from(dest_error_json(from, &detail).to_string()))
    }
}

fn responses_complete_vendor_failure(target: Wire, body: &[u8]) -> Option<String> {
    if target != Wire::Responses {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let wrapped = value.get("type").and_then(serde_json::Value::as_str) == Some("response.failed");
    let response = if wrapped {
        value
            .get("response")
            .filter(|inner| inner.is_object())
            .unwrap_or(&value)
    } else {
        &value
    };
    let status = response.get("status").and_then(serde_json::Value::as_str);
    if status != Some("failed") {
        return None;
    }
    match response.get("output") {
        Some(serde_json::Value::Array(items)) if items.is_empty() => {}
        Some(serde_json::Value::Null) | None => {}
        _ => return None,
    }
    let err = ["error", "last_error"]
        .iter()
        .find_map(|key| response.get(*key).filter(|item| item.is_object()))?;
    let message = err
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let code = err.get("code").and_then(|code| {
        code.as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .or_else(|| code.as_i64().map(|n| n.to_string()))
    });
    match (code, message) {
        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
        (None, Some(message)) => Some(message.to_string()),
        (Some(code), None) => Some(code),
        (None, None) => None,
    }
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
    use super::{
        host_is_loopback, is_json_content_type, read_capped_body, same_wire_success_is_json,
    };

    #[test]
    fn messages_document_citation_reaches_messages_not_chat() {
        let profile =
            crate::parse_profile_str("schema_version = 1\nid = \"m\"\nwire = \"messages\"\n")
                .expect("profile");
        let sse = concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"citations_delta\",\"citation\":{\"type\":\"char_location\",\"cited_text\":\"The grass is green.\",\"document_index\":0,\"document_title\":\"My Document\",\"start_char_index\":0,\"end_char_index\":20}}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let same = super::map_sse_bytes(
            wiremux_auth::Wire::Messages,
            wiremux_auth::Wire::Messages,
            &profile,
            "claude",
            sse.as_bytes(),
        );
        let same_text = String::from_utf8(same).expect("utf8");
        assert!(
            same_text.contains("The grass is green."),
            "same-wire proxy must keep the document citation, got {same_text}"
        );
        assert!(
            same_text.contains("char_location"),
            "citation type must survive, got {same_text}"
        );

        let chat = super::map_sse_bytes(
            wiremux_auth::Wire::ChatCompletions,
            wiremux_auth::Wire::Messages,
            &profile,
            "gpt-4o",
            sse.as_bytes(),
        );
        let chat_text = String::from_utf8(chat).expect("utf8");
        assert!(
            !chat_text.contains("char_location") && !chat_text.contains("content_block_delta"),
            "Chat must not receive a Messages protocol frame, got {chat_text}"
        );
    }

    #[test]
    fn messages_text_stream_is_not_responses_output_items() {
        let profile =
            crate::parse_profile_str("schema_version = 1\nid = \"m\"\nwire = \"messages\"\n")
                .expect("profile");
        let sse = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mapped = super::map_sse_bytes(
            wiremux_auth::Wire::Responses,
            wiremux_auth::Wire::Messages,
            &profile,
            "gpt-5",
            sse.as_bytes(),
        );
        let text = String::from_utf8(mapped).expect("utf8");
        assert!(
            text.contains("hi"),
            "Responses client must still see the text, got {text}"
        );
        assert!(
            !text.contains("content_block_start")
                && !text.contains("content_block_stop")
                && !text.contains("server_tool_use"),
            "Responses client must not see Messages frames as output items, got {text}"
        );
    }

    #[test]
    fn inbound_beta_keeps_profile_header_beta() {
        let mut profile = crate::parse_profile_str(
            r#"
schema_version = 1
id = "m"
wire = "messages"
base_url = "http://127.0.0.1"
auth_scheme = "none"
[headers]
anthropic-beta = "context-1m-2025-08-07"
"#,
        )
        .expect("profile");
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            "fine-grained-tool-streaming-2025-05-14"
                .parse()
                .expect("header"),
        );
        let mut loss = crate::ir::LossReport::default();
        super::forward_inbound_headers(
            &mut profile,
            &mut loss,
            wiremux_auth::Wire::Messages,
            &headers,
        );
        let req = crate::headers::apply_profile_headers(
            reqwest::Client::new().request(reqwest::Method::POST, "http://127.0.0.1/v1/messages"),
            &profile,
            None,
        )
        .build()
        .expect("request");
        let value = req
            .headers()
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        assert!(
            value.contains("context-1m-2025-08-07"),
            "profile beta must survive the client beta, got {value}"
        );
        assert!(
            value.contains("fine-grained-tool-streaming-2025-05-14"),
            "client beta must be forwarded, got {value}"
        );
        assert_eq!(
            req.headers().get_all("anthropic-beta").iter().count(),
            1,
            "betas must be one header, got {:?}",
            req.headers().get_all("anthropic-beta")
        );
    }

    #[tokio::test]
    async fn capped_body_stops_when_the_next_chunk_crosses_the_cap() {
        use bytes::Bytes;
        use http_body_util::StreamBody;
        use hyper::body::Frame;

        let frames = futures_util::stream::iter(vec![
            Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from(vec![1; 3]))),
            Ok(Frame::data(Bytes::from(vec![2; 8]))),
        ]);
        let err = read_capped_body(StreamBody::new(frames), 4)
            .await
            .expect_err("over cap");
        assert!(
            err.downcast_ref::<http_body_util::LengthLimitError>()
                .is_some(),
            "{err}"
        );
    }

    #[test]
    fn html_success_is_not_a_same_wire_completion() {
        assert!(!same_wire_success_is_json(
            "text/html",
            b"<html><body>bad gateway page</body></html>"
        ));
        assert!(!same_wire_success_is_json(
            "application/json",
            b"<html></html>"
        ));
        assert!(same_wire_success_is_json(
            "application/json; charset=utf-8",
            b" {\"id\":\"x\"}"
        ));
        assert!(same_wire_success_is_json("", b"[1]"));
    }

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

    fn first_eventstream_payload(bytes: &[u8]) -> &[u8] {
        let total = u32::from_be_bytes(bytes[0..4].try_into().expect("total")) as usize;
        let headers_len = u32::from_be_bytes(bytes[4..8].try_into().expect("headers")) as usize;
        &bytes[12 + headers_len..total - 4]
    }

    #[test]
    fn dest_path_supplies_gemini_and_converse_model() {
        assert_eq!(
            super::model_from_dest_path(
                wiremux_auth::Wire::Gemini,
                "/v1beta/models/llama3.2:3b:generateContent"
            )
            .as_deref(),
            Some("llama3.2:3b")
        );
        assert_eq!(
            super::model_from_dest_path(
                wiremux_auth::Wire::Gemini,
                "/v1beta/models/gemini-2.5-flash:streamGenerateContent"
            )
            .as_deref(),
            Some("gemini-2.5-flash")
        );
        assert_eq!(
            super::model_from_dest_path(
                wiremux_auth::Wire::Converse,
                "/model/amazon.nova-lite-v1:0/converse"
            )
            .as_deref(),
            Some("amazon.nova-lite-v1:0")
        );
        assert_eq!(
            super::model_from_dest_path(
                wiremux_auth::Wire::Converse,
                "/model/amazon.nova-lite-v1:0/converse-stream"
            )
            .as_deref(),
            Some("amazon.nova-lite-v1:0")
        );
        assert_eq!(
            super::model_from_dest_path(
                wiremux_auth::Wire::ChatCompletions,
                "/v1/chat/completions"
            ),
            None
        );
        assert_eq!(
            super::model_from_dest_path(
                wiremux_auth::Wire::ChatCompletions,
                "/openai/deployments/gpt-4o/chat/completions"
            )
            .as_deref(),
            Some("gpt-4o")
        );
        assert_eq!(
            super::model_from_dest_path(
                wiremux_auth::Wire::Messages,
                "/v1/projects/p/locations/us-east5/publishers/anthropic/models/claude-sonnet-4:rawPredict"
            )
            .as_deref(),
            Some("claude-sonnet-4")
        );
        assert!(super::dest_stream_from_request(
            wiremux_auth::Wire::Gemini,
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent",
            ""
        ));
        assert!(super::dest_stream_from_request(
            wiremux_auth::Wire::Gemini,
            "/v1beta/models/gemini-2.5-flash:generateContent",
            "alt=sse"
        ));
        assert!(!super::dest_stream_from_request(
            wiremux_auth::Wire::Gemini,
            "/v1beta/models/gemini-2.5-flash:generateContent",
            ""
        ));
        assert!(super::dest_stream_from_request(
            wiremux_auth::Wire::Messages,
            "/v1/projects/p/locations/us-east5/publishers/anthropic/models/claude-sonnet-4:streamRawPredict",
            ""
        ));
    }

    #[test]
    fn dest_converse_frame_is_eventstream_not_sse() {
        let raw = crate::stream::RawSse {
            event: None,
            data: r#"{"contentBlockDelta":{"delta":{"text":"hi"}}}"#.into(),
        };
        let bytes = super::dest_frame_bytes(wiremux_auth::Wire::Converse, &raw);
        assert!(
            !bytes.starts_with(b"data:"),
            "dest Converse must not emit SSE, got {}",
            String::from_utf8_lossy(&bytes)
        );
        let payload = first_eventstream_payload(&bytes);
        let value: serde_json::Value = serde_json::from_slice(payload).expect("payload");
        assert!(
            value.get("contentBlockDelta").is_none(),
            "dest Event Stream payload must be the AWS member struct, got {value}"
        );
        assert_eq!(
            value
                .pointer("/delta/text")
                .and_then(serde_json::Value::as_str),
            Some("hi")
        );
        let mut reader = crate::stream::EventStreamReader::new();
        let (frames, err) = reader.feed(&bytes).expect("feed");
        assert!(err.is_none(), "{err:?}");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("contentBlockDelta"));
        assert!(frames[0].data.contains("hi"), "{}", frames[0].data);
        assert_eq!(
            super::dest_stream_content_type(wiremux_auth::Wire::Converse),
            "application/vnd.amazon.eventstream"
        );
        assert_eq!(
            super::dest_stream_content_type(wiremux_auth::Wire::ChatCompletions),
            "text/event-stream"
        );
    }

    #[test]
    fn json_content_type_allows_charset() {
        assert!(is_json_content_type("application/json"));
        assert!(is_json_content_type("Application/JSON; charset=utf-8"));
        assert!(!is_json_content_type("text/plain"));
        assert!(!is_json_content_type("application/jsonp"));
        assert!(!is_json_content_type(""));
    }

    #[test]
    fn proxy_error_frame_keeps_upstream_type() {
        let messages = super::dest_error_bytes(
            wiremux_auth::Wire::Messages,
            "overloaded_error: slow down".into(),
        );
        let text = String::from_utf8(messages.to_vec()).unwrap();
        let data = text
            .lines()
            .find(|line| line.starts_with("data:"))
            .expect("data line")
            .trim_start_matches("data:")
            .trim();
        let value: serde_json::Value = serde_json::from_str(data).expect("error data is JSON");
        assert_eq!(value["type"], "error", "{value}");
        assert_eq!(value["error"]["type"], "overloaded_error", "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("slow"),
            "{value}"
        );

        let chat = super::dest_error_bytes(
            wiremux_auth::Wire::ChatCompletions,
            "overloaded_error: slow down".into(),
        );
        let chat_text = String::from_utf8(chat.to_vec()).unwrap();
        let chat_data = chat_text
            .lines()
            .find(|line| line.starts_with("data:"))
            .expect("chat data")
            .trim_start_matches("data:")
            .trim();
        let chat_value: serde_json::Value =
            serde_json::from_str(chat_data).expect("chat error JSON");
        assert_eq!(
            chat_value["error"]["type"], "overloaded_error",
            "{chat_value}"
        );

        let responses = super::dest_error_bytes(
            wiremux_auth::Wire::Responses,
            "overloaded_error: slow down".into(),
        );
        let responses_text = String::from_utf8(responses.to_vec()).unwrap();
        let responses_data = responses_text
            .lines()
            .find(|line| line.starts_with("data:"))
            .expect("responses data")
            .trim_start_matches("data:")
            .trim();
        let responses_value: serde_json::Value =
            serde_json::from_str(responses_data).expect("responses error JSON");
        assert_eq!(responses_value["type"], "error", "{responses_value}");
        assert_eq!(
            responses_value["code"], "overloaded_error",
            "{responses_value}"
        );
        assert!(
            responses_value["message"]
                .as_str()
                .unwrap_or("")
                .contains("slow"),
            "{responses_value}"
        );

        let gemini = super::dest_error_bytes(
            wiremux_auth::Wire::Gemini,
            "overloaded_error: slow down".into(),
        );
        let gemini_text = String::from_utf8(gemini.to_vec()).unwrap();
        let gemini_data = gemini_text
            .lines()
            .find(|line| line.starts_with("data:"))
            .expect("gemini data")
            .trim_start_matches("data:")
            .trim();
        let gemini_value: serde_json::Value =
            serde_json::from_str(gemini_data).expect("gemini error JSON");
        assert_eq!(
            gemini_value["error"]["status"], "overloaded_error",
            "{gemini_value}"
        );
        assert!(
            gemini_value["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("slow"),
            "{gemini_value}"
        );

        let chat_plain =
            super::dest_error_bytes(wiremux_auth::Wire::ChatCompletions, "boom".into());
        let chat_plain_value = error_data(&chat_plain);
        assert_eq!(
            chat_plain_value["error"]["type"], "server_error",
            "{chat_plain_value}"
        );
        let responses_plain = super::dest_error_bytes(wiremux_auth::Wire::Responses, "boom".into());
        let responses_plain_value = error_data(&responses_plain);
        assert_eq!(
            responses_plain_value["code"], "server_error",
            "{responses_plain_value}"
        );
        let messages_plain = super::dest_error_bytes(wiremux_auth::Wire::Messages, "boom".into());
        let messages_plain_value = error_data(&messages_plain);
        assert_eq!(
            messages_plain_value["error"]["type"], "api_error",
            "{messages_plain_value}"
        );
        let gemini_plain = super::dest_error_bytes(wiremux_auth::Wire::Gemini, "boom".into());
        let gemini_plain_value = error_data(&gemini_plain);
        assert_eq!(
            gemini_plain_value["error"]["status"], "INTERNAL",
            "{gemini_plain_value}"
        );

        let converse = super::dest_error_bytes(wiremux_auth::Wire::Converse, "boom".into());
        assert!(
            !converse.starts_with(b"data:"),
            "dest Converse errors stay Event Stream, got {}",
            String::from_utf8_lossy(&converse)
        );
    }

    fn error_data(bytes: &bytes::Bytes) -> serde_json::Value {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let data = text
            .lines()
            .find(|line| line.starts_with("data:"))
            .expect("data line")
            .trim_start_matches("data:")
            .trim();
        serde_json::from_str(data).expect("error JSON")
    }

    #[test]
    fn chat_error_object_proxied_to_messages_keeps_type() {
        let profile = crate::parse_profile_str(
            "schema_version = 1\nid = \"err\"\nwire = \"chat-completions\"\n",
        )
        .expect("profile");
        let sse =
            "data: {\"error\":{\"message\":\"overloaded\",\"type\":\"overloaded_error\"}}\n\n";
        let bytes = super::map_sse_bytes(
            wiremux_auth::Wire::Messages,
            wiremux_auth::Wire::ChatCompletions,
            &profile,
            "claude",
            sse.as_bytes(),
        );
        let text = String::from_utf8(bytes).expect("utf8");
        let value = error_data(&bytes::Bytes::from(text.clone()));
        assert_eq!(value["error"]["type"], "overloaded_error", "{text}");
        assert_eq!(value["error"]["message"], "overloaded", "{text}");

        let chat = super::map_sse_bytes(
            wiremux_auth::Wire::ChatCompletions,
            wiremux_auth::Wire::ChatCompletions,
            &profile,
            "gpt-4o",
            sse.as_bytes(),
        );
        let chat_text = String::from_utf8(chat.clone()).expect("utf8");
        let chat_value = error_data(&bytes::Bytes::from(chat));
        assert_eq!(
            chat_value["error"]["type"], "overloaded_error",
            "{chat_text}"
        );
        assert!(chat_text.contains("data: {"), "{chat_text}");
    }

    #[test]
    fn incomplete_chat_sse_does_not_emit_finish_reason() {
        let profile = crate::parse_profile_str(
            "schema_version = 1\nid = \"chat-eof\"\nwire = \"chat-completions\"\n",
        )
        .expect("profile");
        let sse = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let bytes = super::map_sse_bytes(
            wiremux_auth::Wire::ChatCompletions,
            wiremux_auth::Wire::ChatCompletions,
            &profile,
            "gpt-4o",
            sse.as_bytes(),
        );
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.contains("hi"), "{text}");
        assert!(
            !text.contains("finish_reason"),
            "incomplete chat SSE must not become a successful finish, got {text}"
        );
        assert!(!text.contains("[DONE]"), "{text}");
    }

    #[test]
    fn responses_error_after_text_reaches_chat() {
        let profile =
            crate::parse_profile_str("schema_version = 1\nid = \"resp\"\nwire = \"responses\"\n")
                .expect("profile");
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"The server had an error\",\"param\":null}\n\n",
        );
        let bytes = super::map_sse_bytes(
            wiremux_auth::Wire::ChatCompletions,
            wiremux_auth::Wire::Responses,
            &profile,
            "gpt-4o",
            sse.as_bytes(),
        );
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.contains("Hi"), "{text}");
        assert!(text.contains("The server had an error"), "{text}");
        assert!(!text.contains("unknown stream event"), "{text}");
    }

    #[test]
    fn responses_failed_after_text_keeps_vendor_code() {
        let profile =
            crate::parse_profile_str("schema_version = 1\nid = \"resp\"\nwire = \"responses\"\n")
                .expect("profile");
        let official = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"please wait\"}}}\n\n",
        );
        assert_failed_vendor_code(&profile, official);
        let older = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"last_error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"please wait\"}}}\n\n",
        );
        assert_failed_vendor_code(&profile, older);
        let message_only = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"last_error\":{\"message\":\"The model is currently at capacity due to high demand.\"}}}\n\n",
        );
        let bytes = super::map_sse_bytes(
            wiremux_auth::Wire::ChatCompletions,
            wiremux_auth::Wire::Responses,
            &profile,
            "gpt-4o",
            message_only.as_bytes(),
        );
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.contains("Hi"), "{text}");
        assert!(text.contains("at capacity due to high demand"), "{text}");
        assert!(!text.contains("decode stream"), "{text}");
    }

    fn assert_failed_vendor_code(profile: &wiremux_auth::ResolvedProfile, sse: &str) {
        let bytes = super::map_sse_bytes(
            wiremux_auth::Wire::ChatCompletions,
            wiremux_auth::Wire::Responses,
            profile,
            "gpt-4o",
            sse.as_bytes(),
        );
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.contains("Hi"), "{text}");
        assert!(
            !text.contains("decode stream"),
            "vendor failure must not look like a decode bug, got {text}"
        );
        assert!(!text.contains("[DONE]"), "{text}");
        let data = text
            .lines()
            .find(|line| line.starts_with("data:") && line.contains("\"error\""))
            .expect("error data")
            .trim_start_matches("data:")
            .trim();
        let value: serde_json::Value = serde_json::from_str(data).expect("error json");
        assert_eq!(value["error"]["type"], "rate_limit_exceeded", "{text}");
        assert_eq!(value["error"]["message"], "please wait", "{text}");
    }

    #[test]
    fn cross_wire_failed_response_is_dest_error() {
        let body = br#"{"id":"resp_1","object":"response","status":"failed","error":{"code":"rate_limit_exceeded","message":"please wait"},"output":[]}"#;
        let chat = wiremux_auth::Wire::ChatCompletions;
        let responses = wiremux_auth::Wire::Responses;
        let bytes = super::cross_wire_vendor_failure(chat, responses, false, body)
            .expect("empty failed response is an error");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["error"]["type"], "rate_limit_exceeded");
        assert_eq!(value["error"]["message"], "please wait");
        assert!(value.get("choices").is_none());

        let older = br#"{"status":"failed","last_error":{"code":"rate_limit_exceeded","message":"please wait"}}"#;
        let value: serde_json::Value = serde_json::from_slice(
            &super::cross_wire_vendor_failure(chat, responses, false, older).expect("last_error"),
        )
        .expect("json");
        assert_eq!(value["error"]["type"], "rate_limit_exceeded");
        assert_eq!(value["error"]["message"], "please wait");

        let message_only = br#"{"status":"failed","error":{"message":"The model is currently at capacity due to high demand."},"output":[]}"#;
        let text = String::from_utf8(
            super::cross_wire_vendor_failure(chat, responses, false, message_only)
                .expect("message only")
                .to_vec(),
        )
        .expect("utf8");
        assert!(text.contains("at capacity due to high demand"), "{text}");
        assert!(!text.contains("decode"), "{text}");

        let partial = br#"{"status":"failed","error":{"code":"server_error","message":"nope"},"output":[{"type":"message","content":[{"type":"output_text","text":"Hi"}]}]}"#;
        assert!(
            super::cross_wire_vendor_failure(chat, responses, false, partial).is_none(),
            "partial text stays on the map path"
        );
        assert!(
            super::cross_wire_vendor_failure(responses, responses, false, body).is_none(),
            "same-wire stays a passthrough"
        );
        let empty_err = br#"{"status":"failed","error":{},"output":[]}"#;
        assert!(super::cross_wire_vendor_failure(chat, responses, false, empty_err).is_none());
        let null_output = br#"{"status":"failed","error":{"code":"rate_limit_exceeded","message":"please wait"},"output":null}"#;
        let value: serde_json::Value = serde_json::from_slice(
            &super::cross_wire_vendor_failure(chat, responses, false, null_output)
                .expect("null output is still no text")
                .to_vec(),
        )
        .expect("json");
        assert_eq!(value["error"]["type"], "rate_limit_exceeded");
        assert_eq!(value["error"]["message"], "please wait");

        let text = String::from_utf8(
            super::cross_wire_vendor_failure(chat, responses, true, body)
                .expect("stream framing")
                .to_vec(),
        )
        .expect("utf8");
        assert!(text.starts_with("event: error\n"), "{text}");
        assert!(text.contains("rate_limit_exceeded"), "{text}");
        assert!(!text.contains("[DONE]"), "{text}");

        let wrapped = br#"{"type":"response.failed","response":{"status":"failed","error":{"code":"rate_limit_exceeded","message":"please wait"},"output":[]}}"#;
        let value: serde_json::Value = serde_json::from_slice(
            &super::cross_wire_vendor_failure(
                wiremux_auth::Wire::Messages,
                responses,
                false,
                wrapped,
            )
            .expect("wrapped event"),
        )
        .expect("json");
        assert_eq!(value["type"], "error");
        assert_eq!(value["error"]["type"], "rate_limit_exceeded");
        assert_eq!(value["error"]["message"], "please wait");
    }
}

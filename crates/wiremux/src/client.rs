//! Optional library POST/SSE client. Feature `client` (not default).

use std::collections::VecDeque;
use std::fmt;
use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use wiremux_auth::{
    AnyTokenProvider, AuthError, LoadOptions, ProfileError, ResolvedProfile, TokenProvider, Wire,
    load_profile, not_found_message, provider_for_profile_opts, redact_secret_looking,
    redact_url_origin, sanitize_oauth_error_text,
};

use crate::aws_sign::{apply_aws_sigv4, bearer_token_applied};
use crate::headers::{apply_profile_headers, apply_provider_headers};
use crate::ir::{IrRequest, IrStreamEvent, LossReport};
use crate::map::{MapError, encode};
use crate::stream::{ToolCallAssembler, UpstreamFrames, decode_response, decode_stream_events};
use crate::upstream::upstream_url_for_model;

const MAX_SUCCESS_BODY: usize = 16 * 1024 * 1024;
const MAX_ERROR_BODY: usize = 64 * 1024;

/// Why [`ClientError::Transient`] fired. Hosts match this instead of
/// scraping Display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransientKind {
    /// Never reached the host (closed port, DNS, SYN timeout).
    Connect,
    /// Reached the host, then timed out (read / hung-after-accept).
    Timeout,
    /// Connection reset or broken pipe after connect.
    Reset,
    /// Leftover transient: HTTP 408/5xx, empty stream, 200-wrapped
    /// overload, or a send failure that is not connect, timeout, or reset.
    Http,
}

/// Matchable HTTP / map / transport failure. Display and Debug redact secrets.
#[non_exhaustive]
pub enum ClientError {
    /// HTTP 401, or 400/403 whose body looks like a bad or missing key.
    Auth {
        /// HTTP status when the vendor responded.
        status: Option<u16>,
        /// Redacted on Display.
        message: String,
    },
    /// HTTP 404 (except `list_models`), or a body that says the model is unknown.
    NotFound {
        /// HTTP status when the vendor responded.
        status: Option<u16>,
        /// Redacted on Display.
        message: String,
    },
    /// HTTP 429. `retry_after` is seconds when `Retry-After` is numeric.
    RateLimit {
        /// HTTP status when the vendor responded.
        status: Option<u16>,
        /// Parsed `Retry-After` in seconds.
        retry_after: Option<u64>,
        /// Redacted on Display.
        message: String,
    },
    /// Connect/timeout/reset, HTTP 408/5xx, or a 200-wrapped overload error.
    Transient {
        /// HTTP status when the vendor responded.
        status: Option<u16>,
        /// Redacted on Display.
        message: String,
        /// Connect vs timeout vs HTTP. Not shown on Display.
        kind: TransientKind,
    },
    /// Other 4xx (validation, region-forbidden 403).
    Vendor {
        /// HTTP status when the vendor responded.
        status: Option<u16>,
        /// Redacted on Display.
        message: String,
    },
    /// Dialect map failure.
    Map(MapError),
    /// reqwest build, or a body read after status classification.
    Transport(String),
}

impl fmt::Debug for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth { status, message } => f
                .debug_struct("Auth")
                .field("status", status)
                .field("message", &redact_client_text(message))
                .finish(),
            Self::NotFound { status, message } => f
                .debug_struct("NotFound")
                .field("status", status)
                .field("message", &redact_client_text(message))
                .finish(),
            Self::RateLimit {
                status,
                retry_after,
                message,
            } => f
                .debug_struct("RateLimit")
                .field("status", status)
                .field("retry_after", retry_after)
                .field("message", &redact_client_text(message))
                .finish(),
            Self::Transient {
                status,
                message,
                kind,
            } => f
                .debug_struct("Transient")
                .field("status", status)
                .field("message", &redact_client_text(message))
                .field("kind", kind)
                .finish(),
            Self::Vendor { status, message } => f
                .debug_struct("Vendor")
                .field("status", status)
                .field("message", &redact_client_text(message))
                .finish(),
            Self::Map(err) => f
                .debug_tuple("Map")
                .field(&redact_client_text(&err.to_string()))
                .finish(),
            Self::Transport(message) => f
                .debug_tuple("Transport")
                .field(&redact_client_text(message))
                .finish(),
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = match self {
            Self::Auth { status, message } => format_status("auth", *status, message),
            Self::NotFound { status, message } => format_status("not found", *status, message),
            Self::RateLimit {
                status,
                retry_after,
                message,
            } => match (status, retry_after) {
                (Some(s), Some(ra)) => {
                    format!("rate limit (HTTP {s}, retry-after {ra}s): {message}")
                }
                (Some(s), None) => format!("rate limit (HTTP {s}): {message}"),
                (None, Some(ra)) => format!("rate limit (retry-after {ra}s): {message}"),
                (None, None) => format!("rate limit: {message}"),
            },
            Self::Transient {
                status, message, ..
            } => format_status("transient", *status, message),
            Self::Vendor { status, message } => format_status("vendor", *status, message),
            Self::Map(err) => err.to_string(),
            Self::Transport(message) => message.clone(),
        };
        f.write_str(&redact_client_text(&raw))
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Map(err) => Some(err),
            _ => None,
        }
    }
}

impl From<MapError> for ClientError {
    fn from(err: MapError) -> Self {
        Self::Map(err)
    }
}

impl ClientError {
    /// Closed port, DNS, or SYN timeout. Not a hung-after-accept read.
    pub fn is_connect(&self) -> bool {
        matches!(
            self,
            Self::Transient {
                kind: TransientKind::Connect,
                ..
            }
        )
    }

    /// Read / hung-after-accept timeout. Not [`Self::is_connect`].
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Self::Transient {
                kind: TransientKind::Timeout,
                ..
            }
        )
    }

    /// Connection reset or broken pipe after the host was reached.
    /// Not [`Self::is_connect`]: do not abort a probe suite.
    pub fn is_reset(&self) -> bool {
        matches!(
            self,
            Self::Transient {
                kind: TransientKind::Reset,
                ..
            }
        )
    }
}

fn transient(status: Option<u16>, message: impl Into<String>, kind: TransientKind) -> ClientError {
    ClientError::Transient {
        status,
        message: message.into(),
        kind,
    }
}

fn format_status(kind: &str, status: Option<u16>, message: &str) -> String {
    match status {
        Some(s) => format!("{kind} (HTTP {s}): {message}"),
        None => format!("{kind}: {message}"),
    }
}

fn redact_client_text(s: &str) -> String {
    let mut out = redact_secret_looking(s);
    out = redact_prefixed(&out, "gsk_");
    out = redact_prefixed(&out, "xai-");
    out = redact_bearer(&out);
    redact_embedded_url(&out)
}

fn redact_prefixed(s: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find(prefix) {
        out.push_str(&rest[..idx]);
        out.push_str("[redacted]");
        rest = &rest[idx + prefix.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn redact_bearer(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(idx) = rest
            .find("Bearer ")
            .or_else(|| rest.find("bearer "))
            .or_else(|| rest.find("BEARER "))
        else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..idx]);
        out.push_str(&rest[idx..idx + "Bearer ".len()]);
        rest = &rest[idx + "Bearer ".len()..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end > 0 {
            out.push_str("[redacted]");
            rest = &rest[end..];
        }
    }
    out
}

/// One row from the models catalog. OpenAI-compat uses the chat version prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListedModel {
    /// Vendor model id. Gemini `name` drops a leading `models/` prefix.
    pub id: String,
    /// `context_length`, Anthropic `max_input_tokens`, Grok Build
    /// `context_window`, or Gemini `inputTokenLimit`.
    pub context_tokens: Option<u32>,
    /// True when `architecture.input_modalities` includes `image` or `vision`.
    pub vision: Option<bool>,
}

/// HTTP POST/SSE client bound to one resolved profile and token provider.
#[derive(Clone)]
pub struct WireClient {
    http: reqwest::Client,
    profile: ResolvedProfile,
    provider: AnyTokenProvider,
}

impl fmt::Debug for WireClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireClient")
            .field("id", &self.profile.id)
            .field("base_url", &self.profile.http.base_url)
            .finish_non_exhaustive()
    }
}

impl WireClient {
    /// Load a catalog id with default [`LoadOptions`].
    pub fn from_profile(id: &str) -> Result<Self, ClientError> {
        Self::from_profile_opts(id, &LoadOptions::default())
    }

    /// Load a catalog id, then [`provider_for_profile_opts`].
    pub fn from_profile_opts(id: &str, opts: &LoadOptions<'_>) -> Result<Self, ClientError> {
        let profile = load_profile(id, opts).map_err(profile_err)?;
        let provider = provider_for_profile_opts(id, opts).map_err(auth_err)?;
        Self::from_resolved(profile, provider)
    }

    /// Bind an already-resolved profile and provider.
    pub fn from_resolved(
        profile: ResolvedProfile,
        provider: AnyTokenProvider,
    ) -> Result<Self, ClientError> {
        let http = crate::headers::default_http_client(&profile).map_err(ClientError::Transport)?;
        Self::from_resolved_with_client(profile, provider, http)
    }

    /// Same as [`Self::from_resolved`], with a host-supplied HTTP client.
    pub fn from_resolved_with_client(
        profile: ResolvedProfile,
        provider: AnyTokenProvider,
        client: reqwest::Client,
    ) -> Result<Self, ClientError> {
        Ok(Self {
            http: client,
            profile,
            provider,
        })
    }

    /// Encode, POST a complete body, decode via [`decode_response`].
    pub async fn send(
        &self,
        mut ir: IrRequest,
    ) -> Result<(Vec<IrStreamEvent>, LossReport), ClientError> {
        ir.sampling.stream = Some(false);
        let wire = profile_wire(&self.profile)?;
        let (encoded, loss) = encode(wire, &ir, &self.profile)?;
        let url = upstream_url_for_model(&self.profile, Some(ir.model.as_str()), false)
            .map_err(ClientError::Transport)?;
        let resp = self.post_json_auth_retry(&url, encoded).await?;
        let status = resp.status().as_u16();
        let retry_after = retry_after_secs(&resp);
        let success = resp.status().is_success();
        let cap = if success {
            MAX_SUCCESS_BODY
        } else {
            MAX_ERROR_BODY
        };
        let body = read_body(resp, cap, success).await?;
        let text = String::from_utf8_lossy(&body);
        if let Some(err) = classify_http(status, &text, retry_after) {
            return Err(err);
        }
        let events = decode_response(wire, &body, &self.profile)?;
        Ok((events, loss))
    }

    /// POST with the dialect stream flag and remap SSE frames incrementally.
    pub fn stream(
        &self,
        ir: IrRequest,
    ) -> impl Stream<Item = Result<IrStreamEvent, ClientError>> + Send {
        let client = self.clone();
        futures_util::stream::unfold(
            StreamPhase::Start {
                client,
                ir: Box::new(ir),
            },
            |phase| async move { step_stream(phase).await },
        )
    }

    /// GET the models catalog. OpenAI-compat uses the chat version prefix
    /// (`{base}/v1/models` for `/v1/chat/completions`). HTTP 404 is an empty
    /// list. HTTP 401 is [`ClientError::Auth`].
    pub async fn list_models(&self) -> Result<Vec<ListedModel>, ClientError> {
        let base = self
            .profile
            .http
            .base_url
            .as_deref()
            .ok_or_else(|| ClientError::Transport(crate::upstream::MISSING_BASE_URL.into()))?;
        let chat_path = self
            .profile
            .http
            .chat_path
            .as_deref()
            .or_else(|| self.profile.dialect.wire.map(Wire::default_chat_path))
            .unwrap_or("");
        let url = models_url(base, chat_path);
        let token = self.access_token().await?;
        let resp = self
            .get_url(&url, token.as_deref())
            .await
            .map_err(classify_send_err)?;
        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(Vec::new());
        }
        let retry_after = retry_after_secs(&resp);
        let success = resp.status().is_success();
        let cap = if success {
            MAX_SUCCESS_BODY
        } else {
            MAX_ERROR_BODY
        };
        let body = read_body(resp, cap, success).await?;
        let text = String::from_utf8_lossy(&body);
        if status == 401 {
            return Err(ClientError::Auth {
                status: Some(401),
                message: error_message(&text),
            });
        }
        if let Some(err) = classify_http(status, &text, retry_after) {
            return Err(err);
        }
        let mut models = parse_listed_models(&body)?;
        if allows_ollama_show(base) {
            for model in &mut models {
                if let Some((context, vision)) = self.ollama_show(&model.id).await? {
                    if model.context_tokens.is_none() {
                        model.context_tokens = context;
                    }
                    if model.vision.is_none() {
                        model.vision = vision;
                    }
                }
            }
        }
        Ok(models)
    }

    async fn access_token(&self) -> Result<Option<String>, ClientError> {
        let token = self.provider.get_token().await.map_err(auth_err)?;
        if token.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(token))
        }
    }

    async fn post_json(
        &self,
        url: &str,
        body: Vec<u8>,
        token: Option<&str>,
    ) -> Result<reqwest::Response, ClientError> {
        let req = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
        let req = apply_profile_headers(req, &self.profile, token);
        let req = apply_provider_headers(req, &self.profile, &self.provider);
        let req = if url.contains("/converse-stream") {
            req.header("accept", "application/vnd.amazon.eventstream")
        } else {
            req
        };
        let built = apply_aws_sigv4(
            &self.profile,
            url,
            "POST",
            &body,
            req,
            bearer_token_applied(&self.profile, token),
        )
        .await
        .map_err(aws_sign_err)?;
        self.http.execute(built).await.map_err(classify_send_err)
    }

    async fn post_json_auth_retry(
        &self,
        url: &str,
        body: Vec<u8>,
    ) -> Result<reqwest::Response, ClientError> {
        let token = self.access_token().await?;
        let resp = self.post_json(url, body.clone(), token.as_deref()).await?;
        if resp.status().as_u16() != 401 || !self.provider.can_refresh() {
            return Ok(resp);
        }
        drop(resp);
        self.provider.mark_stale();
        let token = self.access_token().await?;
        self.post_json(url, body, token.as_deref()).await
    }

    async fn get_url(
        &self,
        url: &str,
        token: Option<&str>,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let req = self.http.get(url);
        let req = apply_profile_headers(req, &self.profile, token);
        apply_provider_headers(req, &self.profile, &self.provider)
            .send()
            .await
    }

    async fn ollama_show(
        &self,
        name: &str,
    ) -> Result<Option<(Option<u32>, Option<bool>)>, ClientError> {
        let base = self
            .profile
            .http
            .base_url
            .as_deref()
            .ok_or_else(|| ClientError::Transport(crate::upstream::MISSING_BASE_URL.into()))?;
        let url = format!("{}/api/show", base.trim_end_matches('/'));
        let token = self.access_token().await?;
        let payload = serde_json::json!({ "name": name }).to_string().into_bytes();
        let resp = match self.post_json(&url, payload, token.as_deref()).await {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        if !resp.status().is_success() {
            return Ok(None);
        }
        let body = match read_body(resp, MAX_SUCCESS_BODY, true).await {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let value: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        Ok(Some((context_from_show(&value), vision_from_show(&value))))
    }

    async fn open_stream(&self, mut ir: IrRequest) -> Result<LiveStream, ClientError> {
        ir.sampling.stream = Some(true);
        let wire = profile_wire(&self.profile)?;
        let (encoded, _loss) = encode(wire, &ir, &self.profile)?;
        let url = upstream_url_for_model(&self.profile, Some(ir.model.as_str()), true)
            .map_err(ClientError::Transport)?;
        let resp = self.post_json_auth_retry(&url, encoded).await?;
        let status = resp.status().as_u16();
        let retry_after = retry_after_secs(&resp);
        if !resp.status().is_success() {
            let body = read_body(resp, MAX_ERROR_BODY, false).await?;
            let text = String::from_utf8_lossy(&body);
            return Err(
                classify_http(status, &text, retry_after).unwrap_or_else(|| ClientError::Vendor {
                    status: Some(status),
                    message: error_message(&text),
                }),
            );
        }
        let bytes: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>> =
            Box::pin(resp.bytes_stream());
        Ok(LiveStream {
            bytes,
            reader: UpstreamFrames::for_wire(wire),
            assembler: ToolCallAssembler::new(),
            pending: VecDeque::new(),
            wire,
            profile: self.profile.clone(),
            eof: false,
            saw_frame: false,
            leftover: Vec::new(),
            http_status: status,
        })
    }
}

enum StreamPhase {
    Start {
        client: WireClient,
        ir: Box<IrRequest>,
    },
    Live(LiveStream),
    Done,
}

struct LiveStream {
    bytes: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    reader: UpstreamFrames,
    assembler: ToolCallAssembler,
    pending: VecDeque<IrStreamEvent>,
    wire: Wire,
    profile: ResolvedProfile,
    eof: bool,
    saw_frame: bool,
    leftover: Vec<u8>,
    http_status: u16,
}

impl LiveStream {
    fn push_frames(&mut self, frames: Vec<crate::stream::RawSse>) -> Result<(), ClientError> {
        for raw in frames {
            self.saw_frame = true;
            if let Some(err) = classify_sse_wrapped_error(&raw.data, self.http_status) {
                return Err(err);
            }
            let events = decode_stream_events(self.wire, &raw, &self.profile)?;
            for ev in events {
                self.pending.extend(self.assembler.push(ev));
            }
        }
        Ok(())
    }
}

async fn step_stream(
    phase: StreamPhase,
) -> Option<(Result<IrStreamEvent, ClientError>, StreamPhase)> {
    match phase {
        StreamPhase::Start { client, ir } => match client.open_stream(*ir).await {
            Ok(live) => pull_live(live).await,
            Err(err) => Some((Err(err), StreamPhase::Done)),
        },
        StreamPhase::Live(live) => pull_live(live).await,
        StreamPhase::Done => None,
    }
}

async fn pull_live(
    mut live: LiveStream,
) -> Option<(Result<IrStreamEvent, ClientError>, StreamPhase)> {
    loop {
        if let Some(ev) = live.pending.pop_front() {
            return Some((Ok(ev), StreamPhase::Live(live)));
        }
        if live.eof {
            return None;
        }
        match live.bytes.next().await {
            Some(Ok(chunk)) => {
                if !live.saw_frame {
                    append_capped(&mut live.leftover, &chunk, MAX_ERROR_BODY);
                }
                match live.reader.feed(&chunk) {
                    Ok((frames, terminal)) => {
                        if let Err(err) = live.push_frames(frames) {
                            return Some((Err(err), StreamPhase::Done));
                        }
                        if let Some(err) = terminal {
                            return Some((
                                Err(classify_feed_err(err, live.http_status)),
                                StreamPhase::Done,
                            ));
                        }
                    }
                    Err(err) => {
                        return Some((
                            Err(classify_feed_err(err, live.http_status)),
                            StreamPhase::Done,
                        ));
                    }
                }
            }
            Some(Err(err)) => {
                return Some((Err(classify_read_err(err)), StreamPhase::Done));
            }
            None => {
                if let Some(last) = live.reader.drain()
                    && let Err(err) = live.push_frames(vec![last])
                {
                    return Some((Err(err), StreamPhase::Done));
                }
                if !live.saw_frame {
                    let text = String::from_utf8_lossy(&live.leftover);
                    return Some((
                        Err(classify_empty_stream(live.http_status, &text)),
                        StreamPhase::Done,
                    ));
                }
                for ev in live.assembler.flush() {
                    live.pending.push_back(ev);
                }
                live.eof = true;
            }
        }
    }
}

fn profile_wire(profile: &ResolvedProfile) -> Result<Wire, ClientError> {
    profile.dialect.wire.ok_or_else(|| {
        ClientError::Map(MapError::Invalid(format!(
            "profile has no wire; set `wire` to `{}`",
            Wire::NAMES.join("|")
        )))
    })
}

fn profile_err(err: ProfileError) -> ClientError {
    match err {
        ProfileError::NotFound { id, known } => ClientError::NotFound {
            status: None,
            message: not_found_message(&id, &known),
        },
        other => ClientError::Auth {
            status: None,
            message: other.to_string(),
        },
    }
}

fn auth_err(err: AuthError) -> ClientError {
    match err {
        AuthError::VendorRejected { status, summary } => ClientError::Auth {
            status: Some(status),
            message: summary,
        },
        other => ClientError::Auth {
            status: None,
            message: other.to_string(),
        },
    }
}

fn aws_sign_err(err: crate::aws_sign::AwsSignError) -> ClientError {
    match err {
        crate::aws_sign::AwsSignError::Auth(message) => ClientError::Auth {
            status: None,
            message,
        },
        crate::aws_sign::AwsSignError::Transport(message) => ClientError::Transport(message),
    }
}

fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

async fn read_body(
    resp: reqwest::Response,
    cap: usize,
    hard_cap: bool,
) -> Result<Vec<u8>, ClientError> {
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(classify_read_err)?;
        if buf.len().saturating_add(chunk.len()) > cap {
            if hard_cap {
                return Err(ClientError::Transport(format!(
                    "response body exceeds {cap} bytes"
                )));
            }
            let room = cap.saturating_sub(buf.len());
            buf.extend_from_slice(&chunk[..room]);
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn transport_message(err: &reqwest::Error) -> String {
    let mut message = err.to_string();
    let lower = message.to_ascii_lowercase();
    if err.is_connect() {
        if !lower.contains("connect") && !lower.contains("connection refused") {
            message.push_str(": error trying to connect");
        }
    } else if err.is_timeout() && !lower.contains("timed out") && !lower.contains("timeout") {
        message.push_str(": timed out");
    }
    message
}

fn classify_send_err(err: reqwest::Error) -> ClientError {
    let message = transport_message(&err);
    let status = err.status().map(|s| s.as_u16());
    if err.is_builder() && !err.is_connect() && !err.is_timeout() && !looks_like_reset(&message) {
        return ClientError::Transport(message);
    }
    let kind = send_transient_kind(
        err.is_connect(),
        err.is_timeout(),
        looks_like_reset(&message),
    );
    transient(status, message, kind)
}

fn classify_read_err(err: reqwest::Error) -> ClientError {
    let message = transport_message(&err);
    let status = err.status().map(|s| s.as_u16());
    if err.is_timeout() {
        transient(status, message, TransientKind::Timeout)
    } else if looks_like_reset(&message) {
        transient(status, message, TransientKind::Reset)
    } else {
        ClientError::Transport(message)
    }
}

fn send_transient_kind(is_connect: bool, is_timeout: bool, reset: bool) -> TransientKind {
    if is_connect {
        TransientKind::Connect
    } else if is_timeout {
        TransientKind::Timeout
    } else if reset {
        TransientKind::Reset
    } else {
        TransientKind::Http
    }
}

fn looks_like_reset(message: &str) -> bool {
    let t = message.to_ascii_lowercase();
    t.contains("connection reset") || t.contains("broken pipe")
}

fn append_capped(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) {
    let room = cap.saturating_sub(buf.len());
    if room == 0 {
        return;
    }
    let take = room.min(chunk.len());
    buf.extend_from_slice(&chunk[..take]);
}

fn classify_feed_err(err: String, status: u16) -> ClientError {
    if let Some(classified) = classify_eventstream_exception(&err, status) {
        return classified;
    }
    ClientError::Transport(err)
}

fn classify_eventstream_exception(err: &str, status: u16) -> Option<ClientError> {
    let rest = err.strip_prefix("eventstream exception ")?;
    let (exception_type, message) = match rest.split_once(':') {
        Some((ty, msg)) => (ty.trim(), msg.trim()),
        None => (rest.trim(), rest.trim()),
    };
    Some(classify_aws_exception_type(
        status,
        exception_type,
        message,
        err,
    ))
}

fn classify_aws_exception_type(
    status: u16,
    exception_type: &str,
    message: &str,
    full: &str,
) -> ClientError {
    let ty = exception_type.to_ascii_lowercase();
    if ty.contains("throttl") {
        return ClientError::RateLimit {
            status: Some(status),
            retry_after: None,
            message: full.to_string(),
        };
    }
    if ty.contains("internal") || ty.contains("serviceunavailable") || ty.contains("timeout") {
        return transient(Some(status), full.to_string(), TransientKind::Http);
    }
    classify_error_payload(Some(status), None, message, full, None)
}

fn classify_empty_stream(status: u16, body: &str) -> ClientError {
    if let Some(err) = classify_http(status, body, None) {
        return err;
    }
    if !body.trim().is_empty() {
        return ClientError::Vendor {
            status: Some(status),
            message: error_message(body),
        };
    }
    transient(Some(status), "empty stream", TransientKind::Http)
}

fn classify_sse_wrapped_error(data: &str, status: u16) -> Option<ClientError> {
    let value: Value = serde_json::from_str(data).ok()?;
    if value.get("choices").is_some() || value.get("delta").is_some() {
        return None;
    }
    let error = value.get("error").filter(|v| v.is_object())?;
    let code = json_error_code(error);
    let message = error_message(data);
    Some(classify_error_payload(
        Some(status),
        code,
        &message,
        data,
        None,
    ))
}

fn classify_http(status: u16, body: &str, retry_after: Option<u64>) -> Option<ClientError> {
    let message = error_message(body);
    let parsed = serde_json::from_str::<Value>(body).ok();
    let error_obj = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .filter(|v| v.is_object());
    let code = error_obj.and_then(json_error_code);

    if (200..300).contains(&status) {
        if error_obj.is_some() {
            return Some(classify_error_payload(
                Some(status),
                code,
                &message,
                body,
                retry_after,
            ));
        }
        return None;
    }

    if status == 401 {
        return Some(ClientError::Auth {
            status: Some(401),
            message,
        });
    }
    if status == 429 {
        return Some(ClientError::RateLimit {
            status: Some(429),
            retry_after,
            message,
        });
    }
    if status == 408 || (500..600).contains(&status) {
        return Some(transient(Some(status), message, TransientKind::Http));
    }
    if status == 404 {
        return Some(ClientError::NotFound {
            status: Some(404),
            message,
        });
    }
    if status == 400 || status == 403 {
        if looks_like_bad_key(body) || looks_like_bad_key(&message) {
            return Some(ClientError::Auth {
                status: Some(status),
                message,
            });
        }
        if looks_like_model_not_found(body) || looks_like_model_not_found(&message) {
            return Some(ClientError::NotFound {
                status: Some(status),
                message,
            });
        }
        return Some(ClientError::Vendor {
            status: Some(status),
            message,
        });
    }
    if (400..500).contains(&status) {
        return Some(ClientError::Vendor {
            status: Some(status),
            message,
        });
    }
    Some(transient(Some(status), message, TransientKind::Http))
}

fn classify_error_payload(
    status: Option<u16>,
    code: Option<i64>,
    message: &str,
    body: &str,
    retry_after: Option<u64>,
) -> ClientError {
    if looks_like_bad_key(body) || looks_like_bad_key(message) {
        return ClientError::Auth {
            status,
            message: message.to_string(),
        };
    }
    if looks_like_model_not_found(body) || looks_like_model_not_found(message) {
        return ClientError::NotFound {
            status,
            message: message.to_string(),
        };
    }
    if code == Some(429) && status == Some(429) {
        return ClientError::RateLimit {
            status,
            retry_after,
            message: message.to_string(),
        };
    }
    if code.is_some_and(is_transient_code)
        || looks_like_overload(body)
        || looks_like_overload(message)
    {
        return transient(status, message.to_string(), TransientKind::Http);
    }
    ClientError::Vendor {
        status,
        message: message.to_string(),
    }
}

fn json_error_code(error: &Value) -> Option<i64> {
    let code = error.get("code")?;
    code.as_i64()
        .or_else(|| code.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| code.as_str()?.parse().ok())
}

fn is_transient_code(code: i64) -> bool {
    code == 408 || code == 429 || (500..600).contains(&code)
}

fn looks_like_bad_key(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("incorrect api key") || t.contains("unauthorized") || t.contains("invalid key")
}

fn looks_like_model_not_found(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("does not exist") || t.contains("model_not_found") || t.contains("unknown model")
}

fn looks_like_overload(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("upstream overload") || t.contains("overloaded")
}

fn error_message(body: &str) -> String {
    let extracted = if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(msg) = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            msg.to_string()
        } else if let Some(msg) = value
            .get("message")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            msg.to_string()
        } else {
            fallback_error_text(body)
        }
    } else {
        fallback_error_text(body)
    };
    redact_embedded_url(&sanitize_oauth_error_text(&extracted))
}

fn redact_embedded_url(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(rel) = s[i..].find("://") {
        let sep = i + rel;
        let scheme_start = scheme_start_before(s, sep);
        if scheme_start == sep {
            out.push_str(&s[i..sep + 3]);
            i = sep + 3;
            continue;
        }
        out.push_str(&s[i..scheme_start]);
        let after = sep + 3;
        let url_end = url_end_from(s, after);
        out.push_str(&redact_url_origin(&s[scheme_start..url_end]));
        i = url_end;
    }
    out.push_str(&s[i..]);
    out
}

fn scheme_start_before(s: &str, sep: usize) -> usize {
    let prefix = &s[..sep];
    let mut start = sep;
    for (idx, ch) in prefix.char_indices().rev() {
        if is_scheme_char(ch) {
            start = idx;
        } else {
            break;
        }
    }
    if start < sep
        && s[start..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return start;
    }
    sep
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-'
}

fn url_end_from(s: &str, start: usize) -> usize {
    s[start..]
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || matches!(*c, '<' | '>' | '"' | '\'' | ')' | ']' | '}'))
        .map(|(idx, _)| start + idx)
        .unwrap_or(s.len())
}

fn fallback_error_text(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "request failed".into()
    } else {
        trimmed.chars().take(512).collect()
    }
}

fn parse_listed_models(bytes: &[u8]) -> Result<Vec<ListedModel>, ClientError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|err| ClientError::Transport(err.to_string()))?;
    if let Some(data) = value.get("data").and_then(Value::as_array) {
        return Ok(parse_openai_listed_models(data));
    }
    if let Some(models) = value.get("models").and_then(Value::as_array) {
        return Ok(parse_gemini_listed_models(models));
    }
    Ok(Vec::new())
}

fn parse_openai_listed_models(data: &[Value]) -> Vec<ListedModel> {
    let mut out = Vec::new();
    for item in data {
        let Some(id) = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let context_tokens = item
            .get("context_length")
            .or_else(|| item.get("max_input_tokens"))
            .or_else(|| item.get("context_window"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        let vision = item
            .pointer("/architecture/input_modalities")
            .and_then(Value::as_array)
            .map(|mods| {
                mods.iter()
                    .any(|m| matches!(m.as_str(), Some("image" | "vision")))
            });
        out.push(ListedModel {
            id: id.to_string(),
            context_tokens,
            vision,
        });
    }
    out
}

fn parse_gemini_listed_models(models: &[Value]) -> Vec<ListedModel> {
    let mut out = Vec::new();
    for item in models {
        let Some(id) = item
            .get("name")
            .and_then(Value::as_str)
            .map(|name| name.strip_prefix("models/").unwrap_or(name))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let context_tokens = item
            .get("inputTokenLimit")
            .or_else(|| item.get("input_token_limit"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        // generateContent is the text/multimodal send method, not a vision flag.
        // Leave vision None unless a field is clearly image input.
        let vision = gemini_listed_vision(item);
        out.push(ListedModel {
            id: id.to_string(),
            context_tokens,
            vision,
        });
    }
    out
}

fn gemini_listed_vision(item: &Value) -> Option<bool> {
    const KEYS: &[&str] = &[
        "supportedInputModalities",
        "supported_input_modalities",
        "inputModalities",
        "input_modalities",
    ];
    for key in KEYS {
        let Some(mods) = item.get(*key).and_then(Value::as_array) else {
            continue;
        };
        return Some(mods.iter().any(|m| {
            matches!(
                m.as_str().map(str::to_ascii_lowercase).as_deref(),
                Some("image" | "vision")
            )
        }));
    }
    None
}

/// Catalog URL for `list_models`.
///
/// `/v1/chat/completions` and `/v1/messages` become `{base}/v1/models`.
/// Gemini `/v1beta/models/{model}:generateContent` becomes `{base}/v1beta/models`.
/// A path with no version prefix stays `{base}/models`.
fn models_url(base: &str, chat_path: &str) -> String {
    let base = base.trim_end_matches('/');
    match chat_version_prefix(chat_path) {
        Some(prefix) => format!("{base}/{prefix}/models"),
        None => format!("{base}/models"),
    }
}

/// First path segment when it looks like `v1` or `v1beta`.
fn chat_version_prefix(chat_path: &str) -> Option<&str> {
    let path = chat_path.trim().trim_start_matches('/');
    let first = path.split('/').next().unwrap_or("");
    let rest = first.strip_prefix('v')?;
    let mut chars = rest.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    if chars.all(|c| c.is_ascii_alphanumeric()) {
        Some(first)
    } else {
        None
    }
}

fn allows_ollama_show(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let host = url.host_str().unwrap_or("");
    let loopback = host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost");
    loopback && url.port() == Some(11434)
}

fn context_from_show(value: &Value) -> Option<u32> {
    if let Some(n) = value
        .get("context_length")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
    {
        return Some(n);
    }
    let info = value.get("model_info")?.as_object()?;
    for (key, val) in info {
        if key.ends_with("context_length")
            && let Some(n) = val.as_u64().and_then(|n| u32::try_from(n).ok())
        {
            return Some(n);
        }
    }
    None
}

fn vision_from_show(value: &Value) -> Option<bool> {
    value
        .pointer("/details/families")
        .and_then(Value::as_array)
        .map(|families| {
            families.iter().any(|f| {
                matches!(
                    f.as_str().map(str::to_ascii_lowercase).as_deref(),
                    Some("clip" | "vision")
                )
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremux_auth::parse_profile_str;

    #[test]
    fn missing_wire_names_legal_values() {
        let profile = parse_profile_str(
            r#"
schema_version = 1
id = "nowire"
base_url = "https://example.invalid"
"#,
        )
        .expect("parse");
        let err = profile_wire(&profile).expect_err("no wire");
        let text = err.to_string();
        assert!(text.contains("wire"), "{text}");
        for name in Wire::NAMES {
            assert!(
                text.contains(name),
                "must list legal wire `{name}`, got {text}"
            );
        }
    }

    #[test]
    fn missing_base_url_names_top_level_field() {
        let profile = parse_profile_str(
            r#"
schema_version = 1
id = "nobase"
wire = "messages"
"#,
        )
        .expect("parse");
        let err = upstream_url_for_model(&profile, None, false).expect_err("no base_url");
        assert!(err.contains("base_url"), "must name base_url, got {err}");
        assert!(
            !err.contains("http.base_url"),
            "must not tell the user to set http.base_url, got {err}"
        );
    }

    #[tokio::test]
    async fn list_models_missing_base_url_names_top_level_field() {
        let profile = parse_profile_str(
            r#"
schema_version = 1
id = "nobase"
wire = "messages"
"#,
        )
        .expect("parse");
        let client = WireClient::from_resolved(
            profile,
            AnyTokenProvider::from(wiremux_auth::StaticToken::new("x")),
        )
        .expect("client");
        let err = client.list_models().await.expect_err("no base_url");
        let text = err.to_string();
        assert!(
            text.contains("base_url"),
            "list_models must name base_url, got {text}"
        );
        assert!(
            !text.contains("http.base_url"),
            "list_models must not tell the user to set http.base_url, got {text}"
        );
    }

    #[test]
    fn from_profile_typo_suggests_close_match() {
        let err = WireClient::from_profile_opts(
            "anthropic-oath",
            &LoadOptions {
                include_user_config: false,
                ..LoadOptions::default()
            },
        )
        .expect_err("near-miss id");
        let text = err.to_string();
        assert!(
            text.contains("did you mean") && text.contains("anthropic"),
            "profile_err must keep the catalog suggestion, got {text}"
        );
    }

    #[test]
    fn error_message_keeps_vendor_text_around_gs_url() {
        let body = r#"{"error":{"message":"invalid fileUri gs://bucket/path/file.pdf because mime type is unsupported"}}"#;
        let msg = error_message(body);
        assert!(
            msg.contains("mime type is unsupported"),
            "must keep vendor text after the URL, got {msg}"
        );
        assert!(
            !msg.contains("/path/file.pdf"),
            "must drop object path, got {msg}"
        );
        assert!(
            msg.contains("gs://bucket"),
            "must keep scheme+bucket, got {msg}"
        );
    }

    #[test]
    fn error_message_redacts_userinfo_and_keeps_surrounding_text() {
        let body = r#"{"error":{"message":"callback https://user:secret@evil.test/x failed"}}"#;
        let msg = error_message(body);
        assert!(
            !msg.contains("user:secret") && !msg.contains("secret@"),
            "must not leak userinfo, got {msg}"
        );
        assert!(
            msg.contains("https://evil.test"),
            "must keep origin, got {msg}"
        );
        assert!(!msg.contains("/x"), "must drop path, got {msg}");
        assert!(
            msg.contains("callback") && msg.contains("failed"),
            "must keep surrounding text, got {msg}"
        );
    }

    #[test]
    fn error_message_without_url_is_unchanged() {
        let body = r#"{"error":{"message":"invalid argument: mime type is unsupported"}}"#;
        let msg = error_message(body);
        assert_eq!(msg, "invalid argument: mime type is unsupported");
    }

    #[test]
    fn syn_timeout_flags_classify_as_connect() {
        assert_eq!(
            send_transient_kind(true, true, false),
            TransientKind::Connect
        );
        assert_eq!(
            send_transient_kind(false, true, false),
            TransientKind::Timeout
        );
        assert_eq!(
            send_transient_kind(false, false, true),
            TransientKind::Reset
        );
        let err = transient(None, "connection reset by peer", TransientKind::Reset);
        assert!(err.is_reset());
        assert!(!err.is_connect());
        assert!(!err.is_timeout());
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
}

//! Optional library POST/SSE client. Feature `client` (not default).

use std::collections::VecDeque;
use std::fmt;
use std::pin::Pin;

use bytes::Bytes;
use futures_util::future::Either;
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
use crate::stream::{
    INCOMPLETE_STREAM_MESSAGE, StreamDecoder, ToolCallAssembler, UpstreamFrames, decode_response,
    frame_is_terminal, sse_wrapped_error_message,
};
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
    /// Vendor rate limit: HTTP 429, or an event-stream exception whose
    /// type says the call was throttled. `retry_after` is seconds when
    /// `Retry-After` is numeric. `status` is the HTTP status the vendor
    /// sent, which is not always 429 for an event-stream throttle.
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
        let events = match decode_response(wire, &body, &self.profile) {
            Ok(events) => events,
            Err(err) => {
                if let Some(classified) = converse_json_client_error(wire, status, &text) {
                    return Err(classified);
                }
                return Err(err.into());
            }
        };
        Ok((events, loss))
    }

    /// POST with the dialect stream flag and remap SSE frames incrementally.
    ///
    /// Encode loss is dropped. [`Self::stream_with_loss`] returns that report.
    /// An encode error is the first item, and no request is sent.
    /// The stream does not borrow `self`.
    pub fn stream(
        &self,
        ir: IrRequest,
    ) -> impl Stream<Item = Result<IrStreamEvent, ClientError>> + Send + use<> {
        match self.stream_with_loss(ir) {
            Ok((_loss, events)) => Either::Left(events),
            Err(err) => Either::Right(futures_util::stream::once(futures_util::future::ready(
                Err(err),
            ))),
        }
    }

    /// Encode with the dialect stream flag, then remap SSE frames.
    ///
    /// The [`LossReport`] is encode loss only, and it is ready before any
    /// HTTP call. Encode errors return [`Err`] and do not open a stream.
    /// HTTP and SSE errors stay on the event stream. The stream does not
    /// borrow `self`.
    pub fn stream_with_loss(
        &self,
        mut ir: IrRequest,
    ) -> Result<
        (
            LossReport,
            impl Stream<Item = Result<IrStreamEvent, ClientError>> + Send + use<>,
        ),
        ClientError,
    > {
        ir.sampling.stream = Some(true);
        let wire = profile_wire(&self.profile)?;
        let (encoded, loss) = encode(wire, &ir, &self.profile)?;
        let client = self.clone();
        let model = ir.model;
        let events = futures_util::stream::unfold(
            StreamPhase::Start {
                client,
                model,
                encoded,
            },
            |phase| async move { step_stream(phase).await },
        );
        Ok((loss, events))
    }

    /// GET the models catalog. OpenAI-compat uses the chat version prefix
    /// (`{base}/v1/models` for `/v1/chat/completions`,
    /// `{base}/openai/v1/models` for `/openai/v1/chat/completions`).
    /// A Vertex `chat_path` keeps `publishers/.../models`.
    /// HTTP 404 after the
    /// request is sent is an empty list, including a SigV4-signed GET.
    /// HTTP 401 is [`ClientError::Auth`] for a static token. A provider
    /// that can refresh retries that GET once after `mark_stale`. When
    /// `aws_service` is set and no bearer token is applied, the GET is
    /// SigV4-signed like `send`. Failure to sign is not an empty catalog.
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
        let resp = self.get_url_auth_retry(&url).await?;
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
        if matches!(
            self.profile.http.auth_scheme,
            Some(wiremux_auth::AuthScheme::None)
        ) && self.profile.oauth.is_none()
        {
            return Ok(None);
        }
        let token = self.provider.get_token().await.map_err(auth_err)?;
        if token.trim().is_empty() {
            Err(ClientError::Auth {
                status: None,
                message: "empty token is not auth_scheme none".into(),
            })
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

    /// GET with the same one-shot 401 refresh as [`Self::post_json_auth_retry`].
    async fn get_url_auth_retry(&self, url: &str) -> Result<reqwest::Response, ClientError> {
        let token = self.access_token().await?;
        let resp = self.get_url(url, token.as_deref()).await?;
        if resp.status().as_u16() != 401 || !self.provider.can_refresh() {
            return Ok(resp);
        }
        drop(resp);
        self.provider.mark_stale();
        let token = self.access_token().await?;
        self.get_url(url, token.as_deref()).await
    }

    async fn get_url(
        &self,
        url: &str,
        token: Option<&str>,
    ) -> Result<reqwest::Response, ClientError> {
        let req = self
            .http
            .get(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        let req = apply_profile_headers(req, &self.profile, token);
        let req = apply_provider_headers(req, &self.profile, &self.provider);
        let built = apply_aws_sigv4(
            &self.profile,
            url,
            "GET",
            b"",
            req,
            bearer_token_applied(&self.profile, token),
        )
        .await
        .map_err(aws_sign_err)?;
        self.http.execute(built).await.map_err(classify_send_err)
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

    async fn open_encoded(&self, model: &str, encoded: Vec<u8>) -> Result<LiveStream, ClientError> {
        let wire = profile_wire(&self.profile)?;
        let url = upstream_url_for_model(&self.profile, Some(model), true)
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
            decoder: StreamDecoder::new(),
            assembler: ToolCallAssembler::new(),
            pending: VecDeque::new(),
            wire,
            profile: self.profile.clone(),
            eof: false,
            saw_frame: false,
            saw_terminal: false,
            leftover: Vec::new(),
            http_status: status,
            stop_error: None,
        })
    }
}

enum StreamPhase {
    Start {
        client: WireClient,
        model: String,
        encoded: Vec<u8>,
    },
    Live(LiveStream),
    Done,
}

struct LiveStream {
    bytes: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    reader: UpstreamFrames,
    decoder: StreamDecoder,
    assembler: ToolCallAssembler,
    pending: VecDeque<IrStreamEvent>,
    wire: Wire,
    profile: ResolvedProfile,
    eof: bool,
    saw_frame: bool,
    saw_terminal: bool,
    leftover: Vec<u8>,
    http_status: u16,
    /// Boxed so `StreamPhase::Live` stays no larger than `Start`.
    stop_error: Option<Box<ClientError>>,
}

impl LiveStream {
    fn push_frames(&mut self, frames: Vec<crate::stream::RawSse>) -> Result<(), ClientError> {
        for raw in frames {
            self.saw_frame = true;
            if frame_is_terminal(self.wire, &raw, &self.profile) {
                self.saw_terminal = true;
            }
            if let Some(err) = classify_sse_wrapped_error(&raw.data, self.http_status) {
                return Err(err);
            }
            if raw.event.as_deref() == Some("response.failed")
                && let Some(err) = classify_responses_failed(&raw.data, self.http_status)
            {
                return Err(err);
            }
            let events = self.decoder.decode(self.wire, &raw, &self.profile)?;
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
        StreamPhase::Start {
            client,
            model,
            encoded,
        } => match client.open_encoded(&model, encoded).await {
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
        if let Some(err) = live.stop_error.take() {
            return Some((Err(*err), StreamPhase::Done));
        }
        if live.eof {
            if live.saw_frame && !live.saw_terminal {
                return Some((
                    Err(ClientError::Transient {
                        status: None,
                        message: INCOMPLETE_STREAM_MESSAGE.to_string(),
                        kind: TransientKind::Http,
                    }),
                    StreamPhase::Done,
                ));
            }
            return None;
        }
        match live.bytes.next().await {
            Some(Ok(chunk)) => {
                if !live.saw_frame {
                    // A JSON body is not an Event Stream frame. The error
                    // cap would truncate it and the prelude CRC would win.
                    let cap = if json_prefix(&live.leftover) || json_prefix(&chunk) {
                        MAX_SUCCESS_BODY
                    } else {
                        MAX_ERROR_BODY
                    };
                    append_capped(&mut live.leftover, &chunk, cap);
                }
                match live.reader.feed(&chunk) {
                    Ok((frames, terminal)) => {
                        if let Err(err) = live.push_frames(frames) {
                            live.stop_error = Some(Box::new(err));
                        } else if let Some(err) = terminal {
                            live.stop_error =
                                Some(Box::new(classify_feed_err(err, live.http_status)));
                        }
                    }
                    Err(err) => {
                        if !live.saw_frame && !err.contains("exceeds") {
                            if promote_non_stream_completion(&mut live) {
                                live.eof = true;
                                continue;
                            }
                            let text = String::from_utf8_lossy(&live.leftover);
                            if let Some(classified) =
                                converse_json_client_error(live.wire, live.http_status, &text)
                            {
                                return Some((Err(classified), StreamPhase::Done));
                            }
                            if json_prefix(&live.leftover)
                                && serde_json::from_slice::<Value>(&live.leftover).is_err()
                            {
                                // A split JSON body is not a frame yet.
                                continue;
                            }
                            if serde_json::from_slice::<Value>(&live.leftover).is_ok() {
                                return Some((
                                    Err(classify_empty_stream(live.http_status, &text)),
                                    StreamPhase::Done,
                                ));
                            }
                        }
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
                match live.reader.finish() {
                    Ok(Some(last)) => {
                        if let Err(err) = live.push_frames(vec![last]) {
                            live.stop_error = Some(Box::new(err));
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        if !live.saw_frame {
                            if promote_non_stream_completion(&mut live) {
                                live.eof = true;
                                continue;
                            }
                            let text = String::from_utf8_lossy(&live.leftover);
                            if let Some(classified) =
                                converse_json_client_error(live.wire, live.http_status, &text)
                            {
                                return Some((Err(classified), StreamPhase::Done));
                            }
                            if live.wire == Wire::Converse
                                && (200..300).contains(&live.http_status)
                                && serde_json::from_slice::<Value>(&live.leftover).is_err()
                                && !live.leftover.is_empty()
                            {
                                return Some((
                                    Err(classify_feed_err(err, live.http_status)),
                                    StreamPhase::Done,
                                ));
                            }
                            return Some((
                                Err(classify_empty_stream(live.http_status, &text)),
                                StreamPhase::Done,
                            ));
                        }
                        return Some((
                            Err(classify_feed_err(err, live.http_status)),
                            StreamPhase::Done,
                        ));
                    }
                }
                if !live.saw_frame {
                    if promote_non_stream_completion(&mut live) {
                        live.eof = true;
                        continue;
                    }
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

/// A missing catalog id stays NotFound. Any other profile failure is the
/// file or document, not a credential, so it must not be Auth.
fn profile_err(err: ProfileError) -> ClientError {
    match err {
        ProfileError::NotFound { id, known } => ClientError::NotFound {
            status: None,
            message: not_found_message(&id, &known),
        },
        other => ClientError::Transport(clip_client_message(&other.to_string())),
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

fn json_prefix(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| matches!(byte, b'{' | b'['))
}

/// Converse HTTP 200 JSON uses `__type` and `message`, or an `error`
/// envelope when `output` and `stopReason` are absent.
///
/// `classify_http` misses that shape, so `send` would report `Map` and
/// `stream` would report a prelude CRC error.
fn converse_json_client_error(wire: Wire, status: u16, body: &str) -> Option<ClientError> {
    if wire != Wire::Converse || classify_http(status, body, None).is_some() {
        return None;
    }
    let value: Value = serde_json::from_str(body).ok()?;
    let detail = crate::stream::complete_exception_detail(&value)?;
    let ty = value.get("__type").and_then(Value::as_str).unwrap_or("");
    Some(classify_aws_exception_type(status, ty, &detail, &detail))
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
            message: clip_client_message(full),
        };
    }
    if ty.contains("internal") || ty.contains("serviceunavailable") || ty.contains("timeout") {
        return transient(Some(status), clip_client_message(full), TransientKind::Http);
    }
    classify_error_payload(Some(status), None, None, message, full, None)
}

/// A stream request sometimes comes back as one JSON completion.
///
/// `classify_http` already rejects an error object that has no visible
/// text. A 200 body whose choice text is empty can still carry a finish
/// reason, so this decodes that body instead of reporting it as a vendor error.
fn promote_non_stream_completion(live: &mut LiveStream) -> bool {
    if live.saw_frame || live.leftover.is_empty() {
        return false;
    }
    let text = String::from_utf8_lossy(&live.leftover);
    if classify_http(live.http_status, &text, None).is_some() {
        return false;
    }
    let Ok(events) = decode_response(live.wire, &live.leftover, &live.profile) else {
        return false;
    };
    if events.is_empty() {
        return false;
    }
    live.pending.extend(events);
    live.saw_frame = true;
    live.saw_terminal = true;
    true
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
    sse_wrapped_error_message(data)?;
    let value: Value = serde_json::from_str(data).ok()?;
    let nested = value.get("error").filter(|v| v.is_object());
    let source = nested.unwrap_or(&value);
    let code = json_error_code(source);
    let code_name = json_error_name(source);
    let message = error_message(data);
    let retry_after = nested
        .and_then(json_retry_after)
        .or_else(|| json_retry_after(&value));
    Some(classify_error_payload(
        Some(status),
        code,
        code_name.as_deref(),
        &message,
        data,
        retry_after,
    ))
}

/// `response.failed` is not an `{error:{...}}` envelope. Official
/// failures put the object on `response.error`. `last_error` is kept
/// for the older shape. Neither field means the frame can finish as
/// `failed`.
fn classify_responses_failed(data: &str, status: u16) -> Option<ClientError> {
    let value: Value = serde_json::from_str(data).ok()?;
    let last = crate::stream::responses_failure_object(&value)?;
    let code = json_error_code(last);
    let code_name = json_error_code_name(last);
    let message = last
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if code.is_none() && code_name.is_none() && message.is_empty() {
        return None;
    }
    let shown = if message.is_empty() {
        code_name
            .clone()
            .or_else(|| code.map(|n| n.to_string()))
            .unwrap_or_default()
    } else {
        message.to_string()
    };
    let shown = clip_client_message(&shown);
    Some(classify_error_payload(
        Some(status),
        code,
        code_name.as_deref(),
        &shown,
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
        if error_obj.is_none()
            && let Some(message) = parsed
                .as_ref()
                .and_then(crate::completion::json_string_error)
            && !success_body_has_completion(parsed.as_ref())
        {
            return Some(ClientError::Vendor {
                status: Some(status),
                message: clip_client_message(message),
            });
        }
        if error_obj.is_some() && !success_body_has_completion(parsed.as_ref()) {
            return Some(classify_error_payload(
                Some(status),
                code,
                error_obj.and_then(json_error_name).as_deref(),
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

/// A 200 body that already has completion text is not only a vendor error.
///
/// Empty arrays and empty strings count as no text. A refusal or a tool
/// call still counts. A non-empty Responses `output` array still counts
/// here, including a message shell. The proxy vendor-failure path uses
/// the leaf check.
fn success_body_has_completion(value: Option<&Value>) -> bool {
    value.is_some_and(crate::completion::json_has_completion)
}

fn classify_error_payload(
    status: Option<u16>,
    code: Option<i64>,
    code_name: Option<&str>,
    message: &str,
    body: &str,
    retry_after: Option<u64>,
) -> ClientError {
    let stored = clip_client_message(message);
    if looks_like_bad_key(body) || looks_like_bad_key(message) {
        return ClientError::Auth {
            status,
            message: stored,
        };
    }
    if looks_like_model_not_found(body) || looks_like_model_not_found(message) {
        return ClientError::NotFound {
            status,
            message: stored,
        };
    }
    if code == Some(429)
        || code_name.is_some_and(is_rate_limit_code)
        || looks_like_rate_limit(message)
        || looks_like_rate_limit(body)
    {
        return ClientError::RateLimit {
            status,
            retry_after,
            message: stored,
        };
    }
    if code.is_some_and(is_transient_code)
        || code_name.is_some_and(is_retryable_server_code)
        || looks_like_overload(body)
        || looks_like_overload(message)
    {
        return transient(status, stored, TransientKind::Http);
    }
    ClientError::Vendor {
        status,
        message: stored,
    }
}

fn is_rate_limit_code(name: &str) -> bool {
    matches!(name, "slow_down" | "rate_limit_exceeded")
}

fn is_retryable_server_code(name: &str) -> bool {
    matches!(
        name,
        "server_error"
            | "internal_error"
            | "connection_failed"
            | "request_timeout"
            | "server_is_overloaded"
            | "server_overloaded"
            | "api_error"
    )
}

fn json_error_code(error: &Value) -> Option<i64> {
    let code = error.get("code")?;
    code.as_i64()
        .or_else(|| code.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| code.as_str()?.parse().ok())
}

/// Non-numeric `code` strings (`server_error`). Numeric strings stay
/// on [`json_error_code`].
fn json_error_code_name(error: &Value) -> Option<String> {
    let name = error.get("code")?.as_str()?.trim();
    if name.is_empty() || name.parse::<i64>().is_ok() {
        return None;
    }
    Some(name.to_string())
}

/// Anthropic puts the class in `error.type` (`api_error`). The literal
/// `error` is the Responses event name, not a class.
fn json_error_type_name(error: &Value) -> Option<String> {
    let name = error.get("type")?.as_str()?.trim();
    if name.is_empty() || name == "error" || name.parse::<i64>().is_ok() {
        return None;
    }
    Some(name.to_string())
}

fn json_error_name(error: &Value) -> Option<String> {
    json_error_code_name(error).or_else(|| json_error_type_name(error))
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

fn json_retry_after(error: &Value) -> Option<u64> {
    let value = error.get("retry_after")?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn looks_like_rate_limit(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("rate_limit") || t.contains("rate limit")
}

fn looks_like_overload(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("upstream overload") || t.contains("overloaded")
}

fn clip_client_message(text: &str) -> String {
    // Redact the URL before the 200-character cut. Otherwise a password
    // whose @ sits past the cut is kept as the host.
    let urls_first = redact_embedded_url(text);
    redact_client_text(&sanitize_oauth_error_text(&urls_first))
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
    clip_client_message(&extracted)
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
    Err(ClientError::Vendor {
        status: Some(200),
        message: "model list has no data or models array".into(),
    })
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
/// The prefix runs through the first `v1` / `v1beta` segment, including
/// directories in front of it. `/v1/chat/completions` becomes
/// `{base}/v1/models`. `/openai/v1/chat/completions` becomes
/// `{base}/openai/v1/models`. `/api/v3/chat/completions` becomes
/// `{base}/api/v3/models`. Gemini
/// `/v1beta/models/{model}:generateContent` becomes `{base}/v1beta/models`.
/// A Vertex path keeps `publishers/.../models`, so the catalog is not
/// `{base}/v1/models`. Azure deployments have no version segment. Their
/// catalog is `{base}/openai/models` plus the chat path's `api-version`.
/// Any other path with no version segment stays `{base}/models`.
fn models_url(base: &str, chat_path: &str) -> String {
    let base = base.trim_end_matches('/');
    if let Some(url) = azure_models_url(base, chat_path) {
        return url;
    }
    match chat_version_prefix(chat_path) {
        Some(prefix) if prefix.ends_with("/models") => format!("{base}/{prefix}"),
        Some(prefix) => format!("{base}/{prefix}/models"),
        None => format!("{base}/models"),
    }
}

/// `{base}/openai/models?api-version=` for `/openai/deployments/{name}/chat/completions`.
///
/// `{base}/models` is not the Azure list. A 404 there looks like an empty catalog.
fn azure_models_url(base: &str, chat_path: &str) -> Option<String> {
    let trimmed = chat_path.trim();
    let (path, query) = match trimmed.split_once('?') {
        Some((path, query)) => (path, Some(query.split('#').next().unwrap_or(query))),
        None => (trimmed.split('#').next().unwrap_or(trimmed), None),
    };
    let path = path.trim_start_matches('/');
    let mut segments = path.split('/');
    if segments.next() != Some("openai") || segments.next() != Some("deployments") {
        return None;
    }
    segments.next().filter(|segment| !segment.is_empty())?;
    if segments.next() != Some("chat") || segments.next() != Some("completions") {
        return None;
    }
    if segments.next().is_some() {
        return None;
    }
    let version = query.and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "api-version" && !value.is_empty()).then_some(value)
        })
    });
    match version {
        Some(version) => Some(format!("{base}/openai/models?api-version={version}")),
        None => Some(format!("{base}/openai/models")),
    }
}

/// Chat path up through the first version segment (`v1`, `v1beta`, `v3`).
fn chat_version_prefix(chat_path: &str) -> Option<&str> {
    let path = chat_path.trim().trim_start_matches('/');
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let mut end = 0usize;
    for (index, segment) in path.split('/').enumerate() {
        if segment.is_empty() {
            return None;
        }
        if index > 0 {
            end += 1;
        }
        end += segment.len();
        if version_segment(segment) {
            return Some(catalog_prefix(path, end));
        }
    }
    None
}

/// Vertex chat paths are `/v1/projects/.../publishers/{name}/models/{model}:...`.
/// The catalog is that `models` collection, not `{base}/v1/models`.
fn catalog_prefix(path: &str, version_end: usize) -> &str {
    let Some(publishers) = path[version_end..].find("/publishers/") else {
        return &path[..version_end];
    };
    let after_publishers = version_end + publishers;
    let Some(models) = path[after_publishers..].find("/models") else {
        return &path[..version_end];
    };
    let models_end = after_publishers + models + "/models".len();
    &path[..models_end]
}

/// `v` plus a digit, then only ASCII letters and digits (`v1`, `v1beta`).
fn version_segment(segment: &str) -> bool {
    let Some(rest) = segment.strip_prefix('v') else {
        return false;
    };
    let mut chars = rest.chars();
    chars.next().is_some_and(|c| c.is_ascii_digit()) && chars.all(|c| c.is_ascii_alphanumeric())
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
    fn models_url_keeps_a_version_segment_that_is_not_first() {
        assert_eq!(
            models_url("https://api.groq.com", "/openai/v1/chat/completions"),
            "https://api.groq.com/openai/v1/models"
        );
        assert_eq!(
            models_url(
                "https://ark.cn-beijing.volces.com",
                "/api/v3/chat/completions"
            ),
            "https://ark.cn-beijing.volces.com/api/v3/models"
        );
        assert_eq!(
            models_url(
                "https://dashscope-intl.aliyuncs.com",
                "/compatible-mode/v1/chat/completions"
            ),
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/models"
        );
        assert_eq!(
            models_url("https://api.groq.com/", "/v1/chat/completions"),
            "https://api.groq.com/v1/models"
        );
        assert_eq!(
            models_url(
                "https://generativelanguage.googleapis.com",
                "/v1beta/models/{model}:generateContent"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
        assert_eq!(
            models_url("https://api.perplexity.ai", "/chat/completions"),
            "https://api.perplexity.ai/models"
        );
        assert_eq!(
            models_url(
                "https://us-central1-aiplatform.googleapis.com",
                "/v1/projects/p/locations/us-central1/publishers/google/models/{model}:generateContent"
            ),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/us-central1/publishers/google/models"
        );
        assert_eq!(
            models_url(
                "https://us-central1-aiplatform.googleapis.com",
                "/v1/projects/p/locations/us-central1/publishers/anthropic/models/{model}:rawPredict"
            ),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/us-central1/publishers/anthropic/models"
        );
        assert_eq!(
            models_url(
                "https://example.openai.azure.com",
                "/openai/deployments/{model}/chat/completions?api-version=2024-10-21"
            ),
            "https://example.openai.azure.com/openai/models?api-version=2024-10-21"
        );
        assert_eq!(
            models_url(
                "https://example.openai.azure.com/",
                "/openai/deployments/gpt-4o/chat/completions?api-version=2024-06-01&foo=1"
            ),
            "https://example.openai.azure.com/openai/models?api-version=2024-06-01"
        );
        assert_eq!(
            models_url(
                "https://example.openai.azure.com",
                "/openai/deployments/{model}/chat/completions"
            ),
            "https://example.openai.azure.com/openai/models"
        );
    }

    #[test]
    fn eventstream_throttle_is_rate_limit_with_caller_status() {
        let err = classify_aws_exception_type(
            200,
            "ThrottlingException",
            "slow down",
            "eventstream exception ThrottlingException: slow down",
        );
        match err {
            ClientError::RateLimit {
                status,
                retry_after,
                ..
            } => {
                assert_eq!(status, Some(200));
                assert_eq!(retry_after, None);
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn classify_http_keeps_completion_beside_an_error_object() {
        let with_choice = r#"{"choices":[{"message":{"role":"assistant","content":"Hi"}}],"error":{"message":"please wait","type":"rate_limit_error"}}"#;
        assert!(
            classify_http(200, with_choice, None).is_none(),
            "choice text must stay a completion"
        );
        let with_content = r#"{"type":"error","content":[{"type":"text","text":"Hi"}],"error":{"type":"api_error","message":"nope"}}"#;
        assert!(
            classify_http(200, with_content, None).is_none(),
            "message text must stay a completion"
        );
        let with_output = r#"{"status":"failed","output":[{"type":"message"}],"error":{"message":"please wait"}}"#;
        assert!(
            classify_http(200, with_output, None).is_none(),
            "response output must stay a completion"
        );
        let with_candidates = r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]}}],"error":{"message":"please wait","status":"RESOURCE_EXHAUSTED"}}"#;
        assert!(
            classify_http(200, with_candidates, None).is_none(),
            "candidates must stay a completion"
        );
        let only = r#"{"error":{"message":"please wait","type":"rate_limit_error"}}"#;
        classify_http(200, only, None)
            .expect("an error object with no completion text is an error");
        let string_error = r#"{"error":"model catalog unavailable"}"#;
        let err = classify_http(200, string_error, None)
            .expect("a string error with no completion text is an error");
        assert!(
            matches!(
                err,
                ClientError::Vendor {
                    status: Some(200),
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(
            err.to_string().contains("model catalog unavailable"),
            "{err}"
        );
        let beside = r#"{"choices":[{"message":{"role":"assistant","content":"Hi"}}],"error":"please wait"}"#;
        assert!(
            classify_http(200, beside, None).is_none(),
            "choice text must stay a completion when error is a string"
        );
    }

    /// The same class as the proxy table: an empty string is not
    /// completion text, and a refusal or a tool call still is.
    #[test]
    fn classify_http_empty_payload_stays_an_error() {
        let cases = [
            (
                r#"{"choices":[{"message":{"role":"assistant","content":""},"finish_reason":"stop"}],"error":{"message":"Provider returned error","code":502}}"#,
                true,
                "Provider returned error",
            ),
            (
                r#"{"choices":[{"message":{"role":"assistant","content":[{"type":"text","text":""}]}}],"error":{"message":"Provider returned error","code":502}}"#,
                true,
                "Provider returned error",
            ),
            (
                r#"{"candidates":[{"content":{"parts":[{"text":""}]}}],"error":{"message":"blocked"}}"#,
                true,
                "blocked",
            ),
            (
                r#"{"choices":[{"message":{"role":"assistant","content":"Hi"}}],"error":{"message":"please wait"}}"#,
                false,
                "",
            ),
            (
                r#"{"choices":[{"message":{"role":"assistant","content":"","refusal":"no"}}],"error":{"message":"please wait"}}"#,
                false,
                "",
            ),
            (
                r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"ping","arguments":"{}"}}]}}],"error":{"message":"please wait"}}"#,
                false,
                "",
            ),
            (
                r#"{"choices":[{"message":{"role":"assistant","content":null,"audio":{"transcript":"hello"}}}],"error":{"message":"please wait"}}"#,
                false,
                "",
            ),
            (
                r#"{"status":"failed","output":[{"type":"message"}],"error":{"message":"please wait"}}"#,
                false,
                "",
            ),
            (
                r#"{"type":"error","content":[{"type":"text","text":""}],"error":{"type":"api_error","message":"nope"}}"#,
                false,
                "",
            ),
        ];
        for (body, is_err, needle) in cases {
            let classified = classify_http(200, body, None);
            if is_err {
                let text = classified.expect(body).to_string();
                assert!(text.contains(needle), "{text}");
            } else {
                assert!(classified.is_none(), "{body}");
            }
        }
    }

    #[test]
    fn promote_keeps_a_finished_empty_choice_without_an_error() {
        fn live(body: &str) -> LiveStream {
            let profile =
                parse_profile_str("schema_version = 1\nid = \"c\"\nwire = \"chat-completions\"\n")
                    .expect("parse");
            let wire = profile_wire(&profile).expect("wire");
            LiveStream {
                bytes: Box::pin(futures_util::stream::iter(Vec::<
                    Result<Bytes, reqwest::Error>,
                >::new())),
                reader: UpstreamFrames::for_wire(wire),
                decoder: StreamDecoder::new(),
                assembler: ToolCallAssembler::new(),
                pending: VecDeque::new(),
                wire,
                profile,
                eof: false,
                saw_frame: false,
                saw_terminal: false,
                leftover: body.as_bytes().to_vec(),
                http_status: 200,
                stop_error: None,
            }
        }

        let mut finished = live(
            r#"{"choices":[{"message":{"role":"assistant","content":""},"finish_reason":"stop"}]}"#,
        );
        assert!(promote_non_stream_completion(&mut finished));
        assert!(finished.saw_terminal);

        let mut rejected = live(
            r#"{"choices":[{"message":{"role":"assistant","content":""},"finish_reason":"stop"}],"error":{"message":"Provider returned error","code":502}}"#,
        );
        assert!(!promote_non_stream_completion(&mut rejected));
    }

    #[tokio::test]
    async fn converse_error_body_keeps_status_when_no_frame() {
        let profile = parse_profile_str("schema_version = 1\nid = \"c\"\nwire = \"converse\"\n")
            .expect("parse");
        let body: &[u8] = &[0x00, 0x00, 0x00, 0x10];
        let live = LiveStream {
            bytes: Box::pin(futures_util::stream::iter(vec![Ok(
                Bytes::copy_from_slice(body),
            )])),
            reader: UpstreamFrames::for_wire(Wire::Converse),
            decoder: StreamDecoder::new(),
            assembler: ToolCallAssembler::new(),
            pending: VecDeque::new(),
            wire: Wire::Converse,
            profile,
            eof: false,
            saw_frame: false,
            saw_terminal: false,
            leftover: Vec::new(),
            http_status: 401,
            stop_error: None,
        };
        let (result, _) = pull_live(live).await.expect("one result");
        match result {
            Err(ClientError::Auth { status, .. }) => {
                assert_eq!(status, Some(401));
            }
            other => panic!("401 body with no frame must stay Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn converse_json_completion_split_before_crc_still_decodes() {
        let profile = parse_profile_str("schema_version = 1\nid = \"c\"\nwire = \"converse\"\n")
            .expect("parse");
        let rest = br#""output":{"message":{"role":"assistant","content":[{"text":"Hi"}]}},"stopReason":"end_turn"}"#;
        let live = LiveStream {
            bytes: Box::pin(futures_util::stream::iter(vec![
                Ok(Bytes::from_static(b"{")),
                Ok(Bytes::copy_from_slice(rest)),
            ])),
            reader: UpstreamFrames::for_wire(Wire::Converse),
            decoder: StreamDecoder::new(),
            assembler: ToolCallAssembler::new(),
            pending: VecDeque::new(),
            wire: Wire::Converse,
            profile,
            eof: false,
            saw_frame: false,
            saw_terminal: false,
            leftover: Vec::new(),
            http_status: 200,
            stop_error: None,
        };
        let mut phase = StreamPhase::Live(live);
        let mut text = String::new();
        loop {
            let StreamPhase::Live(live) = phase else {
                break;
            };
            let Some((item, next)) = pull_live(live).await else {
                break;
            };
            if let Ok(IrStreamEvent::TextDelta { text: delta }) = &item {
                text.push_str(delta);
            }
            if let Err(err) = &item {
                panic!("split JSON completion must decode, got {err}");
            }
            phase = next;
        }
        assert!(text.contains("Hi"), "{text}");
    }

    #[tokio::test]
    async fn converse_json_completion_over_error_cap_still_decodes() {
        let profile = parse_profile_str("schema_version = 1\nid = \"c\"\nwire = \"converse\"\n")
            .expect("parse");
        let mut body =
            br#"{"output":{"message":{"role":"assistant","content":[{"text":"PROMOTED-TAIL-"#
                .to_vec();
        body.extend(std::iter::repeat_n(b'a', 70_000));
        body.extend(br#""}]}},"stopReason":"end_turn"}"#);
        let live = LiveStream {
            bytes: Box::pin(futures_util::stream::iter(vec![
                Ok(Bytes::copy_from_slice(&body[..1])),
                Ok(Bytes::copy_from_slice(&body[1..70_000])),
                Ok(Bytes::copy_from_slice(&body[70_000..])),
            ])),
            reader: UpstreamFrames::for_wire(Wire::Converse),
            decoder: StreamDecoder::new(),
            assembler: ToolCallAssembler::new(),
            pending: VecDeque::new(),
            wire: Wire::Converse,
            profile,
            eof: false,
            saw_frame: false,
            saw_terminal: false,
            leftover: Vec::new(),
            http_status: 200,
            stop_error: None,
        };
        let mut phase = StreamPhase::Live(live);
        let mut text = String::new();
        loop {
            let StreamPhase::Live(live) = phase else {
                break;
            };
            let Some((item, next)) = pull_live(live).await else {
                break;
            };
            if let Ok(IrStreamEvent::TextDelta { text: delta }) = &item {
                text.push_str(delta);
            }
            if let Err(err) = &item {
                panic!("JSON completion past the error cap must decode, got {err}");
            }
            phase = next;
        }
        assert!(
            text.contains("PROMOTED-TAIL-") && text.len() > super::MAX_ERROR_BODY,
            "got {} bytes",
            text.len()
        );
    }

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
    async fn empty_client_token_is_auth_error() {
        let profile = parse_profile_str(
            r#"
schema_version = 1
id = "bearer"
wire = "chat-completions"
auth_scheme = "bearer"
base_url = "http://127.0.0.1:9"
"#,
        )
        .expect("parse");
        let client = WireClient::from_resolved(
            profile,
            AnyTokenProvider::from(wiremux_auth::StaticToken::new("   ")),
        )
        .expect("client");
        let err = client.access_token().await.expect_err("empty bearer");
        assert!(err.to_string().contains("empty token"), "{err}");

        let none = parse_profile_str(
            r#"
schema_version = 1
id = "none-auth"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://127.0.0.1:9"
"#,
        )
        .expect("parse");
        let client = WireClient::from_resolved(
            none,
            AnyTokenProvider::from(wiremux_auth::StaticToken::new("")),
        )
        .expect("client");
        let token = client.access_token().await.expect("none");
        assert!(token.is_none(), "{token:?}");
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
        match err {
            ClientError::NotFound {
                status,
                ref message,
            } => {
                assert_eq!(status, None);
                assert!(
                    message.contains("did you mean `anthropic-oauth`"),
                    "profile_err must suggest anthropic-oauth, got {message}"
                );
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn model_list_without_an_array_is_not_an_empty_catalog() {
        let err = super::parse_listed_models(br#"{"object":"list"}"#)
            .expect_err("object without data or models");
        match err {
            super::ClientError::Vendor { status, message } => {
                assert_eq!(status, Some(200));
                assert!(message.contains("data or models"), "{message}");
            }
            other => panic!("expected Vendor, got {other:?}"),
        }
        assert!(
            super::parse_listed_models(br#"{"data":[]}"#)
                .expect("empty data")
                .is_empty()
        );
        assert!(
            super::parse_listed_models(br#"{"models":[]}"#)
                .expect("empty models")
                .is_empty()
        );
    }

    #[test]
    fn http_200_string_error_is_clipped() {
        let long = "x".repeat(500);
        let body =
            format!(r#"{{"error":"callback https://user:s3cret@evil.test/x failed {long}"}}"#);
        let err = classify_http(200, &body, None).expect("string error");
        let ClientError::Vendor { message, .. } = err else {
            panic!("expected Vendor, got {err:?}");
        };
        assert!(!message.contains("user:s3cret"), "{message}");
        assert!(
            message.chars().filter(|c| *c == 'x').count() <= 200,
            "{message}"
        );
    }

    #[test]
    fn stored_message_redacts_gsk_and_bearer() {
        let body = r#"{"error":{"message":"bad key gsk_LIVE0123456789abcdef and Bearer ya29.a0AfH6SM_token"}}"#;
        let message = error_message(body);
        assert!(!message.contains("gsk_LIVE"), "{message}");
        assert!(!message.contains("ya29."), "{message}");
    }

    #[test]
    fn url_userinfo_past_the_cap_is_still_redacted() {
        let prefix = "w".repeat(160);
        let password = "S".repeat(40);
        let text = format!("vendor rejected {prefix} https://user:{password}@evil.test/object");
        let message = clip_client_message(&text);
        assert!(!message.contains(&password), "{message}");
        assert!(!message.contains("user:"), "{message}");
    }

    #[test]
    fn profile_parse_error_is_clipped() {
        let secret = format!("api_key = \"sk-{}\"", "a".repeat(300));
        let err = profile_err(ProfileError::Parse(secret));
        let ClientError::Transport(message) = err else {
            panic!("expected Transport, got {err:?}");
        };
        assert!(!message.contains("sk-aaaa"), "{message}");
        assert!(message.chars().count() <= 250, "{message}");
    }

    #[test]
    fn validation_exception_message_is_clipped() {
        let long = "x".repeat(400);
        let err = classify_aws_exception_type(
            400,
            "ValidationException",
            &format!("bad key gsk_LIVE0123456789abcdef {long}"),
            "eventstream exception ValidationException: ignored",
        );
        let ClientError::Vendor { message, .. } = err else {
            panic!("expected Vendor, got {err:?}");
        };
        assert!(!message.contains("gsk_LIVE"), "{message}");
        assert!(
            message.chars().filter(|c| *c == 'x').count() <= 200,
            "{message}"
        );
    }

    #[test]
    fn response_failed_message_is_capped() {
        let long = "x".repeat(500);
        let data = format!(
            r#"{{"type":"response.failed","response":{{"error":{{"code":"server_error","message":"{long}"}}}}}}"#
        );
        let err = classify_responses_failed(&data, 200).expect("failed frame");
        let xs = err.to_string().chars().filter(|c| *c == 'x').count();
        assert!(xs <= 200, "message must be capped, got {xs} in {err}");
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

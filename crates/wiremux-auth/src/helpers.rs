//! HTTP, lock, single-flight, and sanitization helpers.

use std::future::Future;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::warn;

use crate::error::AuthError;

/// Cap `expires_in` so a millisecond-as-seconds vendor cannot overflow.
pub(crate) const MAX_EXPIRES_IN_SECS: u64 = 10 * 365 * 24 * 3600;
/// Hard cap on token-endpoint bodies.
pub(crate) const MAX_OAUTH_BODY_BYTES: usize = 1024 * 1024;
/// Hard cap on credential-store files.
pub(crate) const MAX_CREDS_BYTES: u64 = 1024 * 1024;
/// Default lock wait.
pub(crate) const AUTH_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn duration_from_expires_in_secs(secs: u64) -> Duration {
    Duration::from_secs(secs.min(MAX_EXPIRES_IN_SECS))
}

pub(crate) fn oauth_http_client() -> Result<reqwest::Client, AuthError> {
    reqwest::Client::builder()
        .user_agent(format!("wiremux-auth/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AuthError::TokenProvider(format!("failed to build HTTP client: {e}")))
}

/// Shared in-flight refresh so parallel `get_token` issues one POST.
pub(crate) struct InFlight {
    inner: Arc<InFlightInner>,
}

type RefreshWatch = watch::Receiver<Option<Result<String, String>>>;

struct InFlightInner {
    slot: std::sync::Mutex<Option<RefreshWatch>>,
}

pub(crate) enum RefreshRole {
    Leader(RefreshLeader),
    Follower(RefreshWatch),
}

pub(crate) struct RefreshLeader {
    tx: watch::Sender<Option<Result<String, String>>>,
    inner: Arc<InFlightInner>,
    finished: bool,
}

impl InFlight {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(InFlightInner {
                slot: std::sync::Mutex::new(None),
            }),
        }
    }

    pub(crate) fn claim(&self) -> RefreshRole {
        let mut slot = self.inner.slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rx) = slot.as_ref() {
            return RefreshRole::Follower(rx.clone());
        }
        let (tx, rx) = watch::channel(None);
        *slot = Some(rx);
        RefreshRole::Leader(RefreshLeader {
            tx,
            inner: Arc::clone(&self.inner),
            finished: false,
        })
    }

    pub(crate) fn is_busy(&self) -> bool {
        let slot = self.inner.slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.is_some()
    }
}

impl RefreshLeader {
    pub(crate) fn complete(mut self, result: &Result<String, AuthError>) {
        self.finished = true;
        self.clear_slot();
        let shared = match result {
            Ok(token) => Ok(token.clone()),
            Err(err) => Err(err.to_string()),
        };
        let _ = self.tx.send(Some(shared));
    }

    fn clear_slot(&self) {
        let mut slot = self.inner.slot.lock().unwrap_or_else(|e| e.into_inner());
        *slot = None;
    }
}

impl Drop for RefreshLeader {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.clear_slot();
        let _ = self.tx.send(Some(Err("token refresh aborted".into())));
    }
}

pub(crate) async fn follow_refresh(
    mut rx: watch::Receiver<Option<Result<String, String>>>,
) -> Result<String, AuthError> {
    loop {
        if let Some(result) = rx.borrow().clone() {
            return result.map_err(AuthError::TokenProvider);
        }
        if rx.changed().await.is_err() {
            return Err(AuthError::TokenProvider("token refresh cancelled".into()));
        }
    }
}

pub(crate) async fn lead_or_follow<F, Fut>(
    inflight: &InFlight,
    refresh: F,
) -> Result<String, AuthError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, AuthError>>,
{
    match inflight.claim() {
        RefreshRole::Follower(rx) => follow_refresh(rx).await,
        RefreshRole::Leader(leader) => {
            let result = refresh().await;
            leader.complete(&result);
            result
        }
    }
}

pub(crate) fn cached_token_on_lock_failure(
    cached: Option<&str>,
    force: bool,
    err: AuthError,
) -> Result<String, AuthError> {
    if !force && let Some(token) = cached.filter(|t| !t.is_empty()) {
        return Ok(token.to_owned());
    }
    Err(err)
}

pub(crate) fn append_oauth_body_chunk(buf: &mut Vec<u8>, chunk: &[u8]) -> Result<(), AuthError> {
    if buf.len().saturating_add(chunk.len()) > MAX_OAUTH_BODY_BYTES {
        return Err(AuthError::TokenProvider(format!(
            "OAuth response body too large (>{MAX_OAUTH_BODY_BYTES} bytes)"
        )));
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// POST `application/x-www-form-urlencoded` to an https (or test loopback) URL.
pub(crate) async fn post_form_url(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<(u16, String), AuthError> {
    let parsed = parse_token_endpoint(url)?;
    let resp = http.post(parsed).form(form).send().await.map_err(|e| {
        AuthError::TokenProvider(format_oauth_transport_error(
            "token request failed",
            &e,
            url,
        ))
    })?;
    let status = resp.status().as_u16();
    let body = read_oauth_body(resp).await?;
    Ok((status, body))
}

pub(crate) fn parse_token_endpoint(url: &str) -> Result<reqwest::Url, AuthError> {
    let parsed = reqwest::Url::parse(url.trim())
        .map_err(|e| AuthError::TokenProvider(format!("token_url is not a valid URL: {e}")))?;
    if parsed.scheme() == "https" {
        return Ok(parsed);
    }
    #[cfg(any(test, feature = "test-util"))]
    if crate::profile::is_loopback_http(url) {
        return Ok(parsed);
    }
    Err(AuthError::TokenProvider(
        "token_url must be https (or loopback http)".into(),
    ))
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub(crate) async fn read_oauth_body(mut resp: reqwest::Response) -> Result<String, AuthError> {
    if let Some(len) = resp.content_length()
        && len > MAX_OAUTH_BODY_BYTES as u64
    {
        return Err(AuthError::TokenProvider(format!(
            "OAuth response body too large (Content-Length: {len} bytes, max {MAX_OAUTH_BODY_BYTES})"
        )));
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| {
        AuthError::TokenProvider(format_oauth_transport_error(
            "failed to read OAuth response body",
            &e,
            resp.url().as_str(),
        ))
    })? {
        append_oauth_body_chunk(&mut buf, &chunk)?;
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub(crate) fn is_token_rotation_error(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("refresh_token_reused")
        || lower.contains("invalid_grant")
        || lower.contains("token has been revoked")
        || lower.contains("token_expired")
}

/// `error` + `error_description` only. Never echo raw bodies or tokens.
pub(crate) fn sanitize_oauth_error_body(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    let Some(obj) = value.as_object() else {
        return String::new();
    };
    let error = obj
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let desc = obj
        .get("error_description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let summary = match (error.is_empty(), desc.is_empty()) {
        (true, true) => return String::new(),
        (false, true) => error.to_owned(),
        (true, false) => desc.to_owned(),
        (false, false) => format!("{error}: {desc}"),
    };
    sanitize_oauth_error_text(&summary)
}

/// Strip controls, redact secret-looking tokens, and cap length.
pub fn sanitize_oauth_error_text(text: &str) -> String {
    const MAX_CHARS: usize = 200;
    let mut summary: String = text.chars().filter(|c| !c.is_control()).collect();
    summary = redact_secret_looking(&summary);
    if summary.chars().count() > MAX_CHARS {
        summary = summary.chars().take(MAX_CHARS).collect();
    }
    summary
}

/// Short reason plus origin only. Never format a raw `reqwest` error.
pub fn format_oauth_transport_error(context: &str, err: &reqwest::Error, url: &str) -> String {
    format_oauth_transport_via(context, oauth_transport_reason(err), url)
}

pub(crate) fn format_oauth_transport_via(context: &str, reason: &str, url: &str) -> String {
    format!("{context} ({reason}) via {}", redact_url_origin(url))
}

fn oauth_transport_reason(err: &reqwest::Error) -> &'static str {
    if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connect"
    } else if err.is_request() {
        "request"
    } else {
        "transport"
    }
}

pub(crate) fn format_oauth_http_error(
    context: &str,
    status: impl std::fmt::Display,
    body: &str,
    url: &str,
) -> String {
    let summary = sanitize_oauth_error_body(body);
    let via = redact_url_origin(url);
    if summary.is_empty() {
        format!("{context} (HTTP {status}) via {via}")
    } else {
        format!("{context} (HTTP {status}): {summary} via {via}")
    }
}

/// Sanitized body and origin only. HTTP status lives on [`AuthError::VendorRejected`].
pub(crate) fn vendor_rejected_summary(context: &str, body: &str, url: &str) -> String {
    let summary = sanitize_oauth_error_body(body);
    let via = redact_url_origin(url);
    if summary.is_empty() {
        format!("{context} via {via}")
    } else {
        format!("{context}: {summary} via {via}")
    }
}

/// Redact common token prefixes and JWT-looking blobs.
pub fn redact_secret_looking(s: &str) -> String {
    let mut out = redact_prefix(s, "sk-ant-");
    out = redact_prefix(&out, "sk-");
    out = redact_jwt(&out);
    out = redact_prefix(&out, "rt-");
    out = redact_prefix(&out, "ghp_");
    out = redact_prefix(&out, "gho_");
    out = redact_prefix(&out, "ghs_");
    out = redact_prefix(&out, "ghu_");
    out = redact_prefix(&out, "github_pat_");
    redact_prefix(&out, "AKIA")
}

fn redact_prefix(s: &str, prefix: &str) -> String {
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

/// Scheme + host/port only. Drops userinfo, path, query, and fragment.
pub fn redact_url_origin(raw: &str) -> String {
    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, raw),
    };
    let cut = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..cut];
    let host = match authority.find('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    match scheme {
        Some(scheme) => format!("{scheme}://{host}"),
        None => host.to_string(),
    }
}

fn redact_jwt(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find("eyJ") {
        out.push_str(&rest[..idx]);
        out.push_str("[redacted]");
        rest = &rest[idx + 3..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// RAII advisory lock. The sibling `{path}.lock` is left in place on drop.
#[derive(Debug)]
pub(crate) struct FileLockGuard {
    file: std::fs::File,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

pub(crate) async fn try_acquire_refresh_lock(
    path: Option<&Path>,
    context: &str,
    timeout: Duration,
) -> Result<Option<FileLockGuard>, AuthError> {
    let Some(path) = path else {
        return Ok(None);
    };
    match acquire_file_lock(path, timeout).await {
        Ok(guard) => Ok(Some(guard)),
        Err(AuthError::LockTimeout) => {
            warn!("timed out acquiring file lock for {context}");
            Err(AuthError::LockTimeout)
        }
        Err(e) => {
            warn!("could not acquire file lock for {context}: {e}");
            Err(e)
        }
    }
}

pub(crate) async fn acquire_file_lock(
    path: &Path,
    timeout: Duration,
) -> Result<FileLockGuard, AuthError> {
    let lock_path = lock_sibling(path);
    let lock_path_clone = lock_path.clone();
    tokio::task::spawn_blocking(move || lock_exclusive_timeout(&lock_path_clone, timeout))
        .await
        .map_err(|e| AuthError::TokenProvider(format!("spawn_blocking: {e}")))?
}

pub(crate) fn lock_sibling(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

fn lock_exclusive_timeout(lock_path: &Path, timeout: Duration) -> Result<FileLockGuard, AuthError> {
    use fs4::TryLockError;
    use std::fs::OpenOptions;

    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AuthError::io(Some(parent.to_path_buf()), e))?;
    }

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|e| AuthError::io(Some(lock_path.to_path_buf()), e))?;

    let start = std::time::Instant::now();
    loop {
        // UFCS: std::fs::File::try_lock (1.89+) would otherwise shadow FileExt.
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => return Ok(FileLockGuard { file }),
            Err(TryLockError::WouldBlock) => {
                if start.elapsed() >= timeout {
                    return Err(AuthError::LockTimeout);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(TryLockError::Error(e)) if is_lock_busy(&e) => {
                if start.elapsed() >= timeout {
                    return Err(AuthError::LockTimeout);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(TryLockError::Error(e)) => {
                return Err(AuthError::io(Some(lock_path.to_path_buf()), e));
            }
        }
    }
}

fn is_lock_busy(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::ResourceBusy
    ) || err.raw_os_error()
        == Some(
            #[cfg(unix)]
            35, // EAGAIN / EWOULDBLOCK on some Unix; flock often uses EWOULDBLOCK
            #[cfg(not(unix))]
            33,
        )
}

pub(crate) fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    if let Some(rest) = path.strip_prefix("~\\")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// Expand `~` / relative paths, then refuse anything that leaves `$HOME`.
pub(crate) fn resolve_creds_path(raw: &str) -> Result<PathBuf, AuthError> {
    jail_creds_path(&expand_tilde(raw))
}

/// Refuse `..` and any path that is not under `$HOME` / IsolatedHome.
pub(crate) fn jail_creds_path(path: &Path) -> Result<PathBuf, AuthError> {
    if path.as_os_str().is_empty() || has_parent_dir(path) {
        return Err(creds_path_escapes_home());
    }
    let home = home_dir().filter(|h| !h.as_os_str().is_empty());
    let Some(home) = home else {
        return Err(creds_path_escapes_home());
    };
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        home.join(path)
    };
    if has_parent_dir(&resolved) || !path_is_under_home(&resolved, &home) {
        return Err(creds_path_escapes_home());
    }
    Ok(resolved)
}

fn has_parent_dir(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

fn path_is_under_home(path: &Path, home: &Path) -> bool {
    let home_canon = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if let Ok(canon) = std::fs::canonicalize(path) {
        return canon.starts_with(&home_canon);
    }
    // Missing leaf: canonicalize the first existing ancestor so a parent
    // symlink out of home (`~/out` -> `/tmp`) cannot pass a logical prefix.
    if let Some(resolved) = resolve_via_existing_ancestor(path) {
        return resolved.starts_with(&home_canon);
    }
    // No existing ancestor: keep the logical-prefix fallback
    // (macOS /var/folders -> /private/var/folders).
    if let Ok(rest) = path.strip_prefix(home) {
        return home_canon.join(rest).starts_with(&home_canon);
    }
    path.strip_prefix(&home_canon).is_ok()
}

fn resolve_via_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            let mut resolved = std::fs::canonicalize(&current).ok()?;
            for part in suffix.iter().rev() {
                resolved.push(part);
            }
            return Some(resolved);
        }
        let name = current.file_name()?.to_os_string();
        suffix.push(name);
        let parent = current.parent()?;
        if parent.as_os_str().is_empty() || parent == current {
            return None;
        }
        current = parent.to_path_buf();
    }
}

fn creds_path_escapes_home() -> AuthError {
    AuthError::TokenProvider("oauth.creds_path must stay under home".into())
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub(crate) fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(char::from(b"0123456789ABCDEF"[(byte >> 4) as usize]));
                out.push(char::from(b"0123456789ABCDEF"[(byte & 0x0F) as usize]));
            }
        }
    }
    out
}

pub(crate) fn format_rfc3339_now_plus(seconds: u64) -> String {
    let seconds = seconds.min(MAX_EXPIRES_IN_SECS);
    let total_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_add(seconds);
    format_rfc3339_epoch(total_secs)
}

pub(crate) fn format_rfc3339_epoch(total_secs: u64) -> String {
    let secs_per_day: u64 = 86400;
    let mut days = (total_secs / secs_per_day) as i64;
    let day_secs = total_secs % secs_per_day;
    let hour = day_secs / 3600;
    let min = (day_secs % 3600) / 60;
    let sec = day_secs % 60;

    days += 719468;
    let era = days.div_euclid(146097);
    let doe = days.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

pub(crate) fn parse_rfc3339(s: &str) -> Result<std::time::SystemTime, AuthError> {
    let s = s.trim();
    if s.len() < 19 {
        return Err(AuthError::TokenProvider("date too short".into()));
    }
    if !s.is_ascii() {
        return Err(AuthError::TokenProvider("non-ASCII date".into()));
    }
    let year: i32 = s[0..4]
        .parse()
        .map_err(|e| AuthError::TokenProvider(format!("year: {e}")))?;
    let month: u32 = s[5..7]
        .parse()
        .map_err(|e| AuthError::TokenProvider(format!("month: {e}")))?;
    let day: u32 = s[8..10]
        .parse()
        .map_err(|e| AuthError::TokenProvider(format!("day: {e}")))?;
    let hour: u32 = s[11..13]
        .parse()
        .map_err(|e| AuthError::TokenProvider(format!("hour: {e}")))?;
    let min: u32 = s[14..16]
        .parse()
        .map_err(|e| AuthError::TokenProvider(format!("min: {e}")))?;
    let sec: u32 = s[17..19]
        .parse()
        .map_err(|e| AuthError::TokenProvider(format!("sec: {e}")))?;
    if !(1..=12).contains(&month) {
        return Err(AuthError::TokenProvider(format!(
            "month {month} out of range"
        )));
    }
    if !(1..=31).contains(&day) {
        return Err(AuthError::TokenProvider(format!("day {day} out of range")));
    }
    if hour > 23 || min > 59 || sec > 59 {
        return Err(AuthError::TokenProvider("time out of range".into()));
    }
    let days = days_since_epoch(year, month, day);
    let base_secs = days * 86400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec);
    let tz_start = if s.len() > 19 && s.as_bytes()[19] == b'.' {
        s[20..].find(['Z', '+', '-']).map(|p| p + 20)
    } else if s.len() > 19 {
        Some(19)
    } else {
        None
    };
    let offset_secs: i64 = match tz_start {
        Some(pos) if pos < s.len() && s.as_bytes()[pos] == b'Z' => 0,
        Some(pos) if pos + 5 <= s.len() => {
            let sign: i64 = if s.as_bytes()[pos] == b'+' { 1 } else { -1 };
            let oh: i64 = s[pos + 1..pos + 3]
                .parse()
                .map_err(|e| AuthError::TokenProvider(format!("tz hour: {e}")))?;
            let om_start = if s.as_bytes()[pos + 3] == b':' {
                pos + 4
            } else {
                pos + 3
            };
            if om_start.saturating_add(2) > s.len() {
                return Err(AuthError::TokenProvider("tz min too short".into()));
            }
            let om: i64 = s[om_start..om_start + 2]
                .parse()
                .map_err(|e| AuthError::TokenProvider(format!("tz min: {e}")))?;
            sign * (oh * 3600 + om * 60)
        }
        _ => 0,
    };
    let total_secs = (base_secs - offset_secs).max(0) as u64;
    Ok(std::time::UNIX_EPOCH + Duration::from_secs(total_secs))
}

fn days_since_epoch(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(if month <= 2 { year - 1 } else { year });
    let m = if month <= 2 {
        i64::from(month) + 9
    } else {
        i64::from(month) - 3
    };
    let d = i64::from(day);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub(crate) fn remaining_from_system_time(expiry: std::time::SystemTime) -> Duration {
    match expiry.duration_since(std::time::SystemTime::now()) {
        Ok(remaining) => remaining,
        Err(_) => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_drops_raw_tokens() {
        let body = r#"{"error":"invalid_grant","refresh_token":"rt-LEAK","access_token":"sk-ant-oat01-LEAK","error_description":"revoked sk-ant-oat01-LEAK"}"#;
        let summary = sanitize_oauth_error_body(body);
        assert!(!summary.contains("sk-ant-oat01-LEAK"));
        assert!(!summary.contains("rt-LEAK"));
        assert!(summary.contains("invalid_grant"));
    }

    #[test]
    fn redact_secret_looking_strips_github_pats() {
        let text = redact_secret_looking("paste ghp_ENVSUBST_LEAK_TOKEN_51 into the vendor CLI");
        assert!(!text.contains("ghp_ENVSUBST_LEAK_TOKEN_51"), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
        assert!(text.contains("paste"), "{text}");
    }

    #[test]
    fn redact_url_origin_drops_userinfo_path_and_query() {
        let redacted = redact_url_origin(
            "https://user:s3cret@auth.example.invalid/oauth/token?client_secret=supersecret#frag",
        );
        assert_eq!(redacted, "https://auth.example.invalid");
        assert!(!redacted.contains("s3cret"));
        assert!(!redacted.contains("supersecret"));
        assert!(!redacted.contains("/oauth"));
    }

    #[test]
    fn format_oauth_transport_error_redacts_userinfo_and_client_secret() {
        let msg = format_oauth_transport_via(
            "auth code exchange failed",
            "connect",
            "https://user:s3cret@auth.example.invalid/oauth/token?client_secret=supersecret",
        );
        assert!(
            msg.contains("via https://auth.example.invalid"),
            "transport error must name the redacted origin, got {msg}"
        );
        assert!(!msg.contains("s3cret"), "{msg}");
        assert!(!msg.contains("client_secret="), "{msg}");
        assert!(!msg.contains("supersecret"), "{msg}");
        assert!(!msg.contains("user:"), "{msg}");
        assert!(!msg.contains("/oauth"), "{msg}");
    }

    #[test]
    fn vendor_rejected_summary_omits_http_status() {
        let summary = vendor_rejected_summary(
            "Azure token request failed",
            r#"{"error":"invalid_client"}"#,
            "https://login.microsoftonline.com/t/oauth2/v2.0/token",
        );
        assert!(
            !summary.contains("HTTP"),
            "summary must not re-wrap HTTP status, got {summary}"
        );
        assert!(summary.contains("invalid_client"), "{summary}");
        assert!(summary.contains("login.microsoftonline.com"), "{summary}");
        let xml = vendor_rejected_summary(
            "STS AssumeRole failed",
            "<ErrorResponse><Error><Code>AccessDenied</Code></Error></ErrorResponse>",
            "https://sts.amazonaws.com/",
        );
        assert!(!xml.contains("HTTP"), "{xml}");
        assert!(xml.contains("AssumeRole"), "{xml}");
    }

    #[test]
    fn format_oauth_http_error_includes_redacted_host() {
        let msg = format_oauth_http_error(
            "token refresh failed",
            401,
            r#"{"error":"invalid_grant"}"#,
            "https://user:s3cret@auth.example.invalid/oauth/token?client_secret=supersecret",
        );
        assert!(
            msg.contains("via https://auth.example.invalid"),
            "HTTP error must name the redacted origin, got {msg}"
        );
        assert!(!msg.contains("s3cret"), "{msg}");
        assert!(!msg.contains("supersecret"), "{msg}");
        assert!(!msg.contains("/oauth"), "{msg}");
        let fallback = format_oauth_http_error(
            "token refresh failed",
            404,
            "",
            "https://fallback.example.invalid/v1/oauth/token",
        );
        assert!(
            fallback.contains("via https://fallback.example.invalid"),
            "fallback host must be distinguishable, got {fallback}"
        );
    }

    #[test]
    fn percent_encode_reserved() {
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("ok-._~"), "ok-._~");
    }

    #[test]
    fn expand_tilde_uses_home() {
        let prev = std::env::var_os("HOME");
        // IsolatedHome owns HOME in other tests; this unit test only
        // checks the join, not process-global isolation.
        let expanded = expand_tilde("~/foo/bar");
        if let Some(home) = home_dir() {
            assert_eq!(expanded, home.join("foo/bar"));
        }
        drop(prev);
    }

    #[test]
    fn jail_accepts_canonical_path_when_home_is_a_symlink() {
        let home = crate::isolated_home::IsolatedHome::new();
        let planted = home.plant_credentials(crate::isolated_home::PlantCredentials::JsonPointer {
            relative_path: ".config/github-copilot/hosts.json",
            document: serde_json::json!({"github.com": {"oauth_token": "ghu"}}),
        });
        let canon = std::fs::canonicalize(&planted).expect("canonicalize planted");
        jail_creds_path(&planted).expect("logical plant path");
        jail_creds_path(&canon).expect("canonical path under IsolatedHome");
        let missing = home.path().join(".claude/.credentials.json");
        jail_creds_path(&missing).expect("missing file still under IsolatedHome");
    }

    #[cfg(unix)]
    #[test]
    fn jail_rejects_parent_symlink_out_of_home() {
        let home = crate::isolated_home::IsolatedHome::new();
        let outside = tempfile::tempdir().expect("outside home");
        let link = home.path().join("out");
        std::os::unix::fs::symlink(outside.path(), &link).expect("symlink out of home");
        let stolen = home.path().join("out/stolen.json");
        jail_creds_path(&stolen).expect_err("creds_path must stay under home");

        let ok_dir = home.path().join("ok");
        std::fs::create_dir_all(&ok_dir).expect("mkdir ok");
        let missing = ok_dir.join("missing.json");
        jail_creds_path(&missing).expect("missing file under a real in-home dir");
    }
}

//! Shared CLI helpers: profile flag, validate, login, status.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::time::Duration;

use wiremux_auth::{
    AuthScheme, LoadOptions, Login, OauthPack, ProfileError, ResolvedProfile, TokenProvider, Wire,
    load_profile_from_cli, persist_login_tokens, provider_from_profile,
};

/// Process exit: success.
pub const EXIT_OK: i32 = 0;
/// Process exit: hard error.
pub const EXIT_ERROR: i32 = 1;
/// Process exit: not-ready (setup-token / empty client_id / login=none).
pub const EXIT_NOT_READY: i32 = 2;

/// Load `--profile` as an id or a file path.
pub fn load_cli_profile(profile_arg: &str) -> Result<ResolvedProfile, ProfileError> {
    load_profile_from_cli(profile_arg, &LoadOptions::default())
}

/// Gist lint output: resolved URLs with secrets redacted.
pub fn validate_report(profile: &ResolvedProfile) -> String {
    let mut lines = Vec::new();
    lines.push(format!("id: {}", profile.id));
    lines.push(format!("schema_version: {}", profile.schema_version));
    if let Some(wire) = profile.dialect.wire {
        lines.push(format!("wire: {}", wire_name(wire)));
    }
    push_url(&mut lines, "base_url", profile.http.base_url.as_deref());
    push_url(&mut lines, "chat_path", profile.http.chat_path.as_deref());
    if let Some(oauth) = &profile.oauth {
        push_url(
            &mut lines,
            "oauth.token_url",
            Some(oauth.token_url.as_str()),
        );
        push_url(
            &mut lines,
            "oauth.token_url_fallback",
            oauth.token_url_fallback.as_deref(),
        );
        push_url(
            &mut lines,
            "oauth.authorize_url",
            oauth.authorize_url.as_deref(),
        );
        push_url(
            &mut lines,
            "oauth.device_auth_url",
            oauth.device_auth_url.as_deref(),
        );
        push_url(
            &mut lines,
            "oauth.redirect_uri",
            oauth.redirect_uri.as_deref(),
        );
    }
    lines.join("\n")
}

fn push_url(lines: &mut Vec<String>, field: &str, value: Option<&str>) {
    let Some(value) = value.filter(|s| !s.is_empty()) else {
        return;
    };
    lines.push(format!("{field}: {}", redact_secret_url(value)));
}

/// Redact userinfo, query values, fragments, and secret-looking tokens.
pub fn redact_secret_url(raw: &str) -> String {
    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, raw),
    };
    // Split host from path, query, or fragment. `https://host?key=s` has no `/`.
    let cut = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..cut];
    let tail = &rest[cut..];
    let authority = if let Some(at) = authority.find('@') {
        format!("[redacted]@{}", &authority[at + 1..])
    } else {
        authority.to_string()
    };
    let mut out = match scheme {
        Some(scheme) => format!("{scheme}://{authority}"),
        None => authority,
    };
    let (path, query, frag) = split_url_tail(tail);
    if let Some(path) = path {
        out.push('/');
        out.push_str(path);
    }
    if let Some(query) = query {
        out.push('?');
        out.push_str(&redact_query(query));
    }
    if frag.is_some() {
        out.push_str("#[redacted]");
    }
    redact_secret_looking(&out)
}

fn split_url_tail(tail: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    if tail.is_empty() {
        return (None, None, None);
    }
    let (before_hash, frag) = match tail.split_once('#') {
        Some((before, frag)) => (before, Some(frag)),
        None => (tail, None),
    };
    let (before_query, query) = match before_hash.split_once('?') {
        Some((before, query)) => (before, Some(query)),
        None => (before_hash, None),
    };
    let path = before_query.strip_prefix('/');
    (path, query, frag)
}

fn redact_query(query: &str) -> String {
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _)) => format!("{k}=[redacted]"),
            None => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn redact_secret_looking(s: &str) -> String {
    let mut out = redact_prefix(s, "sk-");
    out = redact_prefix(&out, "eyJ");
    redact_prefix(&out, "rt-")
}

fn redact_prefix(s: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find(prefix) {
        out.push_str(&rest[..idx]);
        out.push_str("[redacted]");
        rest = &rest[idx + prefix.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// What `auth login` should do for this profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginPlan {
    /// Print hint and exit 2.
    SetupToken {
        /// Free-text hint from the profile.
        hint: String,
    },
    /// Print why and exit 2.
    NotReady {
        /// Human-readable reason.
        reason: String,
    },
    /// Try loopback PKCE (device fallback if bind fails).
    Pkce,
    /// Device authorization grant.
    Device,
}

/// Decide login without talking to a vendor.
pub fn login_plan(profile: &ResolvedProfile) -> LoginPlan {
    let Some(oauth) = profile.oauth.as_ref() else {
        return LoginPlan::NotReady {
            reason: "profile has no [oauth] table".into(),
        };
    };
    let client_id = oauth.client_id.as_deref().unwrap_or("");
    let login = oauth.login.unwrap_or(Login::None);
    match login {
        Login::SetupToken => LoginPlan::SetupToken {
            hint: oauth
                .setup_token_hint
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "run the vendor setup-token command".into()),
        },
        Login::None => {
            let reason = if client_id.is_empty() {
                "login is none and client_id is empty (set a wiremux client id and login=pkce)"
                    .into()
            } else {
                "login is none".into()
            };
            LoginPlan::NotReady { reason }
        }
        Login::Pkce | Login::Device => {
            if client_id.is_empty() {
                LoginPlan::NotReady {
                    reason: "client_id is empty after subst".into(),
                }
            } else if matches!(login, Login::Device) {
                LoginPlan::Device
            } else {
                LoginPlan::Pkce
            }
        }
    }
}

/// Run `auth login`. Returns a process exit code.
pub async fn run_login(profile: &ResolvedProfile) -> i32 {
    match login_plan(profile) {
        LoginPlan::SetupToken { hint } => {
            println!("{hint}");
            EXIT_NOT_READY
        }
        LoginPlan::NotReady { reason } => {
            eprintln!("{reason}");
            EXIT_NOT_READY
        }
        LoginPlan::Pkce => match run_pkce_or_device(profile).await {
            Ok(()) => EXIT_OK,
            Err(err) => {
                eprintln!("{err}");
                EXIT_ERROR
            }
        },
        LoginPlan::Device => match run_device(profile).await {
            Ok(()) => EXIT_OK,
            Err(err) => {
                eprintln!("{err}");
                EXIT_ERROR
            }
        },
    }
}

async fn run_pkce_or_device(profile: &ResolvedProfile) -> Result<(), String> {
    let oauth = profile.oauth.as_ref().ok_or("missing [oauth]")?;
    let redirect = oauth
        .redirect_uri
        .as_deref()
        .ok_or("oauth.redirect_uri is required for PKCE")?;
    match bind_redirect(redirect) {
        Ok(listener) => run_pkce(oauth, listener).await,
        Err(bind_err) => {
            if oauth.device_auth_url.is_some() {
                eprintln!("loopback bind failed ({bind_err}); falling back to device flow");
                run_device(profile).await
            } else {
                Err(format!(
                    "loopback bind failed on {redirect} ({bind_err}); device_auth_url not set"
                ))
            }
        }
    }
}

fn bind_redirect(redirect_uri: &str) -> Result<TcpListener, String> {
    let addr = redirect_bind_addr(redirect_uri)?;
    TcpListener::bind(addr).map_err(|e| e.to_string())
}

/// Loopback host + port from a redirect URI. Only 127.0.0.1 / localhost.
pub fn redirect_bind_addr(redirect_uri: &str) -> Result<SocketAddr, String> {
    let rest = redirect_uri
        .strip_prefix("http://")
        .or_else(|| redirect_uri.strip_prefix("https://"))
        .ok_or_else(|| format!("redirect_uri must be http(s): {redirect_uri}"))?;
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => (hostport, "80"),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host != "127.0.0.1" && !host.eq_ignore_ascii_case("localhost") {
        return Err(format!(
            "PKCE redirect_uri host must be 127.0.0.1 or localhost, got {host}"
        ));
    }
    let port: u16 = port
        .parse()
        .map_err(|_| format!("invalid redirect_uri port: {port}"))?;
    Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

async fn run_pkce(oauth: &OauthPack, listener: TcpListener) -> Result<(), String> {
    use wiremux_auth::pkce::{build_auth_url_from_oauth, exchange_auth_code, generate_pkce};

    let pkce = generate_pkce().map_err(|e| e.to_string())?;
    let url = build_auth_url_from_oauth(oauth, &pkce).map_err(|e| e.to_string())?;
    println!("Open this URL to authorize:\n{url}");
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    println!("Waiting for loopback callback on {local}");
    let _ = std::io::stdout().flush();
    listener.set_nonblocking(false).map_err(|e| e.to_string())?;
    let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5 * 60)))
        .ok();
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let first = req.lines().next().unwrap_or("");
    let target = first.split_whitespace().nth(1).unwrap_or("");
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "code" => code = Some(url_decode(v)),
                "state" => state = Some(url_decode(v)),
                _ => {}
            }
        }
    }
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nok, you can close this tab\n",
    );
    let code = code.ok_or("callback missing code")?;
    let state = state.ok_or("callback missing state")?;
    if state != pkce.state {
        return Err("callback state mismatch".into());
    }
    let redirect = oauth.redirect_uri.as_deref().unwrap_or("");
    let client_id = oauth.client_id.as_deref().unwrap_or("");
    let tokens = exchange_auth_code(
        &oauth.token_url,
        client_id,
        &code,
        redirect,
        &pkce.code_verifier,
    )
    .await
    .map_err(|e| e.to_string())?;
    persist_login_tokens(oauth, &tokens)
        .await
        .map_err(|e| e.to_string())?;
    println!("login saved");
    Ok(())
}

async fn run_device(profile: &ResolvedProfile) -> Result<(), String> {
    use wiremux_auth::device_flow::{poll_device_token, start_device_flow_from_oauth};

    let oauth = profile.oauth.as_ref().ok_or("missing [oauth]")?;
    let started = start_device_flow_from_oauth(oauth)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "Visit {} and enter {}",
        started.verification_uri, started.user_code
    );
    if let Some(complete) = started.verification_uri_complete.as_deref() {
        println!("or open {complete}");
    }
    let _ = std::io::stdout().flush();
    let client_id = oauth.client_id.as_deref().unwrap_or("");
    let headers: Vec<(&str, &str)> = oauth
        .token_headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let tokens = poll_device_token(
        &oauth.token_url,
        client_id,
        &started.device_code,
        started.interval,
        started.expires_in,
        if headers.is_empty() {
            None
        } else {
            Some(&headers)
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    persist_login_tokens(oauth, &tokens)
        .await
        .map_err(|e| e.to_string())?;
    println!("login saved");
    Ok(())
}

fn url_decode(s: &str) -> String {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v as char);
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

/// Whether a token can be loaded. Never includes the token value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenStatus {
    /// Catalog id.
    pub id: String,
    /// True when a credential source yielded a token, or auth is none.
    pub available: bool,
    /// Short reason when unavailable.
    pub detail: String,
}

/// Check local credential sources. Does not print secrets.
pub fn token_status(profile: &ResolvedProfile) -> TokenStatus {
    if matches!(profile.http.auth_scheme, Some(AuthScheme::None)) && profile.oauth.is_none() {
        return TokenStatus {
            id: profile.id.clone(),
            available: true,
            detail: "auth_scheme is none".into(),
        };
    }
    if profile
        .http
        .headers
        .iter()
        .any(|(k, v)| is_auth_header(k) && !v.trim().is_empty())
    {
        return TokenStatus {
            id: profile.id.clone(),
            available: true,
            detail: "header credential present".into(),
        };
    }
    if let Some(oauth) = &profile.oauth {
        match provider_from_profile(profile) {
            Ok(_) => {
                return TokenStatus {
                    id: profile.id.clone(),
                    available: true,
                    detail: "credentials loaded".into(),
                };
            }
            Err(err) => {
                let _ = oauth;
                return TokenStatus {
                    id: profile.id.clone(),
                    available: false,
                    detail: err.to_string(),
                };
            }
        }
    }
    TokenStatus {
        id: profile.id.clone(),
        available: false,
        detail: "no credentials".into(),
    }
}

fn is_auth_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key")
}

/// Format status without leaking the token.
pub fn format_status(status: &TokenStatus) -> String {
    if status.available {
        format!("profile: {}\ntoken: available", status.id)
    } else {
        format!(
            "profile: {}\ntoken: unavailable\n{}",
            status.id, status.detail
        )
    }
}

/// `127.0.0.1` only (ephemeral port allowed).
pub fn parse_listen(s: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = s
        .parse()
        .map_err(|e| format!("invalid --listen {s}: {e}"))?;
    match addr.ip() {
        IpAddr::V4(ip) if ip == Ipv4Addr::LOCALHOST => Ok(addr),
        _ => Err("proxy listen address must be 127.0.0.1".into()),
    }
}

/// Parse `--from` dialect name.
pub fn parse_wire(s: &str) -> Result<Wire, String> {
    match s {
        "responses" => Ok(Wire::Responses),
        "messages" => Ok(Wire::Messages),
        "chat-completions" | "chat" => Ok(Wire::ChatCompletions),
        "gemini" => Ok(Wire::Gemini),
        other => Err(format!(
            "unknown --from `{other}` (responses|messages|chat-completions|gemini)"
        )),
    }
}

pub fn wire_name(wire: Wire) -> &'static str {
    match wire {
        Wire::ChatCompletions => "chat-completions",
        Wire::Messages => "messages",
        Wire::Responses => "responses",
        Wire::Gemini => "gemini",
    }
}

/// Resolve an API token for the proxy. Empty means send no auth header.
pub async fn proxy_token(profile: &ResolvedProfile) -> Result<Option<String>, String> {
    if matches!(profile.http.auth_scheme, Some(AuthScheme::None)) && profile.oauth.is_none() {
        return Ok(None);
    }
    if let Some((_, value)) = profile
        .http
        .headers
        .iter()
        .find(|(k, v)| is_auth_header(k) && !v.trim().is_empty())
    {
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .unwrap_or(value)
            .to_string();
        return Ok(Some(token));
    }
    if profile.oauth.is_some() {
        let provider = provider_from_profile(profile).map_err(|e| e.to_string())?;
        let token = provider.get_token().await.map_err(|e| e.to_string())?;
        return Ok(Some(token));
    }
    Ok(None)
}

/// Join `base_url` + `chat_path` for the upstream request.
pub fn upstream_url(profile: &ResolvedProfile) -> Result<String, String> {
    upstream_url_for_model(profile, None)
}

/// Join `base_url` + `chat_path`, substituting `{model}` when present.
pub fn upstream_url_for_model(
    profile: &ResolvedProfile,
    model: Option<&str>,
) -> Result<String, String> {
    let base = profile
        .http
        .base_url
        .as_deref()
        .ok_or("profile has no base_url")?;
    let path = profile
        .http
        .chat_path
        .as_deref()
        .or_else(|| profile.dialect.wire.map(Wire::default_chat_path))
        .unwrap_or("/");
    let path = if let Some(model) = model.filter(|m| !m.is_empty()) {
        path.replace("{model}", model)
    } else {
        path.to_string()
    };
    if path.starts_with("http://") || path.starts_with("https://") {
        return Ok(path);
    }
    Ok(format!(
        "{}{}",
        base.trim_end_matches('/'),
        if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremux_auth::parse_profile_str;

    #[test]
    fn redact_url_strips_userinfo_and_query() {
        let redacted = redact_secret_url(
            "https://user:s3cret@api.example.invalid/v1?api_key=supersecret#frag",
        );
        assert!(!redacted.contains("s3cret"));
        assert!(!redacted.contains("supersecret"));
        assert!(!redacted.contains("#frag"));
        assert!(redacted.contains("[redacted]"));
        assert!(redacted.contains("https://"));
        assert!(redacted.contains("api.example.invalid/v1"));
    }

    #[test]
    fn redact_url_query_and_fragment_without_path() {
        let query_only = redact_secret_url("https://auth.example.invalid?api_key=supersecret");
        assert!(
            !query_only.contains("supersecret"),
            "query leaked: {query_only}"
        );
        assert!(query_only.contains("https://auth.example.invalid"));
        assert!(query_only.contains("api_key=[redacted]"), "{query_only}");

        let frag_only = redact_secret_url("https://auth.example.invalid#token=s3cret");
        assert!(
            !frag_only.contains("s3cret"),
            "fragment leaked: {frag_only}"
        );
        assert!(frag_only.ends_with("#[redacted]"), "{frag_only}");
    }

    fn shipped(id: &str) -> ResolvedProfile {
        wiremux_auth::load_profile(
            id,
            &LoadOptions {
                include_user_config: false,
                ..LoadOptions::default()
            },
        )
        .expect("shipped")
    }

    #[test]
    fn login_plan_anthropic_is_setup_token() {
        let profile = shipped("anthropic-oauth");
        match login_plan(&profile) {
            LoginPlan::SetupToken { hint } => {
                assert!(hint.contains("claude setup-token"));
            }
            other => panic!("expected setup-token, got {other:?}"),
        }
    }

    #[test]
    fn login_plan_openai_is_not_ready() {
        let profile = shipped("openai-codex-oauth");
        match login_plan(&profile) {
            LoginPlan::NotReady { reason } => {
                assert!(reason.contains("client_id") || reason.contains("login"));
            }
            other => panic!("expected not-ready, got {other:?}"),
        }
    }

    #[test]
    fn parse_wire_accepts_gemini() {
        assert_eq!(parse_wire("gemini").unwrap(), Wire::Gemini);
        assert_eq!(wire_name(Wire::Gemini), "gemini");
    }

    #[test]
    fn gemini_upstream_url_substitutes_model() {
        let profile = parse_profile_str(
            r#"
schema_version = 1
id = "g"
wire = "gemini"
base_url = "https://generativelanguage.googleapis.com"
"#,
        )
        .expect("parse");
        let url = upstream_url_for_model(&profile, Some("gemini-2.5-flash")).expect("url");
        assert!(
            url.ends_with("/v1beta/models/gemini-2.5-flash:generateContent"),
            "{url}"
        );
    }

    #[test]
    fn parse_listen_rejects_wildcard() {
        assert!(parse_listen("0.0.0.0:0").is_err());
        assert!(parse_listen("127.0.0.1:0").is_ok());
    }

    #[test]
    fn token_status_none_auth_is_available() {
        let profile = parse_profile_str(
            r#"
schema_version = 1
id = "none-auth"
wire = "chat-completions"
auth_scheme = "none"
base_url = "http://127.0.0.1:9"
"#,
        )
        .expect("parse");
        let status = token_status(&profile);
        assert!(status.available);
        let text = format_status(&status);
        assert!(!text.contains("http://"));
    }
}

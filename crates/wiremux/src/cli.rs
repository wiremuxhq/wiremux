//! Shared CLI helpers: profile flag, validate, login, status.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::time::Duration;

use wiremux_auth::{
    AuthScheme, LoadOptions, Login, OauthPack, ProfileError, ResolvedProfile, TokenProvider, Wire,
    load_profile_from_cli, persist_login_tokens, provider_from_profile, redact_secret_looking,
    redact_url_origin, sanitize_oauth_error_text,
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
    lines.push(format!("{field}: {}", redact_printed_url(value)));
}

fn redact_printed_url(raw: &str) -> String {
    if raw.contains("://") {
        redact_secret_looking(&redact_url_origin(raw))
    } else {
        redact_secret_looking(raw)
    }
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
            hint: redact_secret_looking(
                &oauth
                    .setup_token_hint
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "run the vendor setup-token command".into()),
            ),
        },
        Login::None => {
            let mut reason = String::from(
                "oauth.login is none (set pkce or device; shipped openai-codex-oauth stays none until an overlay sets login)",
            );
            if client_id.is_empty() {
                reason.push_str(" and client_id is empty (set a wiremux client id)");
            }
            LoginPlan::NotReady { reason }
        }
        Login::Pkce | Login::Device => {
            if client_id.is_empty() {
                LoginPlan::NotReady {
                    reason:
                        "oauth.client_id is empty after subst (set it in the profile or overlay)"
                            .into(),
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
        LoginPlan::Device => {
            if let Some(note) = copilot_tos_note(profile) {
                println!("{note}");
            }
            match run_device(profile).await {
                Ok(()) => EXIT_OK,
                Err(err) => {
                    eprintln!("{err}");
                    EXIT_ERROR
                }
            }
        }
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
    let parsed = pkce_callback_from_query(query);
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nok, you can close this tab\n",
    );
    let (code, state) = parsed?;
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

fn copilot_tos_note(profile: &ResolvedProfile) -> Option<String> {
    let oauth = profile.oauth.as_ref()?;
    if oauth.creds_format != Some(wiremux_auth::CredsFormat::CopilotHosts) {
        return None;
    }
    Some("GitHub Copilot product terms apply to tokens obtained with this profile.".into())
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

/// Read `code`/`state` from a PKCE loopback query. Surfaces vendor `error`.
pub(crate) fn pkce_callback_from_query(query: &str) -> Result<(String, String), String> {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "code" => code = Some(url_decode(v)),
                "state" => state = Some(url_decode(v)),
                "error" => error = Some(url_decode(v)),
                "error_description" => error_description = Some(url_decode(v)),
                _ => {}
            }
        }
    }
    let code_ok = code.as_ref().is_some_and(|c| !c.is_empty());
    let state_ok = state.as_ref().is_some_and(|s| !s.is_empty());
    if code_ok && state_ok {
        return Ok((code.unwrap(), state.unwrap()));
    }
    if error.is_some() || error_description.is_some() {
        return Err(format_pkce_vendor_error(
            error.as_deref(),
            error_description.as_deref(),
        ));
    }
    if !code_ok {
        return Err("callback missing code".into());
    }
    Err("callback missing state".into())
}

fn format_pkce_vendor_error(error: Option<&str>, description: Option<&str>) -> String {
    let err = error.filter(|s| !s.is_empty()).unwrap_or("error");
    let raw = match description.filter(|s| !s.is_empty()) {
        Some(desc) => format!("{err}: {desc}"),
        None => err.to_owned(),
    };
    let summary = sanitize_oauth_error_text(&raw);
    if summary.is_empty() {
        "callback error".into()
    } else {
        format!("callback error {summary}")
    }
}

pub(crate) fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &bytes[i + 1..i + 3];
                if let Ok(hex) = std::str::from_utf8(hex)
                    && let Ok(v) = u8::from_str_radix(hex, 16)
                {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
                    detail: redact_secret_looking(&err.to_string()),
                };
            }
        }
    }
    TokenStatus {
        id: profile.id.clone(),
        available: false,
        detail: "no credentials (set [headers] Authorization or x-api-key, [oauth], or auth_scheme = \"none\")".into(),
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
        other => {
            let listed = "responses|messages|chat-completions|gemini";
            let mut msg = format!("unknown --from `{other}` ({listed})");
            if let Some(suggest) = Wire::suggest(other) {
                msg.push_str(&format!("; did you mean `{suggest}`"));
            }
            Err(msg)
        }
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
    upstream_url_for_model(profile, None, false)
}

/// Join `base_url` + `chat_path`, substituting `{model}` when present.
///
/// Gemini streaming uses `:streamGenerateContent?alt=sse` when the path
/// is the unary `:generateContent` default.
pub fn upstream_url_for_model(
    profile: &ResolvedProfile,
    model: Option<&str>,
    stream: bool,
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
    let mut path = if let Some(model) = model.filter(|m| !m.is_empty()) {
        path.replace("{model}", model)
    } else {
        path.to_string()
    };
    if stream
        && matches!(profile.dialect.wire, Some(Wire::Gemini))
        && path.ends_with(":generateContent")
    {
        path = path.replacen(":generateContent", ":streamGenerateContent?alt=sse", 1);
    }
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
    fn redact_printed_url_keeps_origin_only() {
        let redacted = redact_printed_url(
            "https://user:s3cret@api.example.invalid/v1?api_key=supersecret#frag",
        );
        assert_eq!(redacted, "https://api.example.invalid");
        assert!(!redacted.contains("s3cret"));
        assert!(!redacted.contains("supersecret"));
        assert!(!redacted.contains("/v1"));
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
    fn login_plan_copilot_gist_device_needs_client_id() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../wiremux-auth/tests/gists/github-copilot-device.toml");
        let profile =
            parse_profile_str(&std::fs::read_to_string(&path).expect("gist")).expect("parse");
        match login_plan(&profile) {
            LoginPlan::NotReady { reason } => {
                assert!(
                    reason.contains("oauth.client_id"),
                    "device/pkce empty client_id must name oauth.client_id, got {reason}"
                );
                assert!(
                    reason.contains("empty after subst"),
                    "must say empty after subst, got {reason}"
                );
                assert!(
                    reason.contains("profile") && reason.contains("overlay"),
                    "must say set it in the profile or overlay, got {reason}"
                );
            }
            other => panic!("empty client_id must be not-ready, got {other:?}"),
        }
        assert_eq!(
            profile.oauth.as_ref().and_then(|o| o.creds_format),
            Some(wiremux_auth::CredsFormat::CopilotHosts)
        );
        assert!(
            copilot_tos_note(&profile)
                .unwrap()
                .contains("Copilot product terms")
        );
    }

    #[test]
    fn login_plan_openai_is_not_ready() {
        let profile = shipped("openai-codex-oauth");
        match login_plan(&profile) {
            LoginPlan::NotReady { reason } => {
                assert_login_none_names_field_and_values(&reason);
                assert!(
                    reason.contains("client_id"),
                    "openai login plan must keep the empty-client_id hint, got {reason}"
                );
            }
            other => panic!("expected not-ready, got {other:?}"),
        }
    }

    #[test]
    fn login_plan_none_with_client_id_names_oauth_login() {
        let profile = parse_profile_str(
            r#"
schema_version = 1
id = "x"
[oauth]
token_url = "https://auth.example.invalid/token"
client_id = "already-set"
login = "none"
"#,
        )
        .expect("parse");
        match login_plan(&profile) {
            LoginPlan::NotReady { reason } => {
                assert_login_none_names_field_and_values(&reason);
                assert!(
                    !reason.contains("client_id is empty"),
                    "set client_id must not keep the empty-client_id hint, got {reason}"
                );
            }
            other => panic!("expected not-ready, got {other:?}"),
        }
    }

    fn assert_login_none_names_field_and_values(reason: &str) {
        assert!(
            reason.contains("oauth.login"),
            "login=none must name oauth.login, got {reason}"
        );
        assert!(
            reason.contains("pkce") && reason.contains("device"),
            "login=none must list pkce and device, got {reason}"
        );
        assert!(
            reason.contains("openai-codex-oauth"),
            "must say shipped openai-codex-oauth stays none, got {reason}"
        );
        assert!(
            reason.contains("overlay"),
            "must say overlay sets login, got {reason}"
        );
    }

    #[test]
    fn parse_wire_accepts_gemini() {
        assert_eq!(parse_wire("gemini").unwrap(), Wire::Gemini);
        assert_eq!(wire_name(Wire::Gemini), "gemini");
    }

    #[test]
    fn parse_wire_typo_suggests_close_matches() {
        assert_eq!(parse_wire("chat").unwrap(), Wire::ChatCompletions);

        for (input, want) in [
            ("chat_completions", "chat-completions"),
            ("ChatCompletions", "chat-completions"),
            ("response", "responses"),
        ] {
            let err = parse_wire(input).expect_err(input);
            assert!(
                err.contains(&format!("unknown --from `{input}`")),
                "{input} must echo the unknown value, got {err}"
            );
            assert!(
                err.contains("responses")
                    && err.contains("messages")
                    && err.contains("chat-completions")
                    && err.contains("gemini"),
                "{input} must list legal --from values, got {err}"
            );
            assert!(
                err.to_ascii_lowercase().contains("did you mean")
                    && err.contains(&format!("`{want}`")),
                "{input} should suggest {want}, got {err}"
            );
        }
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
        let url = upstream_url_for_model(&profile, Some("gemini-2.5-flash"), false).expect("url");
        assert!(
            url.ends_with("/v1beta/models/gemini-2.5-flash:generateContent"),
            "{url}"
        );
        let stream = upstream_url_for_model(&profile, Some("gemini-2.5-flash"), true).expect("url");
        assert!(
            stream.ends_with("/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"),
            "{stream}"
        );
    }

    #[test]
    fn parse_listen_rejects_wildcard() {
        assert!(parse_listen("0.0.0.0:0").is_err());
        let addr = parse_listen("127.0.0.1:0").expect("loopback ephemeral listen");
        assert_eq!(addr, "127.0.0.1:0".parse().expect("socket addr"));
    }

    #[test]
    fn token_status_no_credentials_names_what_to_set() {
        let profile = parse_profile_str(
            r#"
schema_version = 1
id = "no-creds"
wire = "chat-completions"
base_url = "http://127.0.0.1:9"
"#,
        )
        .expect("parse");
        let status = token_status(&profile);
        assert!(!status.available);
        let detail = &status.detail;
        assert!(detail.contains("no credentials"), "{detail}");
        assert!(
            detail.contains("[headers]")
                && detail.contains("Authorization")
                && detail.contains("x-api-key"),
            "must name [headers] Authorization or x-api-key, got {detail}"
        );
        assert!(
            detail.contains("[oauth]"),
            "must name [oauth], got {detail}"
        );
        assert!(
            detail.contains("auth_scheme") && detail.contains("none"),
            "must name auth_scheme = none, got {detail}"
        );
        let text = format_status(&status);
        assert!(text.contains("unavailable"), "{text}");
        assert!(text.contains("[headers]"), "{text}");
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

    #[test]
    fn format_pkce_vendor_error_redacts_secret_looking() {
        let sk = format_pkce_vendor_error(
            Some("invalid_request"),
            Some("rejected token sk-ant-oat01-LEAKED"),
        );
        assert!(!sk.contains("sk-ant-oat01-LEAKED"), "API key leaked: {sk}");
        assert!(sk.contains("invalid_request"), "{sk}");

        let jwt = format_pkce_vendor_error(
            Some("invalid_request"),
            Some("bad jwt eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIn0.sig"),
        );
        assert!(
            !jwt.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"),
            "{jwt}"
        );
        assert!(!jwt.contains("eyJzdWIiOiIxIn0"), "{jwt}");
    }

    #[test]
    fn pkce_callback_surfaces_vendor_error_and_description() {
        let err = pkce_callback_from_query("error=access_denied&error_description=user+denied")
            .expect_err("vendor error query");
        assert!(
            err.contains("access_denied"),
            "callback should surface vendor error, got {err}"
        );
        assert!(
            err.contains("user denied"),
            "callback should surface error_description, got {err}"
        );
    }

    #[test]
    fn pkce_callback_accepts_code_and_state() {
        let (code, state) = pkce_callback_from_query("code=abc&state=xyz").expect("code and state");
        assert_eq!(code, "abc");
        assert_eq!(state, "xyz");
    }

    #[test]
    fn url_decode_percent_then_multibyte_utf8_does_not_panic() {
        let decoded = url_decode("%完");
        assert_eq!(decoded, "%完");
        let callback = pkce_callback_from_query("code=%完&state=xyz");
        assert!(
            callback.is_ok(),
            "hostile query must not abort: {callback:?}"
        );
        assert_eq!(url_decode("%FF"), "\u{FFFD}");
        assert_eq!(url_decode("a+b%20c"), "a b c");
        assert_eq!(url_decode("%E2%82%AC"), "€");
    }
}

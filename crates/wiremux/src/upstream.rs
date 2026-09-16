//! Join profile `base_url` + `chat_path` (Gemini `{model}` + stream rewrite).

use wiremux_auth::{ResolvedProfile, Wire};

/// Shared miss text for `list_models` / `upstream_url_for_model`.
pub(crate) const MISSING_BASE_URL: &str =
    "profile has no base_url; set top-level `base_url` on the profile";

/// Join `base_url` + `chat_path`, substituting `{model}` when present.
///
/// Gemini streaming uses `:streamGenerateContent?alt=sse` when the path
/// is the unary `:generateContent` default.
pub fn upstream_url_for_model(
    profile: &ResolvedProfile,
    model: Option<&str>,
    stream: bool,
) -> Result<String, String> {
    let base = profile.http.base_url.as_deref().ok_or(MISSING_BASE_URL)?;
    let path = profile
        .http
        .chat_path
        .as_deref()
        .or_else(|| profile.dialect.wire.map(Wire::default_chat_path))
        .unwrap_or("/");
    let mut path = if let Some(model) = model.filter(|m| !m.is_empty()) {
        path.replace("{model}", &encode_model_segment(model))
    } else {
        path.to_string()
    };
    if stream
        && matches!(profile.dialect.wire, Some(Wire::Gemini))
        && path.ends_with(":generateContent")
    {
        path = path.replacen(":generateContent", ":streamGenerateContent?alt=sse", 1);
    }
    if stream
        && matches!(profile.dialect.wire, Some(Wire::Messages))
        && path.ends_with(":rawPredict")
    {
        path = path.replacen(":rawPredict", ":streamRawPredict", 1);
    }
    if stream && matches!(profile.dialect.wire, Some(Wire::Converse)) && path.ends_with("/converse")
    {
        path = format!("{path}-stream");
    }
    if path.starts_with("http://") || path.starts_with("https://") {
        return Ok(rewrite_vertex_global_host(path));
    }
    Ok(rewrite_vertex_global_host(format!(
        "{}{}",
        base.trim_end_matches('/'),
        if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        }
    )))
}

/// Encode `{model}` as one path segment. Keep `:` so Bedrock model ids
/// stay readable. Encode `/ ? # % \\` and controls so the name cannot
/// change the path, query, or fragment.
pub(crate) fn encode_model_segment(model: &str) -> String {
    let mut out = String::with_capacity(model.len());
    for byte in model.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' => {
                out.push(byte as char)
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

/// Ingest writes `https://{env:GOOGLE_VERTEX_LOCATION}-aiplatform.googleapis.com`.
/// After envsubst, `LOCATION=global` becomes `global-aiplatform.googleapis.com`.
/// Google's global host is `aiplatform.googleapis.com` (path keeps `/locations/global/`).
fn rewrite_vertex_global_host(url: String) -> String {
    url.replacen(
        "://global-aiplatform.googleapis.com",
        "://aiplatform.googleapis.com",
        1,
    )
}

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
        // `.` and `..` are one segment. URL parsers remove them, so
        // `/model/../converse` becomes `/converse` on the profile host.
        if model == "." || model == ".." {
            return Err("model is a dot segment and would change the upstream path".to_string());
        }
        path.replace("{model}", &encode_model_segment(model))
    } else {
        path.to_string()
    };
    if path.contains("{model}") {
        return Err("request has no model and the profile path still contains {model}".to_string());
    }
    // Model ids keep `:`, so the method name can sit inside the model
    // segment. Rewrite only the trailing method, not the first match.
    // A query or fragment is not part of that method.
    path = rewrite_stream_method(profile.dialect.wire, &path, stream);
    // Same absolute-URL rule as profile `check_url`: scheme case does not
    // matter. A lowercase prefix left `HTTPS://...` joined onto base_url.
    let path = path.trim().to_string();
    if is_absolute_http_url(&path) {
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

fn rewrite_stream_method(wire: Option<Wire>, path: &str, stream: bool) -> String {
    let (path_only, suffix) = split_path_suffix(path);
    if !stream {
        return format!("{path_only}{suffix}");
    }
    if matches!(wire, Some(Wire::Gemini))
        && let Some(prefix) = path_only.strip_suffix(":generateContent")
    {
        return format!("{prefix}:streamGenerateContent{}", with_alt_sse(suffix));
    }
    if matches!(wire, Some(Wire::Messages))
        && let Some(prefix) = path_only.strip_suffix(":rawPredict")
    {
        return format!("{prefix}:streamRawPredict{suffix}");
    }
    if matches!(wire, Some(Wire::Converse)) && path_only.ends_with("/converse") {
        return format!("{path_only}-stream{suffix}");
    }
    format!("{path_only}{suffix}")
}

fn split_path_suffix(path: &str) -> (&str, &str) {
    match path.find(['?', '#']) {
        Some(index) => (&path[..index], &path[index..]),
        None => (path, ""),
    }
}

/// Gemini streaming needs `alt=sse`. Keep an existing query and fragment.
fn with_alt_sse(suffix: &str) -> String {
    if suffix.is_empty() {
        return "?alt=sse".to_string();
    }
    let (query, fragment) = match suffix.split_once('#') {
        Some((query, fragment)) => (query, Some(fragment)),
        None => (suffix, None),
    };
    let mut out = if query.is_empty() {
        "?alt=sse".to_string()
    } else if query.split(['?', '&']).any(|part| part == "alt=sse") {
        query.to_string()
    } else {
        format!("{query}&alt=sse")
    };
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// Encode `{model}` as one path segment. Keep `:` so Bedrock model ids
/// stay readable. Encode `/ ? # % \\` and controls so the name cannot
/// change the path, query, or fragment.
fn is_absolute_http_url(path: &str) -> bool {
    let Some((scheme, _)) = path.split_once("://") else {
        return false;
    };
    scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
}

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

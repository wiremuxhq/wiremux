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

//! Data-only refuse scanners. Two walks: key names, then values.

use serde_json::Value;

use super::error::ProfileError;

const FORBIDDEN_KEYS: &[&str] = &[
    "fn",
    "function",
    "js",
    "javascript",
    "lua",
    "script",
    "command",
    "apply",
    "normalize",
];

/// Scan a parsed document for code-exec keys, interpolation, and bad URLs.
pub(crate) fn scan(value: &Value) -> Result<(), ProfileError> {
    scan_value(value, None, "")
}

fn scan_value(value: &Value, key: Option<&str>, path: &str) -> Result<(), ProfileError> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if FORBIDDEN_KEYS.iter().any(|f| k.eq_ignore_ascii_case(f)) {
                    return Err(ProfileError::ForbiddenKey(k.clone()));
                }
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                scan_value(v, Some(k.as_str()), &child)?;
            }
            Ok(())
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let child = format!("{path}[{i}]");
                scan_value(v, key, &child)?;
            }
            Ok(())
        }
        Value::String(s) => scan_string(s, key, path),
        _ => Ok(()),
    }
}

fn scan_string(s: &str, key: Option<&str>, path: &str) -> Result<(), ProfileError> {
    if is_native_module_path(s) {
        return Err(ProfileError::NativeModule {
            field: path.to_string(),
        });
    }
    let key = key.unwrap_or("");
    if is_url_field(key) {
        // `check_url` already accepts schemeless relative paths (`/v1/messages`).
        check_url(path, s)?;
    }
    refuse_interpolation(s, path)?;
    Ok(())
}

fn is_hint_path(path: &str) -> bool {
    matches!(
        path,
        "display_name" | "displayName" | "oauth.setup_token_hint" | "oauth.setupTokenHint"
    )
}

fn is_url_field(key: &str) -> bool {
    matches!(
        key,
        "base_url"
            | "baseUrl"
            | "token_url"
            | "tokenUrl"
            | "token_url_fallback"
            | "tokenUrlFallback"
            | "authorize_url"
            | "authorizeUrl"
            | "device_auth_url"
            | "deviceAuthUrl"
            | "redirect_uri"
            | "redirectUri"
            | "chat_path"
            | "chatPath"
    )
}

fn scheme_of(s: &str) -> Option<&str> {
    let colon = s.find(':')?;
    let scheme = &s[..colon];
    if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some(scheme)
}

fn check_url(field: &str, raw: &str) -> Result<(), ProfileError> {
    let s = raw.trim();
    let Some(scheme) = scheme_of(s) else {
        return Ok(());
    };
    let scheme_l = scheme.to_ascii_lowercase();
    let allowed = match scheme_l.as_str() {
        "https" => true,
        "http" => is_loopback_http(s),
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(ProfileError::DisallowedUrl {
            field: field.to_string(),
            url: raw.to_string(),
        })
    }
}

fn is_loopback_http(url: &str) -> bool {
    let Some((_, rest)) = url.split_once(':') else {
        return false;
    };
    let Some(after_slashes) = rest.strip_prefix("//") else {
        return false;
    };
    let without_userinfo = match after_slashes.rfind('@') {
        Some(i) => &after_slashes[i + 1..],
        None => after_slashes,
    };
    let hostport = without_userinfo
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_userinfo);
    let host = if let Some(inner) = hostport.strip_prefix('[') {
        match inner.split_once(']') {
            Some((h, _)) => h,
            None => return false,
        }
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("localhost.")
        || host == "127.0.0.1"
        || host == "::1"
}

fn refuse_interpolation(s: &str, path: &str) -> Result<(), ProfileError> {
    let trimmed = s.trim_start();
    let command = trimmed.starts_with('!') || has_dollar_paren(s);
    let backtick = s.contains('`');
    // Hint fields may contain backticks. `!command` and `$(...)` are still refuse.
    if command || (backtick && !is_hint_path(path)) {
        return Err(ProfileError::Interpolation {
            field: path.to_string(),
        });
    }
    Ok(())
}

fn has_dollar_paren(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' && bytes[i + 1] == b'(' {
            return bytes[i + 2..].contains(&b')');
        }
        i += 1;
    }
    false
}

fn is_native_module_path(s: &str) -> bool {
    let path = s.trim().split(['?', '#']).next().unwrap_or(s).trim();
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".wasm")
        || lower.ends_with(".dylib")
        || lower.ends_with(".so")
        || lower.ends_with(".dll")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        assert!(is_loopback_http("http://localhost:11434"));
        assert!(is_loopback_http("http://127.0.0.1"));
        assert!(is_loopback_http("http://[::1]:80/cb"));
        assert!(!is_loopback_http("http://192.0.2.1"));
        assert!(!is_loopback_http("http://127.0.0.1.example"));
    }

    #[test]
    fn hint_path_is_document_fields_only() {
        assert!(is_hint_path("display_name"));
        assert!(is_hint_path("oauth.setup_token_hint"));
        assert!(!is_hint_path("headers.setup_token_hint"));
        assert!(!is_hint_path("setup_token_hint"));
    }
}

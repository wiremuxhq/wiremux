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
            | "messages_path"
            | "messagesPath"
            | "responses_path"
            | "responsesPath"
            | "gemini_path"
            | "geminiPath"
            | "converse_path"
            | "conversePath"
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
    // This parser strips ASCII tab and newline before the scheme.
    // `scheme_of` does not, so those bytes must not skip the loopback gate.
    if let Ok(parsed) = url::Url::parse(s) {
        let http = parsed.scheme().eq_ignore_ascii_case("http");
        let allowed =
            parsed.scheme().eq_ignore_ascii_case("https") || (http && loopback_host(&parsed));
        if allowed {
            return Ok(());
        }
        return Err(ProfileError::DisallowedUrl {
            field: field.to_string(),
            url: raw.to_string(),
        });
    }
    // `{env:VAR}` in a host does not parse. https stays allowed.
    // A cleartext http URL that does not parse is still refused.
    let Some(scheme) = scheme_of(s) else {
        return Ok(());
    };
    let allowed = match scheme.to_ascii_lowercase().as_str() {
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

pub(crate) fn is_loopback_http(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return false;
    };
    parsed.scheme().eq_ignore_ascii_case("http") && loopback_host(&parsed)
}

fn loopback_host(parsed: &url::Url) -> bool {
    let Some(host) = parsed.host_str() else {
        return false;
    };
    // `host_str` puts IPv6 in brackets (`[::1]`).
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("localhost.")
        || host == "127.0.0.1"
        || host == "::1"
}

fn refuse_interpolation(s: &str, path: &str) -> Result<(), ProfileError> {
    let trimmed = s.trim_start();
    let bang = trimmed.starts_with('!');
    let dollar = has_dollar_paren(s);
    let backtick = s.contains('`');
    // Hint fields may contain backticks. `!command` and `$(...)` are still refuse.
    let trigger = if bang {
        Some("!")
    } else if dollar {
        Some("$(...)")
    } else if backtick && !is_hint_path(path) {
        Some("backticks")
    } else {
        None
    };
    if let Some(trigger) = trigger {
        return Err(ProfileError::Interpolation {
            field: path.to_string(),
            trigger,
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
        assert!(is_loopback_http("http://localhost."));
        assert!(is_loopback_http("http://127.0.0.1"));
        assert!(is_loopback_http("http://[::1]:80/cb"));
        assert!(!is_loopback_http("http://192.0.2.1"));
        assert!(!is_loopback_http("http://127.0.0.1.example"));
        assert!(
            !is_loopback_http("http://evil.com#@127.0.0.1"),
            "fragment @ is not userinfo"
        );
        assert!(
            !is_loopback_http("http://evil.com?@127.0.0.1"),
            "query @ is not userinfo"
        );
        assert!(is_loopback_http("http://127.0.0.1#@evil.com"));
        assert!(is_loopback_http("http://user:pass@127.0.0.1/cb"));
        assert!(
            is_loopback_http(r"http://127.0.0.1\@evil.com"),
            "backslash is a path separator, host stays loopback"
        );
        assert!(
            !is_loopback_http(r"http://evil.com\@127.0.0.1"),
            "backslash is a path separator, host is evil.com"
        );
        let err = check_url("base_url", "http://evil.com#@127.0.0.1").unwrap_err();
        assert!(matches!(err, ProfileError::DisallowedUrl { .. }), "{err}");
        let slash = check_url("base_url", r"http://evil.com\@127.0.0.1").unwrap_err();
        assert!(
            matches!(slash, ProfileError::DisallowedUrl { .. }),
            "{slash}"
        );
        for raw in ["ht\ttp://evil.example/v1", "http\t://evil.example/v1"] {
            let err = check_url("base_url", raw).unwrap_err();
            assert!(
                matches!(err, ProfileError::DisallowedUrl { .. }),
                "{raw} parsed as cleartext and must be refused, got {err}"
            );
        }
        assert!(check_url("base_url", "/v1/messages").is_ok());
        assert!(
            check_url(
                "base_url",
                "https://bedrock-runtime.{env:AWS_REGION}.amazonaws.com"
            )
            .is_ok(),
            "an https host template is expanded later"
        );
    }

    #[test]
    fn hint_path_is_document_fields_only() {
        assert!(is_hint_path("display_name"));
        assert!(is_hint_path("oauth.setup_token_hint"));
        assert!(!is_hint_path("headers.setup_token_hint"));
        assert!(!is_hint_path("setup_token_hint"));
    }
}

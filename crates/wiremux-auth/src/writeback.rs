//! Fail-closed credential write-back: 0600, JSON-pointer RMW, refuse empty.

use std::path::Path;

use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::error::AuthError;
use crate::helpers::MAX_CREDS_BYTES;
use crate::profile::ExpiresUnit;

/// Pointers and token values applied during fail-closed write-back.
pub(crate) struct TokenWrite<'a> {
    pub access_ptr: &'a str,
    pub refresh_ptr: Option<&'a str>,
    pub expires_ptr: Option<&'a str>,
    pub expires_unit: ExpiresUnit,
    pub access_token: &'a str,
    pub refresh_token: Option<&'a str>,
    pub expires_in_secs: u64,
}

/// Read-modify-write tokens into an existing JSON object via pointers.
///
/// Refuses if the file is missing, not an object, parse fails, the new
/// access token is empty, or chmod 0600 fails. Never replaces a rich
/// document with a stub that only has the access token.
pub(crate) async fn write_tokens(path: &Path, write: &TokenWrite<'_>) -> Result<(), AuthError> {
    if write.access_token.trim().is_empty() {
        return Err(AuthError::EmptyWriteRefused);
    }
    let content = read_creds_string(path).await?;
    let mut doc: Value = serde_json::from_str(&content)?;
    if !doc.is_object() {
        return Err(AuthError::EmptyWriteRefused);
    }
    apply_tokens(&mut doc, write)?;
    let updated = serde_json::to_string_pretty(&doc)?;
    write_secret_file(path, updated.as_bytes()).await
}

pub(crate) fn apply_tokens(doc: &mut Value, write: &TokenWrite<'_>) -> Result<(), AuthError> {
    if write.access_token.trim().is_empty() {
        return Err(AuthError::EmptyWriteRefused);
    }
    if !doc.is_object() {
        return Err(AuthError::EmptyWriteRefused);
    }
    pointer_set(
        doc,
        write.access_ptr,
        Value::String(write.access_token.to_owned()),
    )?;
    if let (Some(ptr), Some(rt)) = (write.refresh_ptr, write.refresh_token) {
        pointer_set(doc, ptr, Value::String(rt.to_owned()))?;
    }
    if let Some(ptr) = write.expires_ptr {
        pointer_set(
            doc,
            ptr,
            expires_value(doc, ptr, write.expires_unit, write.expires_in_secs),
        )?;
    }
    Ok(())
}

fn expires_value(doc: &Value, ptr: &str, unit: ExpiresUnit, expires_in_secs: u64) -> Value {
    let existing_is_string = pointer_get(doc, ptr).is_some_and(Value::is_string);
    if existing_is_string {
        return Value::String(crate::helpers::format_rfc3339_now_plus(expires_in_secs));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    match unit {
        ExpiresUnit::Ms => {
            let ms = (now.as_millis().min(u128::from(u64::MAX)) as u64)
                .saturating_add(expires_in_secs.saturating_mul(1000));
            json_u64_value(ms)
        }
        ExpiresUnit::S => {
            let secs = now.as_secs().saturating_add(expires_in_secs);
            json_u64_value(secs)
        }
    }
}

fn json_u64_value(n: u64) -> Value {
    serde_json::Number::from(n).into()
}

pub(crate) async fn read_creds_string(path: &Path) -> Result<String, AuthError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;
    if meta.len() > MAX_CREDS_BYTES {
        return Err(AuthError::TokenProvider(format!(
            "credentials file too large ({} bytes, max {MAX_CREDS_BYTES})",
            meta.len()
        )));
    }
    tokio::fs::read_to_string(path)
        .await
        .map_err(|e| AuthError::io(Some(path.to_path_buf()), e))
}

/// Create or replace `path` with mode 0600. Chmod happens before secret bytes.
pub(crate) async fn write_secret_file(path: &Path, contents: &[u8]) -> Result<(), AuthError> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .await
        .map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        file.set_permissions(perms)
            .await
            .map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;
    }

    file.write_all(contents)
        .await
        .map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;
    file.flush()
        .await
        .map_err(|e| AuthError::io(Some(path.to_path_buf()), e))?;
    Ok(())
}

/// RFC 6901 get. Missing path yields `None`.
pub(crate) fn pointer_get<'a>(doc: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        return Some(doc);
    }
    if !pointer.starts_with('/') {
        return None;
    }
    let mut cur = doc;
    for token in pointer[1..].split('/') {
        let key = unescape_pointer_token(token);
        cur = match cur {
            Value::Object(map) => map.get(&key)?,
            Value::Array(arr) => {
                let idx: usize = key.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// RFC 6901 set. Creates missing object parents. Refuses a non-object parent.
pub(crate) fn pointer_set(doc: &mut Value, pointer: &str, value: Value) -> Result<(), AuthError> {
    if pointer.is_empty() {
        *doc = value;
        return Ok(());
    }
    if !pointer.starts_with('/') {
        return Err(AuthError::TokenProvider(format!(
            "invalid JSON pointer `{pointer}`"
        )));
    }
    let tokens: Vec<String> = pointer[1..]
        .split('/')
        .map(unescape_pointer_token)
        .collect();
    if tokens.is_empty() {
        *doc = value;
        return Ok(());
    }
    let mut cur = doc;
    for token in &tokens[..tokens.len() - 1] {
        match cur {
            Value::Object(map) => {
                if !matches!(map.get(token), Some(Value::Object(_) | Value::Array(_))) {
                    map.insert(token.clone(), Value::Object(serde_json::Map::new()));
                }
                cur = map.get_mut(token).expect("just inserted or existed");
            }
            Value::Array(arr) => {
                let idx: usize = token.parse().map_err(|_| {
                    AuthError::TokenProvider(format!("JSON pointer array index `{token}`"))
                })?;
                cur = arr.get_mut(idx).ok_or_else(|| {
                    AuthError::TokenProvider("JSON pointer array index out of range".into())
                })?;
            }
            _ => return Err(AuthError::EmptyWriteRefused),
        }
    }
    let last = tokens.last().expect("non-empty tokens");
    match cur {
        Value::Object(map) => {
            map.insert(last.clone(), value);
            Ok(())
        }
        Value::Array(arr) => {
            let idx: usize = last.parse().map_err(|_| {
                AuthError::TokenProvider(format!("JSON pointer array index `{last}`"))
            })?;
            let slot = arr.get_mut(idx).ok_or_else(|| {
                AuthError::TokenProvider("JSON pointer array index out of range".into())
            })?;
            *slot = value;
            Ok(())
        }
        _ => Err(AuthError::EmptyWriteRefused),
    }
}

fn unescape_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

pub(crate) fn json_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(crate) fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .map(|n| n as u64)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_round_trip_nested() {
        let mut doc = serde_json::json!({"tokens": {"access": "old"}});
        pointer_set(&mut doc, "/tokens/access", Value::String("new".into())).unwrap();
        assert_eq!(
            pointer_get(&doc, "/tokens/access").and_then(Value::as_str),
            Some("new")
        );
    }

    #[test]
    fn pointer_escapes_slash() {
        let mut doc = serde_json::json!({});
        pointer_set(&mut doc, "/a~1b", Value::String("x".into())).unwrap();
        assert_eq!(doc["a/b"].as_str(), Some("x"));
        assert_eq!(
            pointer_get(&doc, "/a~1b").and_then(Value::as_str),
            Some("x")
        );
    }

    #[test]
    fn apply_tokens_refuses_empty_access() {
        let mut doc = serde_json::json!({"access": "old"});
        let err = apply_tokens(
            &mut doc,
            &TokenWrite {
                access_ptr: "/access",
                refresh_ptr: None,
                expires_ptr: None,
                expires_unit: ExpiresUnit::S,
                access_token: "  ",
                refresh_token: None,
                expires_in_secs: 60,
            },
        )
        .unwrap_err();
        assert!(matches!(err, AuthError::EmptyWriteRefused));
        assert_eq!(doc["access"].as_str(), Some("old"));
    }

    #[test]
    fn apply_tokens_refuses_non_object() {
        let mut doc = serde_json::json!(["not", "object"]);
        let err = apply_tokens(
            &mut doc,
            &TokenWrite {
                access_ptr: "/0",
                refresh_ptr: None,
                expires_ptr: None,
                expires_unit: ExpiresUnit::S,
                access_token: "tok",
                refresh_token: None,
                expires_in_secs: 60,
            },
        )
        .unwrap_err();
        assert!(matches!(err, AuthError::EmptyWriteRefused));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_secret_file_sets_0600_on_existing_0644() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_secret_file(&path, b"new-secret")
            .await
            .expect("write_secret_file");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600 after write, got {mode:o}");
        assert_eq!(std::fs::read(&path).unwrap(), b"new-secret");
    }
}

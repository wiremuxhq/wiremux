//! Fail-closed credential write-back: 0600, JSON-pointer RMW, refuse empty.

use std::path::Path;

use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::error::AuthError;
use crate::exchange::TokenExchangeResponse;
use crate::helpers::{MAX_CREDS_BYTES, resolve_creds_path};
use crate::profile::{CredsFormat, ExpiresUnit, OauthPack};

/// Pointers and token values applied during fail-closed write-back.
pub(crate) struct TokenWrite<'a> {
    pub access_ptr: &'a str,
    pub refresh_ptr: Option<&'a str>,
    pub expires_ptr: Option<&'a str>,
    pub expires_unit: ExpiresUnit,
    pub access_token: &'a str,
    pub refresh_token: Option<&'a str>,
    pub expires_in_secs: u64,
    /// When true, write `expires_at` as RFC 3339 even if the slot is new.
    pub expires_rfc3339: bool,
    pub token_url_ptr: Option<&'a str>,
    pub token_url: Option<&'a str>,
}

/// `{issuer}::{client_id}` store key. Trailing slash on the issuer is stripped.
pub(crate) fn oidc_profile_key(issuer: &str, client_id: &str) -> String {
    format!("{}::{}", issuer.trim_end_matches('/'), client_id)
}

/// Scheme + host from an https or loopback http URL.
pub(crate) fn issuer_origin(url: &str) -> Option<String> {
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        ("http", rest)
    } else {
        return None;
    };
    let host = rest.split('/').next()?.trim();
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Exact `{issuer}::{client_id}`, else `{issuer}::{client_id}@{name}`.
pub(crate) fn select_oidc_entry_key(doc: Option<&Value>, want: &str) -> String {
    let Some(obj) = doc.and_then(Value::as_object) else {
        return want.to_owned();
    };
    if obj.contains_key(want) {
        return want.to_owned();
    }
    obj.keys()
        .find(|key| {
            key.strip_prefix(want)
                .and_then(|rest| rest.strip_prefix('@'))
                .is_some_and(|suffix| {
                    !suffix.is_empty() && !suffix.contains("::") && !suffix.contains('/')
                })
        })
        .cloned()
        .unwrap_or_else(|| want.to_owned())
}

fn oidc_issuer_and_client(oauth: &OauthPack) -> Result<(String, String), AuthError> {
    let client = oauth
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AuthError::MissingField("oauth.client_id".into()))?
        .to_owned();
    let issuer = oauth
        .authorize_url
        .as_deref()
        .and_then(issuer_origin)
        .or_else(|| issuer_origin(&oauth.token_url))
        .ok_or_else(|| {
            AuthError::MissingField("oauth issuer (authorize_url or token_url origin)".into())
        })?;
    Ok((issuer, client))
}

/// Access / refresh / expires pointers for `creds_format = "oidc-auth-json"`.
pub(crate) fn oidc_store_pointers(
    oauth: &OauthPack,
    doc: Option<&Value>,
) -> Result<Option<(String, String, String)>, AuthError> {
    if oauth.creds_format != Some(CredsFormat::OidcAuthJson) {
        return Ok(None);
    }
    if oauth.access_token_ptr.is_some() {
        return Ok(None);
    }
    let (issuer, client) = oidc_issuer_and_client(oauth)?;
    let want = oidc_profile_key(&issuer, &client);
    let entry = select_oidc_entry_key(doc, &want);
    let escaped = escape_pointer_token(&entry);
    Ok(Some((
        format!("/{escaped}/key"),
        format!("/{escaped}/refresh_token"),
        format!("/{escaped}/expires_at"),
    )))
}

/// Access pointer for `creds_format = "copilot-hosts"`.
pub(crate) fn copilot_store_pointers(
    oauth: &OauthPack,
    doc: Option<&Value>,
) -> Result<Option<(String, Option<String>)>, AuthError> {
    if oauth.creds_format != Some(CredsFormat::CopilotHosts) {
        return Ok(None);
    }
    if oauth.access_token_ptr.is_some() {
        return Ok(None);
    }
    let host = select_copilot_host(doc);
    let escaped = escape_pointer_token(&host);
    let refresh = oauth.refresh_token_ptr.clone().or_else(|| {
        let ptr = format!("/{escaped}/refresh_token");
        doc.and_then(|d| pointer_get(d, &ptr))
            .and_then(json_string)
            .map(|_| ptr)
    });
    Ok(Some((format!("/{escaped}/oauth_token"), refresh)))
}

struct PersistPtrs {
    access: String,
    refresh: Option<String>,
    expires: Option<String>,
    expires_rfc3339: bool,
}

fn persist_store_pointers(
    oauth: &OauthPack,
    doc: Option<&Value>,
) -> Result<PersistPtrs, AuthError> {
    if let Some((access, refresh, expires)) = oidc_store_pointers(oauth, doc)? {
        return Ok(PersistPtrs {
            access,
            refresh: Some(refresh),
            expires: Some(expires),
            expires_rfc3339: true,
        });
    }
    if let Some((access, refresh)) = copilot_store_pointers(oauth, doc)? {
        return Ok(PersistPtrs {
            access,
            refresh,
            expires: None,
            expires_rfc3339: false,
        });
    }
    Ok(PersistPtrs {
        access: oauth
            .access_token_ptr
            .clone()
            .unwrap_or_else(|| "/access_token".into()),
        refresh: oauth
            .refresh_token_ptr
            .clone()
            .or_else(|| Some("/refresh_token".into())),
        expires: oauth
            .expires_ptr
            .clone()
            .or_else(|| Some("/expires_in".into())),
        expires_rfc3339: false,
    })
}

fn select_copilot_host(doc: Option<&Value>) -> String {
    let Some(obj) = doc.and_then(Value::as_object) else {
        return "github.com".into();
    };
    obj.iter()
        .find_map(|(key, entry)| {
            entry
                .get("oauth_token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|_| key.clone())
        })
        .unwrap_or_else(|| "github.com".into())
}

/// Persist tokens from a login exchange.
///
/// Creates the credential file when missing. Refuses to invent a Claude
/// credentials document from scratch (env-only stays env-only).
pub async fn persist_login_tokens(
    oauth: &OauthPack,
    tokens: &TokenExchangeResponse,
) -> Result<(), AuthError> {
    if tokens.access_token.trim().is_empty() {
        return Err(AuthError::EmptyWriteRefused);
    }
    if matches!(oauth.creds_format, Some(CredsFormat::ClaudeCredentials)) {
        return Err(AuthError::TokenProvider(
            "refusing to invent a Claude credentials file; run the setup-token flow".into(),
        ));
    }
    let raw = oauth
        .creds_path
        .as_deref()
        .ok_or_else(|| AuthError::MissingField("oauth.creds_path".into()))?;
    let path = resolve_creds_path(raw)?;
    let existing = if path.is_file() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    } else {
        None
    };
    let ptrs = persist_store_pointers(oauth, existing.as_ref())?;
    let token_url_ptr = oidc_token_url_ptr(oauth, &ptrs.access);
    let write = TokenWrite {
        access_ptr: &ptrs.access,
        refresh_ptr: ptrs.refresh.as_deref(),
        expires_ptr: ptrs.expires.as_deref(),
        expires_unit: oauth.expires_unit.unwrap_or(ExpiresUnit::S),
        access_token: &tokens.access_token,
        refresh_token: tokens.refresh_token.as_deref(),
        expires_in_secs: tokens.expires_in.unwrap_or(3600),
        expires_rfc3339: ptrs.expires_rfc3339,
        token_url_ptr: token_url_ptr.as_deref(),
        token_url: oidc_token_url_value(oauth),
    };
    if path.is_file() {
        return write_tokens(&path, &write).await;
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AuthError::io(Some(parent.to_path_buf()), e))?;
    }
    let mut doc = serde_json::json!({});
    apply_tokens(&mut doc, &write)?;
    let updated = serde_json::to_string_pretty(&doc)?;
    write_secret_file(&path, updated.as_bytes()).await
}

/// Remove one `oidc-auth-json` profile entry. Deletes the file when empty.
///
/// Fail-closed on missing path, corrupt JSON, or a non-object root.
/// Sibling entries stay. Missing entry is an error (do not invent).
pub async fn remove_store_entry(oauth: &OauthPack) -> Result<(), AuthError> {
    if oauth.creds_format != Some(CredsFormat::OidcAuthJson) {
        return Err(AuthError::TokenProvider(
            "remove_store_entry requires creds_format = oidc-auth-json".into(),
        ));
    }
    let raw = oauth
        .creds_path
        .as_deref()
        .ok_or_else(|| AuthError::MissingField("oauth.creds_path".into()))?;
    let path = resolve_creds_path(raw)?;
    if !path.is_file() {
        return Err(AuthError::TokenProvider(
            "credentials file is missing; nothing to remove".into(),
        ));
    }
    let content = read_creds_string(&path).await?;
    let mut doc: Value = serde_json::from_str(&content).map_err(|e| {
        AuthError::TokenProvider(format!(
            "parse credentials {}: {e}; refusing to remove",
            path.display()
        ))
    })?;
    if !doc.is_object() {
        return Err(AuthError::TokenProvider(
            "auth file is not a JSON object; refusing to remove".into(),
        ));
    }
    let (issuer, client) = oidc_issuer_and_client(oauth)?;
    let want = oidc_profile_key(&issuer, &client);
    let key = select_oidc_entry_key(Some(&doc), &want);
    let obj = doc.as_object_mut().expect("object");
    if obj.remove(&key).is_none() {
        return Err(AuthError::TokenProvider(format!(
            "auth profile {want} not found; refusing to write"
        )));
    }
    if obj.is_empty() {
        std::fs::remove_file(&path).map_err(|e| AuthError::io(Some(path.clone()), e))?;
        return Ok(());
    }
    let updated = serde_json::to_string_pretty(&doc)?;
    write_secret_file(&path, updated.as_bytes()).await
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

pub(crate) fn oidc_token_url_ptr(oauth: &OauthPack, access_ptr: &str) -> Option<String> {
    if oauth.creds_format != Some(CredsFormat::OidcAuthJson) {
        return None;
    }
    access_ptr
        .strip_suffix("/key")
        .map(|entry| format!("{entry}/token_url"))
}

pub(crate) fn oidc_token_url_value(oauth: &OauthPack) -> Option<&str> {
    (oauth.creds_format == Some(CredsFormat::OidcAuthJson)).then_some(oauth.token_url.as_str())
}

pub(crate) fn apply_tokens(doc: &mut Value, write: &TokenWrite<'_>) -> Result<(), AuthError> {
    if write.access_token.trim().is_empty() {
        return Err(AuthError::EmptyWriteRefused);
    }
    if !doc.is_object() {
        return Err(AuthError::EmptyWriteRefused);
    }
    // Validate every pointer before mutating so a root pointer cannot
    // replace a rich object with a bare string or number.
    refuse_root_pointer(write.access_ptr)?;
    if let Some(ptr) = write.refresh_ptr {
        refuse_root_pointer(ptr)?;
    }
    if let Some(ptr) = write.expires_ptr {
        refuse_root_pointer(ptr)?;
    }
    if let Some(ptr) = write.token_url_ptr {
        refuse_root_pointer(ptr)?;
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
            expires_value(
                doc,
                ptr,
                write.expires_unit,
                write.expires_in_secs,
                write.expires_rfc3339,
            ),
        )?;
    }
    if let (Some(ptr), Some(url)) = (write.token_url_ptr, write.token_url)
        && !url.is_empty()
    {
        pointer_set(doc, ptr, Value::String(url.to_owned()))?;
    }
    Ok(())
}

fn refuse_root_pointer(pointer: &str) -> Result<(), AuthError> {
    if is_root_pointer(pointer) {
        return Err(AuthError::EmptyWriteRefused);
    }
    Ok(())
}

fn is_root_pointer(pointer: &str) -> bool {
    pointer.is_empty()
}

fn expires_value(
    doc: &Value,
    ptr: &str,
    unit: ExpiresUnit,
    expires_in_secs: u64,
    expires_rfc3339: bool,
) -> Value {
    let existing_is_string = pointer_get(doc, ptr).is_some_and(Value::is_string);
    if existing_is_string || expires_rfc3339 {
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

/// Replace `path` with `contents` at mode 0600 without truncating the live file.
///
/// Bytes go to a sibling temp file (created 0600). The live path is replaced
/// only after that write and chmod succeed.
pub(crate) async fn write_secret_file(path: &Path, contents: &[u8]) -> Result<(), AuthError> {
    let tmp = secret_tmp_path(path);
    if let Err(e) = write_secret_tmp(&tmp, contents).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    if let Err(e) = replace_secret_file(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    Ok(())
}

fn secret_tmp_path(path: &Path) -> std::path::PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "creds".into());
    name.push(".tmp");
    path.with_file_name(name)
}

async fn write_secret_tmp(tmp: &Path, contents: &[u8]) -> Result<(), AuthError> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        opts.mode(0o600);
    }
    let mut file = opts
        .open(tmp)
        .await
        .map_err(|e| AuthError::io(Some(tmp.to_path_buf()), e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        file.set_permissions(perms)
            .await
            .map_err(|e| AuthError::io(Some(tmp.to_path_buf()), e))?;
    }

    file.write_all(contents)
        .await
        .map_err(|e| AuthError::io(Some(tmp.to_path_buf()), e))?;
    file.flush()
        .await
        .map_err(|e| AuthError::io(Some(tmp.to_path_buf()), e))?;
    file.sync_all()
        .await
        .map_err(|e| AuthError::io(Some(tmp.to_path_buf()), e))?;
    Ok(())
}

async fn replace_secret_file(tmp: &Path, dest: &Path) -> Result<(), AuthError> {
    #[cfg(unix)]
    {
        tokio::fs::rename(tmp, dest)
            .await
            .map_err(|e| AuthError::io(Some(dest.to_path_buf()), e))
    }
    #[cfg(not(unix))]
    {
        persist_over_existing(tmp, dest).await
    }
}

/// Dest is only touched after the sibling write finished.
#[cfg(not(unix))]
async fn persist_over_existing(tmp: &Path, dest: &Path) -> Result<(), AuthError> {
    if tokio::fs::rename(tmp, dest).await.is_ok() {
        return Ok(());
    }
    let contents = tokio::fs::read(tmp)
        .await
        .map_err(|e| AuthError::io(Some(tmp.to_path_buf()), e))?;
    tokio::fs::write(dest, contents)
        .await
        .map_err(|e| AuthError::io(Some(dest.to_path_buf()), e))?;
    let _ = tokio::fs::remove_file(tmp).await;
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
///
/// The empty / root pointer is refused so write-back cannot replace a rich
/// object with a bare token string.
pub(crate) fn pointer_set(doc: &mut Value, pointer: &str, value: Value) -> Result<(), AuthError> {
    if is_root_pointer(pointer) {
        return Err(AuthError::EmptyWriteRefused);
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
        return Err(AuthError::EmptyWriteRefused);
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
    fn oidc_pointer_escapes_issuer_slashes() {
        let key = oidc_profile_key("https://auth.openai.com/", "wiremux-cli");
        assert_eq!(key, "https://auth.openai.com::wiremux-cli");
        let ptr = format!("/{}/key", escape_pointer_token(&key));
        let mut doc = serde_json::json!({});
        pointer_set(&mut doc, &ptr, Value::String("tok".into())).unwrap();
        assert_eq!(
            doc["https://auth.openai.com::wiremux-cli"]["key"].as_str(),
            Some("tok")
        );
        assert_eq!(
            issuer_origin("https://auth.openai.com/oauth/token").as_deref(),
            Some("https://auth.openai.com")
        );
        assert_eq!(
            select_oidc_entry_key(
                Some(&serde_json::json!({
                    "https://auth.openai.com::wiremux-cli@work": {"key": "x"}
                })),
                "https://auth.openai.com::wiremux-cli"
            ),
            "https://auth.openai.com::wiremux-cli@work"
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
                expires_rfc3339: false,
                token_url_ptr: None,
                token_url: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, AuthError::EmptyWriteRefused));
        assert_eq!(doc["access"].as_str(), Some("old"));
    }

    #[test]
    fn apply_tokens_refuses_empty_pointer_without_mutating() {
        let original = serde_json::json!({
            "claudeAiOauth": {"accessToken": "old", "keep": true},
            "otherField": "keep-me"
        });
        let mut doc = original.clone();
        let err = apply_tokens(
            &mut doc,
            &TokenWrite {
                access_ptr: "",
                refresh_ptr: Some("/claudeAiOauth/refreshToken"),
                expires_ptr: None,
                expires_unit: ExpiresUnit::Ms,
                access_token: "new-token",
                refresh_token: Some("rt-new"),
                expires_in_secs: 60,
                expires_rfc3339: false,
                token_url_ptr: None,
                token_url: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, AuthError::EmptyWriteRefused));
        assert_eq!(doc, original);
    }

    #[test]
    fn pointer_set_refuses_root_pointer() {
        let mut doc = serde_json::json!({"keep": true});
        let err = pointer_set(&mut doc, "", Value::String("bare".into())).unwrap_err();
        assert!(matches!(err, AuthError::EmptyWriteRefused));
        assert_eq!(doc, serde_json::json!({"keep": true}));
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
                expires_rfc3339: false,
                token_url_ptr: None,
                token_url: None,
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
        assert!(
            !secret_tmp_path(&path).exists(),
            "sibling tmp must be gone after a successful replace"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_secret_file_leaves_dest_untouched_when_tmp_cannot_be_created() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        std::fs::write(&path, b"keep-me").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = write_secret_file(&path, b"new-secret").await;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err.is_err(), "create tmp in a 0555 dir must fail");
        assert_eq!(std::fs::read(&path).unwrap(), b"keep-me");
    }

    #[tokio::test]
    async fn persist_login_tokens_refuses_claude_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let oauth = OauthPack {
            token_url: "https://auth.example.invalid/token".into(),
            token_url_fallback: None,
            authorize_url: None,
            authorize_params: Default::default(),
            device_auth_url: None,
            client_id: None,
            redirect_uri: None,
            scopes: Vec::new(),
            list_merge: crate::profile::ListMerge::Union,
            pkce: None,
            refresh_grant: None,
            refresh_body: Default::default(),
            token_request_format: None,
            token_headers: Default::default(),
            creds_path: Some(path.to_string_lossy().into_owned()),
            creds_format: Some(CredsFormat::ClaudeCredentials),
            access_token_ptr: None,
            refresh_token_ptr: None,
            expires_ptr: None,
            expires_unit: None,
            access_env: None,
            login: None,
            setup_token_hint: None,
            keychain_service: None,
            keychain_accounts: Vec::new(),
            token_response: None,
        };
        let tokens = TokenExchangeResponse {
            access_token: "sk-new".into(),
            refresh_token: Some("rt-new".into()),
            expires_in: Some(60),
            token_type: None,
            scope: None,
        };
        let err = persist_login_tokens(&oauth, &tokens).await.unwrap_err();
        assert!(
            matches!(err, AuthError::TokenProvider(ref msg) if msg.contains("Claude")),
            "{err:?}"
        );
        assert!(!path.exists(), "must not invent a Claude credentials file");
    }

    #[tokio::test]
    async fn persist_login_tokens_writes_oidc_named_entry() {
        let home = crate::isolated_home::IsolatedHome::new();
        let path = home.path().join("auth-openai.json");
        let oauth = crate::parse_profile_str(&format!(
            r#"
schema_version = 1
id = "openai-codex-oauth"
[oauth]
token_url = "https://auth.openai.com/oauth/token"
authorize_url = "https://auth.openai.com/oauth/authorize"
client_id = "wiremux-cli"
creds_format = "oidc-auth-json"
creds_path = "{}"
login = "none"
"#,
            path.display().to_string().replace('\\', "/")
        ))
        .unwrap()
        .oauth
        .unwrap();
        let tokens = TokenExchangeResponse {
            access_token: "new-access".into(),
            refresh_token: Some("new-rt".into()),
            expires_in: Some(60),
            token_type: None,
            scope: None,
        };
        persist_login_tokens(&oauth, &tokens)
            .await
            .expect("persist oidc");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entry = &doc["https://auth.openai.com::wiremux-cli"];
        assert_eq!(entry["key"].as_str(), Some("new-access"));
        assert_eq!(entry["refresh_token"].as_str(), Some("new-rt"));
        let expires = entry["expires_at"].as_str().expect("rfc3339");
        assert!(expires.contains('T'), "{expires}");
        assert!(
            doc.get("access_token").is_none(),
            "must not use root /access_token"
        );
        assert_eq!(
            entry["token_url"].as_str(),
            Some("https://auth.openai.com/oauth/token")
        );
    }

    fn oidc_pack(path: &std::path::Path) -> crate::OauthPack {
        crate::parse_profile_str(&format!(
            r#"
schema_version = 1
id = "openai-codex-oauth"
[oauth]
token_url = "https://auth.openai.com/oauth/token"
authorize_url = "https://auth.openai.com/oauth/authorize"
client_id = "wiremux-cli"
creds_format = "oidc-auth-json"
creds_path = "{}"
login = "none"
"#,
            path.display().to_string().replace('\\', "/")
        ))
        .unwrap()
        .oauth
        .unwrap()
    }

    #[tokio::test]
    async fn remove_auth_file_entry_leaves_sibling() {
        let home = crate::isolated_home::IsolatedHome::new();
        let path = home.path().join("auth-openai.json");
        std::fs::write(
            &path,
            serde_json::to_string(&serde_json::json!({
                "https://aaa.example::other": {"key": "keep-me", "refresh_token": "rt"},
                "https://auth.openai.com::wiremux-cli": {"key": "drop-me", "refresh_token": "rt"}
            }))
            .unwrap(),
        )
        .unwrap();
        remove_store_entry(&oidc_pack(&path)).await.expect("remove");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc["https://aaa.example::other"]["key"].as_str(),
            Some("keep-me")
        );
        assert!(doc.get("https://auth.openai.com::wiremux-cli").is_none());
    }

    #[tokio::test]
    async fn remove_auth_file_entry_deletes_file_when_last() {
        let home = crate::isolated_home::IsolatedHome::new();
        let path = home.path().join("auth-openai.json");
        std::fs::write(
            &path,
            serde_json::to_string(&serde_json::json!({
                "https://auth.openai.com::wiremux-cli": {"key": "only", "refresh_token": "rt"}
            }))
            .unwrap(),
        )
        .unwrap();
        remove_store_entry(&oidc_pack(&path))
            .await
            .expect("remove last");
        assert!(!path.exists(), "empty store must be deleted");
    }

    #[tokio::test]
    async fn remove_auth_file_entry_fails_closed_on_corrupt_json() {
        let home = crate::isolated_home::IsolatedHome::new();
        let path = home.path().join("auth-openai.json");
        std::fs::write(&path, "not-json").unwrap();
        let err = remove_store_entry(&oidc_pack(&path))
            .await
            .expect_err("corrupt");
        assert!(
            err.to_string().contains("parse"),
            "must mention parse, got {err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not-json");
    }

    #[tokio::test]
    async fn persist_login_tokens_writes_copilot_hosts_entry() {
        let home = crate::isolated_home::IsolatedHome::new();
        let path = home.path().join("hosts.json");
        let oauth = crate::parse_profile_str(&format!(
            r#"
schema_version = 1
id = "gist-copilot"
[oauth]
token_url = "https://github.com/login/oauth/access_token"
creds_format = "copilot-hosts"
creds_path = "{}"
login = "none"
"#,
            path.display().to_string().replace('\\', "/")
        ))
        .unwrap()
        .oauth
        .unwrap();
        let tokens = TokenExchangeResponse {
            access_token: "ghu_new".into(),
            refresh_token: None,
            expires_in: Some(3600),
            token_type: None,
            scope: None,
        };
        persist_login_tokens(&oauth, &tokens)
            .await
            .expect("persist copilot");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["github.com"]["oauth_token"].as_str(), Some("ghu_new"));
        assert!(doc.get("access_token").is_none());
    }

    #[tokio::test]
    async fn persist_login_tokens_refuses_parent_dir() {
        let _home = crate::isolated_home::IsolatedHome::new();
        let oauth = crate::parse_profile_str(
            r#"
schema_version = 1
id = "escape-home"
[oauth]
token_url = "https://auth.example.invalid/token"
authorize_url = "https://auth.example.invalid/authorize"
client_id = "wiremux-cli"
creds_format = "oidc-auth-json"
creds_path = "~/ok/../escaped.json"
login = "none"
"#,
        )
        .unwrap()
        .oauth
        .unwrap();
        let tokens = TokenExchangeResponse {
            access_token: "new-access".into(),
            refresh_token: Some("new-rt".into()),
            expires_in: Some(60),
            token_type: None,
            scope: None,
        };
        let err = persist_login_tokens(&oauth, &tokens)
            .await
            .expect_err("parent dir must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("oauth.creds_path"),
            "error must name oauth.creds_path, got {msg}"
        );
        assert!(
            msg.to_ascii_lowercase().contains("home"),
            "error must say path must stay under home, got {msg}"
        );
        let escaped = crate::helpers::expand_tilde("~/ok/../escaped.json");
        assert!(
            !escaped.exists(),
            "must not create_dir_all or write across .."
        );
    }

    #[tokio::test]
    async fn persist_login_tokens_writes_under_isolated_home() {
        let home = crate::isolated_home::IsolatedHome::new();
        let oauth = crate::parse_profile_str(
            r#"
schema_version = 1
id = "under-home"
[oauth]
token_url = "https://auth.example.invalid/token"
authorize_url = "https://auth.example.invalid/authorize"
client_id = "wiremux-cli"
creds_format = "oidc-auth-json"
creds_path = "~/.config/wiremux/auth-openai.json"
login = "none"
"#,
        )
        .unwrap()
        .oauth
        .unwrap();
        let tokens = TokenExchangeResponse {
            access_token: "new-access".into(),
            refresh_token: Some("new-rt".into()),
            expires_in: Some(60),
            token_type: None,
            scope: None,
        };
        persist_login_tokens(&oauth, &tokens)
            .await
            .expect("path under IsolatedHome must write");
        let path = home.path().join(".config/wiremux/auth-openai.json");
        assert!(path.is_file(), "expected write at {}", path.display());
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc["https://auth.example.invalid::wiremux-cli"]["key"].as_str(),
            Some("new-access")
        );
    }
}

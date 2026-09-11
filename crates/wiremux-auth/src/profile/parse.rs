//! TOML/JSON profile parse into the data AST.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::envsubst;
use super::error::ProfileError;
use super::refuse;
use super::types::{
    AuthScheme, Betas, CredsFormat, Dialect, ExpiresUnit, Fingerprint, ForbiddenFieldPolicy, Http,
    ListMerge, Login, OauthPack, ResolvedProfile, SCHEMA_VERSION_MAX, StreamUnknownPolicy,
    TokenRequestFormat, TokenResponse, ToolNameCase, ToolTypePolicy, Wire,
};

/// Parse a profile document from TOML or JSON text.
pub fn parse_profile_str(text: &str) -> Result<ResolvedProfile, ProfileError> {
    parse_profile_text(text, None)
}

pub(crate) fn parse_profile_file(path: &Path) -> Result<ResolvedProfile, ProfileError> {
    let text = fs::read_to_string(path).map_err(|source| ProfileError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_profile_text(&text, Some(path))
}

fn parse_profile_text(text: &str, path: Option<&Path>) -> Result<ResolvedProfile, ProfileError> {
    let value = parse_to_value(text, path)?;
    refuse::scan(&value)?;
    let value = envsubst::walk(value);
    let raw: RawProfile =
        serde_json::from_value(value).map_err(|e| ProfileError::Parse(e.to_string()))?;
    resolve(raw)
}

fn looks_like_json(text: &str, path: Option<&Path>) -> bool {
    if let Some(path) = path {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => return true,
            Some("toml") => return false,
            _ => {}
        }
    }
    text.trim_start().starts_with('{')
}

fn parse_to_value(text: &str, path: Option<&Path>) -> Result<Value, ProfileError> {
    if looks_like_json(text, path) {
        serde_json::from_str(text).map_err(|e| ProfileError::Parse(e.to_string()))
    } else {
        toml::from_str(text).map_err(|e| ProfileError::Parse(e.to_string()))
    }
}

#[derive(Debug, Deserialize)]
struct RawProfile {
    #[serde(default, alias = "schemaVersion")]
    schema_version: Option<u32>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default, alias = "displayName")]
    display_name: Option<String>,
    #[serde(default)]
    wire: Option<Wire>,
    #[serde(default, alias = "streamEvents")]
    stream_events: Option<Vec<String>>,
    #[serde(default, alias = "listMerge")]
    list_merge: Option<ListMerge>,
    #[serde(default, alias = "toolTypePolicy")]
    tool_type_policy: Option<ToolTypePolicy>,
    #[serde(default, alias = "streamUnknownPolicy")]
    stream_unknown_policy: Option<StreamUnknownPolicy>,
    #[serde(default, alias = "baseUrl")]
    base_url: Option<String>,
    #[serde(default, alias = "chatPath")]
    chat_path: Option<String>,
    #[serde(default, alias = "authScheme")]
    auth_scheme: Option<AuthScheme>,
    #[serde(default)]
    headers: Option<BTreeMap<String, String>>,
    #[serde(default, alias = "headerMerge")]
    header_merge: Option<ListMerge>,
    #[serde(default)]
    oauth: Option<RawOauth>,
    #[serde(default)]
    fingerprint: Option<RawFingerprint>,
    #[serde(default)]
    betas: Option<RawBetasField>,
    #[serde(default, alias = "betaHeader")]
    beta_header: Option<String>,
    #[serde(default, alias = "betaMerge")]
    beta_merge: Option<ListMerge>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawBetasField {
    List(Vec<String>),
    Table(RawBetasTable),
}

#[derive(Debug, Deserialize)]
struct RawBetasTable {
    #[serde(default)]
    values: Option<Vec<String>>,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    merge: Option<ListMerge>,
}

#[derive(Debug, Deserialize)]
struct RawOauth {
    #[serde(default, alias = "tokenUrl")]
    token_url: Option<String>,
    #[serde(default, alias = "tokenUrlFallback")]
    token_url_fallback: Option<String>,
    #[serde(default, alias = "authorizeUrl")]
    authorize_url: Option<String>,
    #[serde(default, alias = "authorizeParams")]
    authorize_params: Option<BTreeMap<String, String>>,
    #[serde(default, alias = "deviceAuthUrl")]
    device_auth_url: Option<String>,
    #[serde(default, alias = "clientId")]
    client_id: Option<String>,
    #[serde(default, alias = "redirectUri")]
    redirect_uri: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default, alias = "listMerge")]
    list_merge: Option<ListMerge>,
    #[serde(default)]
    pkce: Option<bool>,
    #[serde(default, alias = "refreshGrant")]
    refresh_grant: Option<String>,
    #[serde(default, alias = "refreshBody")]
    refresh_body: Option<BTreeMap<String, String>>,
    #[serde(default, alias = "tokenRequestFormat")]
    token_request_format: Option<TokenRequestFormat>,
    #[serde(default, alias = "tokenHeaders")]
    token_headers: Option<BTreeMap<String, String>>,
    #[serde(default, alias = "credsPath")]
    creds_path: Option<String>,
    #[serde(default, alias = "credsFormat")]
    creds_format: Option<CredsFormat>,
    #[serde(default, alias = "accessTokenPtr")]
    access_token_ptr: Option<String>,
    #[serde(default, alias = "refreshTokenPtr")]
    refresh_token_ptr: Option<String>,
    #[serde(default, alias = "expiresPtr")]
    expires_ptr: Option<String>,
    #[serde(default, alias = "expiresUnit")]
    expires_unit: Option<ExpiresUnit>,
    #[serde(default, alias = "accessEnv")]
    access_env: Option<String>,
    #[serde(default)]
    login: Option<Login>,
    #[serde(default, alias = "setupTokenHint")]
    setup_token_hint: Option<String>,
    #[serde(default, alias = "keychainService")]
    keychain_service: Option<String>,
    #[serde(default, alias = "keychainAccounts")]
    keychain_accounts: Option<Vec<String>>,
    #[serde(default, alias = "tokenResponse")]
    token_response: Option<RawTokenResponse>,
}

#[derive(Debug, Deserialize)]
struct RawTokenResponse {
    #[serde(default, alias = "accessTokenPtr")]
    access_token_ptr: Option<String>,
    #[serde(default, alias = "refreshTokenPtr")]
    refresh_token_ptr: Option<String>,
    #[serde(default, alias = "expiresPtr")]
    expires_ptr: Option<String>,
    #[serde(default, alias = "expiresUnit")]
    expires_unit: Option<ExpiresUnit>,
}

#[derive(Debug, Deserialize)]
struct RawFingerprint {
    #[serde(default, alias = "userAgent")]
    user_agent: Option<String>,
    #[serde(default, alias = "xApp")]
    x_app: Option<String>,
    #[serde(default, alias = "systemPromptPrefix")]
    system_prompt_prefix: Option<String>,
    #[serde(default, alias = "toolNameCase")]
    tool_name_case: Option<ToolNameCase>,
    #[serde(default, alias = "forbiddenBodyFields")]
    forbidden_body_fields: Option<Vec<String>>,
    #[serde(default, alias = "forbiddenFieldPolicy")]
    forbidden_field_policy: Option<ForbiddenFieldPolicy>,
    #[serde(default, alias = "extraBody")]
    extra_body: Option<BTreeMap<String, Value>>,
}

fn resolve(raw: RawProfile) -> Result<ResolvedProfile, ProfileError> {
    let schema_version = raw
        .schema_version
        .ok_or(ProfileError::MissingField("schema_version"))?;
    if schema_version > SCHEMA_VERSION_MAX {
        return Err(ProfileError::SchemaVersion {
            found: schema_version,
            max: SCHEMA_VERSION_MAX,
        });
    }
    let id = match raw.id {
        Some(id) if !id.is_empty() => id,
        _ => return Err(ProfileError::MissingField("id")),
    };

    let wire = raw.wire;
    let stream_events = match raw.stream_events {
        Some(events) => events,
        None => wire
            .map(|w| {
                w.default_stream_events()
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect()
            })
            .unwrap_or_default(),
    };

    let oauth = match raw.oauth {
        Some(o) => Some(resolve_oauth(o)?),
        None => None,
    };

    let mut betas = Betas::default_for(wire, oauth.is_some());
    let mut header_from_table = false;
    let mut merge_from_table = false;
    match raw.betas {
        Some(RawBetasField::List(values)) => betas.values = values,
        Some(RawBetasField::Table(table)) => {
            if let Some(values) = table.values {
                betas.values = values;
            }
            if let Some(header) = table.header {
                betas.header = header;
                header_from_table = true;
            }
            if let Some(merge) = table.merge {
                betas.merge = merge;
                merge_from_table = true;
            }
        }
        None => {}
    }
    if !header_from_table && let Some(header) = raw.beta_header {
        betas.header = header;
    }
    if !merge_from_table && let Some(merge) = raw.beta_merge {
        betas.merge = merge;
    }

    Ok(ResolvedProfile {
        schema_version,
        id,
        display_name: raw.display_name,
        dialect: Dialect {
            wire,
            stream_events,
            list_merge: raw.list_merge.unwrap_or_default(),
            tool_type_policy: raw.tool_type_policy.unwrap_or_default(),
            stream_unknown_policy: raw.stream_unknown_policy.unwrap_or_default(),
        },
        http: Http {
            base_url: raw.base_url,
            chat_path: raw
                .chat_path
                .or_else(|| wire.map(|w| w.default_chat_path().to_string())),
            auth_scheme: raw
                .auth_scheme
                .or_else(|| wire.map(Wire::default_auth_scheme)),
            headers: raw.headers.unwrap_or_default(),
            header_merge: raw.header_merge.unwrap_or_default(),
        },
        oauth,
        fingerprint: raw.fingerprint.map(resolve_fingerprint),
        betas,
    })
}

fn resolve_oauth(raw: RawOauth) -> Result<OauthPack, ProfileError> {
    let token_url = match raw.token_url {
        Some(url) if !url.is_empty() => url,
        _ => return Err(ProfileError::MissingField("token_url")),
    };

    let mut access_token_ptr = raw.access_token_ptr;
    let mut refresh_token_ptr = raw.refresh_token_ptr;
    let mut expires_ptr = raw.expires_ptr;
    let mut expires_unit = raw.expires_unit;
    if raw.creds_format == Some(CredsFormat::ClaudeCredentials) {
        if access_token_ptr.is_none() {
            access_token_ptr = Some("/claudeAiOauth/accessToken".into());
        }
        if refresh_token_ptr.is_none() {
            refresh_token_ptr = Some("/claudeAiOauth/refreshToken".into());
        }
        if expires_ptr.is_none() {
            expires_ptr = Some("/claudeAiOauth/expiresAt".into());
        }
        if expires_unit.is_none() {
            expires_unit = Some(ExpiresUnit::Ms);
        }
    } else if expires_unit.is_none() {
        expires_unit = Some(ExpiresUnit::S);
    }

    let pkce = match (raw.pkce, raw.authorize_url.is_some()) {
        (Some(v), _) => Some(v),
        (None, true) => Some(true),
        (None, false) => None,
    };

    Ok(OauthPack {
        token_url,
        token_url_fallback: raw.token_url_fallback,
        authorize_url: raw.authorize_url,
        authorize_params: raw.authorize_params.unwrap_or_default(),
        device_auth_url: raw.device_auth_url,
        client_id: raw.client_id,
        redirect_uri: raw.redirect_uri,
        scopes: raw.scopes.unwrap_or_default(),
        list_merge: raw.list_merge.unwrap_or_default(),
        pkce,
        refresh_grant: raw.refresh_grant.or_else(|| Some("refresh_token".into())),
        refresh_body: raw.refresh_body.unwrap_or_default(),
        token_request_format: raw.token_request_format,
        token_headers: raw.token_headers.unwrap_or_default(),
        creds_path: raw.creds_path,
        creds_format: raw.creds_format,
        access_token_ptr,
        refresh_token_ptr,
        expires_ptr,
        expires_unit,
        access_env: raw.access_env,
        login: raw.login,
        setup_token_hint: raw.setup_token_hint,
        keychain_service: raw.keychain_service,
        keychain_accounts: raw.keychain_accounts.unwrap_or_default(),
        token_response: raw.token_response.map(|t| TokenResponse {
            access_token_ptr: t.access_token_ptr,
            refresh_token_ptr: t.refresh_token_ptr,
            expires_ptr: t.expires_ptr,
            expires_unit: t.expires_unit,
        }),
    })
}

fn resolve_fingerprint(raw: RawFingerprint) -> Fingerprint {
    Fingerprint {
        user_agent: raw.user_agent,
        x_app: raw.x_app,
        system_prompt_prefix: raw.system_prompt_prefix,
        tool_name_case: raw.tool_name_case,
        forbidden_body_fields: raw.forbidden_body_fields.unwrap_or_default(),
        forbidden_field_policy: raw.forbidden_field_policy.unwrap_or_default(),
        extra_body: raw.extra_body.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_greater_is_hard_error() {
        let err = parse_profile_str("schema_version = 2\nid = \"x\"\n").unwrap_err();
        assert!(matches!(
            err,
            ProfileError::SchemaVersion { found: 2, max: 1 }
        ));
    }

    #[test]
    fn lesser_schema_version_accepted() {
        let p = parse_profile_str("schema_version = 0\nid = \"x\"\n").unwrap();
        assert_eq!(p.schema_version, 0);
    }

    #[test]
    fn unknown_keys_ignored() {
        let p = parse_profile_str("schema_version = 1\nid = \"x\"\nfuture_field = 1\n").unwrap();
        assert_eq!(p.id, "x");
    }

    #[test]
    fn json_camel_case_aliases() {
        let p = parse_profile_str(
            r#"{"schemaVersion":1,"id":"x","displayName":"X","chatPath":"/v1","authScheme":"none"}"#,
        )
        .unwrap();
        assert_eq!(p.display_name.as_deref(), Some("X"));
        assert_eq!(p.http.chat_path.as_deref(), Some("/v1"));
        assert_eq!(p.http.auth_scheme, Some(AuthScheme::None));
    }

    #[test]
    fn loopback_ipv4_and_ipv6_accepted() {
        let p = parse_profile_str(
            "schema_version = 1\nid = \"x\"\nbase_url = \"http://127.0.0.1:9\"\n",
        )
        .unwrap();
        assert_eq!(p.http.base_url.as_deref(), Some("http://127.0.0.1:9"));
        let p = parse_profile_str("schema_version = 1\nid = \"x\"\nbase_url = \"http://[::1]/\"\n")
            .unwrap();
        assert_eq!(p.http.base_url.as_deref(), Some("http://[::1]/"));
    }

    #[test]
    fn dollar_paren_and_backtick_refused() {
        assert!(matches!(
            parse_profile_str("schema_version = 1\nid = \"x\"\nbase_url = \"$(whoami)\"\n"),
            Err(ProfileError::Interpolation { .. })
        ));
        assert!(matches!(
            parse_profile_str("schema_version = 1\nid = \"x\"\nbase_url = \"`whoami`\"\n"),
            Err(ProfileError::Interpolation { .. })
        ));
    }

    #[test]
    fn wasm_path_refused() {
        assert!(matches!(
            parse_profile_str(
                "schema_version = 1\nid = \"x\"\nbase_url = \"https://example.invalid/p.wasm\"\n"
            ),
            Err(ProfileError::NativeModule { .. })
        ));
    }

    #[test]
    fn header_auth_scheme() {
        let p = parse_profile_str(
            "schema_version = 1\nid = \"x\"\nauth_scheme = \"header:X-Api-Key\"\n",
        )
        .unwrap();
        assert_eq!(
            p.http.auth_scheme,
            Some(AuthScheme::Header("X-Api-Key".into()))
        );
    }

    #[test]
    fn oauth_without_token_url_fails_closed() {
        let err = parse_profile_str("schema_version = 1\nid = \"x\"\n[oauth]\nclient_id = \"c\"\n")
            .unwrap_err();
        assert!(matches!(err, ProfileError::MissingField("token_url")));
    }
}

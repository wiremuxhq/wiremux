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
    resolve(parse_layer_text(text, None)?)
}

pub(crate) fn parse_layer_file(path: &Path) -> Result<RawProfile, ProfileError> {
    let text = fs::read_to_string(path).map_err(|source| ProfileError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_layer_text(&text, Some(path))
}

pub(crate) fn parse_layer_str(text: &str) -> Result<RawProfile, ProfileError> {
    parse_layer_text(text, None)
}

fn parse_layer_text(text: &str, path: Option<&Path>) -> Result<RawProfile, ProfileError> {
    let value = parse_to_value(text, path)?;
    refuse::scan(&value)?;
    refuse_nested_struct_tables(&value)?;
    let value = envsubst::walk(value);
    refuse::scan(&value)?;
    refuse_nested_struct_tables(&value)?;
    let raw: RawProfile = serde_json::from_value(value).map_err(|e| parse_error(path, e))?;
    if let Some(found) = raw.schema_version
        && found > SCHEMA_VERSION_MAX
    {
        return Err(ProfileError::SchemaVersion {
            found,
            max: SCHEMA_VERSION_MAX,
        });
    }
    Ok(raw)
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
        serde_json::from_str(text).map_err(|e| parse_error(path, e))
    } else {
        toml::from_str(text).map_err(|e| parse_error(path, e))
    }
}

/// Shipped presets are flat. Nested `[dialect]` / `[http]` / `[auth]`
/// tables match the Rust structs and are otherwise dropped by serde.
fn refuse_nested_struct_tables(value: &Value) -> Result<(), ProfileError> {
    let Some(obj) = value.as_object() else {
        return Ok(());
    };
    for (key, hint) in [
        (
            "dialect",
            "set top-level `wire` (chat-completions|messages|responses|gemini)",
        ),
        ("http", "set top-level `base_url` and `chat_path`"),
        ("auth", "set top-level `auth_scheme` and `access_env`"),
    ] {
        if obj.get(key).is_some_and(Value::is_object) {
            return Err(ProfileError::Parse(format!(
                "unknown table `{key}`; {hint}"
            )));
        }
    }
    Ok(())
}

fn parse_error(path: Option<&Path>, err: impl std::fmt::Display) -> ProfileError {
    match path {
        Some(path) => ProfileError::Parse(format!("{}: {err}", path.display())),
        None => ProfileError::Parse(err.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawProfile {
    #[serde(default, alias = "schemaVersion")]
    pub(crate) schema_version: Option<u32>,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default, alias = "displayName")]
    pub(crate) display_name: Option<String>,
    #[serde(default)]
    pub(crate) wire: Option<Wire>,
    #[serde(default, alias = "streamEvents")]
    pub(crate) stream_events: Option<Vec<String>>,
    #[serde(default, alias = "listMerge")]
    pub(crate) list_merge: Option<ListMerge>,
    #[serde(default, alias = "toolTypePolicy")]
    pub(crate) tool_type_policy: Option<ToolTypePolicy>,
    #[serde(default, alias = "streamUnknownPolicy")]
    pub(crate) stream_unknown_policy: Option<StreamUnknownPolicy>,
    #[serde(default, alias = "baseUrl")]
    pub(crate) base_url: Option<String>,
    #[serde(default, alias = "chatPath")]
    pub(crate) chat_path: Option<String>,
    #[serde(default, alias = "authScheme")]
    pub(crate) auth_scheme: Option<AuthScheme>,
    #[serde(default)]
    pub(crate) headers: Option<BTreeMap<String, String>>,
    #[serde(default, alias = "headerMerge")]
    pub(crate) header_merge: Option<ListMerge>,
    #[serde(default, alias = "accessEnv")]
    pub(crate) access_env: Option<RawAccessEnv>,
    #[serde(default)]
    pub(crate) oauth: Option<RawOauth>,
    #[serde(default)]
    pub(crate) fingerprint: Option<RawFingerprint>,
    #[serde(default)]
    pub(crate) betas: Option<RawBetasField>,
    #[serde(default, alias = "betaHeader")]
    pub(crate) beta_header: Option<String>,
    #[serde(default, alias = "betaMerge")]
    pub(crate) beta_merge: Option<ListMerge>,
}

/// Top-level `access_env`: one name or a first-wins list.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawAccessEnv {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawBetasField {
    List(Vec<String>),
    Table(RawBetasTable),
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawBetasTable {
    #[serde(default)]
    pub(crate) values: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) header: Option<String>,
    #[serde(default)]
    pub(crate) merge: Option<ListMerge>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawOauth {
    #[serde(default, alias = "tokenUrl")]
    pub(crate) token_url: Option<String>,
    #[serde(default, alias = "tokenUrlFallback")]
    pub(crate) token_url_fallback: Option<String>,
    #[serde(default, alias = "authorizeUrl")]
    pub(crate) authorize_url: Option<String>,
    #[serde(default, alias = "authorizeParams")]
    pub(crate) authorize_params: Option<BTreeMap<String, String>>,
    #[serde(default, alias = "deviceAuthUrl")]
    pub(crate) device_auth_url: Option<String>,
    #[serde(default, alias = "clientId")]
    pub(crate) client_id: Option<String>,
    #[serde(default, alias = "redirectUri")]
    pub(crate) redirect_uri: Option<String>,
    #[serde(default)]
    pub(crate) scopes: Option<Vec<String>>,
    #[serde(default, alias = "listMerge")]
    pub(crate) list_merge: Option<ListMerge>,
    #[serde(default)]
    pub(crate) pkce: Option<bool>,
    #[serde(default, alias = "refreshGrant")]
    pub(crate) refresh_grant: Option<String>,
    #[serde(default, alias = "refreshBody")]
    pub(crate) refresh_body: Option<BTreeMap<String, String>>,
    #[serde(default, alias = "tokenRequestFormat")]
    pub(crate) token_request_format: Option<TokenRequestFormat>,
    #[serde(default, alias = "tokenHeaders")]
    pub(crate) token_headers: Option<BTreeMap<String, String>>,
    #[serde(default, alias = "credsPath")]
    pub(crate) creds_path: Option<String>,
    #[serde(default, alias = "credsFormat")]
    pub(crate) creds_format: Option<CredsFormat>,
    #[serde(default, alias = "accessTokenPtr")]
    pub(crate) access_token_ptr: Option<String>,
    #[serde(default, alias = "refreshTokenPtr")]
    pub(crate) refresh_token_ptr: Option<String>,
    #[serde(default, alias = "expiresPtr")]
    pub(crate) expires_ptr: Option<String>,
    #[serde(default, alias = "expiresUnit")]
    pub(crate) expires_unit: Option<ExpiresUnit>,
    #[serde(default, alias = "accessEnv")]
    pub(crate) access_env: Option<String>,
    #[serde(default)]
    pub(crate) login: Option<Login>,
    #[serde(default, alias = "setupTokenHint")]
    pub(crate) setup_token_hint: Option<String>,
    #[serde(default, alias = "keychainService")]
    pub(crate) keychain_service: Option<String>,
    #[serde(default, alias = "keychainAccounts")]
    pub(crate) keychain_accounts: Option<Vec<String>>,
    #[serde(default, alias = "tokenResponse")]
    pub(crate) token_response: Option<RawTokenResponse>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTokenResponse {
    #[serde(default, alias = "accessTokenPtr")]
    pub(crate) access_token_ptr: Option<String>,
    #[serde(default, alias = "refreshTokenPtr")]
    pub(crate) refresh_token_ptr: Option<String>,
    #[serde(default, alias = "expiresPtr")]
    pub(crate) expires_ptr: Option<String>,
    #[serde(default, alias = "expiresUnit")]
    pub(crate) expires_unit: Option<ExpiresUnit>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawFingerprint {
    #[serde(default, alias = "userAgent")]
    pub(crate) user_agent: Option<String>,
    #[serde(default, alias = "xApp")]
    pub(crate) x_app: Option<String>,
    #[serde(default, alias = "systemPromptPrefix")]
    pub(crate) system_prompt_prefix: Option<String>,
    #[serde(default, alias = "toolNameCase")]
    pub(crate) tool_name_case: Option<ToolNameCase>,
    #[serde(default, alias = "forbiddenBodyFields")]
    pub(crate) forbidden_body_fields: Option<Vec<String>>,
    #[serde(default, alias = "forbiddenFieldPolicy")]
    pub(crate) forbidden_field_policy: Option<ForbiddenFieldPolicy>,
    #[serde(default, alias = "extraBody")]
    pub(crate) extra_body: Option<BTreeMap<String, Value>>,
}

pub(crate) fn resolve(raw: RawProfile) -> Result<ResolvedProfile, ProfileError> {
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
        access_env: resolve_access_env(raw.access_env),
        fingerprint: raw.fingerprint.map(resolve_fingerprint),
        betas,
    })
}

fn resolve_access_env(raw: Option<RawAccessEnv>) -> Vec<String> {
    match raw {
        None => Vec::new(),
        Some(RawAccessEnv::One(name)) => {
            if name.is_empty() {
                Vec::new()
            } else {
                vec![name]
            }
        }
        Some(RawAccessEnv::Many(names)) => names.into_iter().filter(|s| !s.is_empty()).collect(),
    }
}

fn resolve_oauth(raw: RawOauth) -> Result<OauthPack, ProfileError> {
    let token_url = match raw.token_url {
        Some(url) if !url.is_empty() => url,
        _ => return Err(ProfileError::MissingField("oauth.token_url")),
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
    fn nested_dialect_http_auth_tables_are_refused() {
        let err =
            parse_profile_str("schema_version = 1\nid = \"x\"\n[dialect]\nwire = \"chatt\"\n")
                .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("unknown table `dialect`") && text.contains("top-level `wire`"),
            "nested [dialect] must fail closed, got {text}"
        );
        let err = parse_profile_str(
            "schema_version = 1\nid = \"x\"\n[http]\nbase_url = \"https://api.example.test\"\n",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown table `http`"),
            "nested [http] must fail closed, got {err}"
        );
        let err =
            parse_profile_str("schema_version = 1\nid = \"x\"\n[auth]\nscheme = \"bearer\"\n")
                .unwrap_err();
        assert!(
            err.to_string().contains("unknown table `auth`"),
            "nested [auth] must fail closed, got {err}"
        );
    }

    #[test]
    fn missing_schema_version_names_legal_value() {
        let err = parse_profile_str("id = \"x\"\n").unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("schema_version") && text.contains("set to 1"),
            "missing schema_version must name the legal value, got {text}"
        );
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
    fn access_env_accepts_string_or_array() {
        let one =
            parse_profile_str("schema_version = 1\nid = \"x\"\naccess_env = \"OPENAI_API_KEY\"\n")
                .unwrap();
        assert_eq!(one.access_env, ["OPENAI_API_KEY"]);
        let many = parse_profile_str(
            "schema_version = 1\nid = \"x\"\naccess_env = [\"ANTHROPIC_AUTH_TOKEN\", \"ANTHROPIC_API_KEY\"]\n",
        )
        .unwrap();
        assert_eq!(
            many.access_env,
            ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"]
        );
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
    fn unknown_stream_unknown_policy_names_field_and_suggests() {
        let err = parse_profile_str(
            "schema_version = 1\nid = \"x\"\nstream_unknown_policy = \"hard_error\"\n",
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("stream_unknown_policy"),
            "must name the field, got {text}"
        );
        assert!(
            text.contains("hard-error") && text.contains("passthrough"),
            "must list legal values, got {text}"
        );
        assert!(
            text.to_ascii_lowercase().contains("did you mean") && text.contains("`hard-error`"),
            "hard_error should suggest hard-error, got {text}"
        );
    }

    #[test]
    fn unknown_wire_chat_suggests_chat_completions() {
        let err =
            parse_profile_str("schema_version = 1\nid = \"x\"\nwire = \"chat\"\n").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("unknown wire `chat`"), "{text}");
        assert!(
            text.contains("chat-completions")
                && text.contains("messages")
                && text.contains("responses")
                && text.contains("gemini"),
            "must list legal wire values, got {text}"
        );
        assert!(
            text.to_ascii_lowercase().contains("did you mean")
                && text.contains("`chat-completions`"),
            "CLI-only chat alias must suggest chat-completions in a profile, got {text}"
        );
    }

    #[test]
    fn oauth_without_token_url_fails_closed() {
        let err = parse_profile_str("schema_version = 1\nid = \"x\"\n[oauth]\nclient_id = \"c\"\n")
            .unwrap_err();
        assert!(
            matches!(err, ProfileError::MissingField("oauth.token_url")),
            "missing token_url must name oauth.token_url, got {err}"
        );
        assert!(
            err.to_string().contains("oauth.token_url"),
            "display must name oauth.token_url, got {err}"
        );
    }

    #[test]
    fn padded_chat_path_urls_are_refused() {
        assert!(matches!(
            parse_profile_str(
                "schema_version = 1\nid = \"x\"\nchat_path = \" javascript:alert(1)\"\n"
            ),
            Err(ProfileError::DisallowedUrl { .. })
        ));
        assert!(matches!(
            parse_profile_str(
                "schema_version = 1\nid = \"x\"\nchat_path = \" http://192.0.2.1/v1\"\n"
            ),
            Err(ProfileError::DisallowedUrl { .. })
        ));
        let p = parse_profile_str("schema_version = 1\nid = \"x\"\nchat_path = \"/v1/messages\"\n")
            .unwrap();
        assert_eq!(p.http.chat_path.as_deref(), Some("/v1/messages"));
    }

    #[test]
    fn env_injected_url_is_refused() {
        let var = "WIREMUX_TEST_INJECT_URL_7b1a";
        let err = with_env(var, "http://192.0.2.1", || {
            parse_profile_str(&format!(
                "schema_version = 1\nid = \"x\"\nbase_url = \"{{env:{var}}}\"\n"
            ))
        })
        .unwrap_err();
        assert!(
            matches!(err, ProfileError::DisallowedUrl { .. }),
            "env-injected non-loopback http must refuse, got {err}"
        );
    }

    #[test]
    fn disallowed_url_display_redacts_envsubst_path() {
        let var = "WIREMUX_TEST_ENVSUBST_URL_51";
        let leak = "ghp_ENVSUBST_LEAK_TOKEN_51";
        let err = with_env(var, &format!("http://192.0.2.1/{leak}"), || {
            parse_profile_str(&format!(
                "schema_version = 1\nid = \"x\"\nbase_url = \"{{env:{var}}}\"\n"
            ))
        })
        .unwrap_err();
        assert!(
            matches!(err, ProfileError::DisallowedUrl { .. }),
            "env-injected non-loopback http must refuse, got {err}"
        );
        let text = err.to_string();
        assert!(
            !text.contains(leak),
            "DisallowedUrl Display leaked envsubst token: {text}"
        );
        assert!(
            text.contains("192.0.2.1"),
            "Display must still name the origin, got {text}"
        );
    }

    #[test]
    fn hint_fields_allow_backticks_not_command() {
        let ok = parse_profile_str(
            "schema_version = 1\nid = \"x\"\ndisplay_name = \"run `tool`\"\n[oauth]\ntoken_url = \"https://auth.example.invalid/token\"\nsetup_token_hint = \"run `claude setup-token`\"\n",
        )
        .unwrap();
        assert!(ok.display_name.as_deref().unwrap().contains('`'));
        assert!(
            ok.oauth
                .as_ref()
                .unwrap()
                .setup_token_hint
                .as_deref()
                .unwrap()
                .contains('`')
        );

        assert!(matches!(
            parse_profile_str("schema_version = 1\nid = \"x\"\ndisplay_name = \"!command x\"\n"),
            Err(ProfileError::Interpolation { .. })
        ));
        assert!(matches!(
            parse_profile_str(
                "schema_version = 1\nid = \"x\"\n[headers]\nsetup_token_hint = \"!command curl\"\n"
            ),
            Err(ProfileError::Interpolation { .. })
        ));
        assert!(matches!(
            parse_profile_str(
                "schema_version = 1\nid = \"x\"\n[headers]\nsetup_token_hint = \"run `x`\"\n"
            ),
            Err(ProfileError::Interpolation { .. })
        ));
    }

    fn with_env<T>(key: &str, val: &str, f: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(key).ok();
        // SAFETY: process-global lock; restored by EnvRestore on all paths.
        unsafe {
            std::env::set_var(key, val);
        }
        let _restore = EnvRestore {
            key: key.to_string(),
            prev,
        };
        f()
    }

    struct EnvRestore {
        key: String,
        prev: Option<String>,
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            // SAFETY: same lock as with_env is still held by the caller.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(&self.key, v),
                    None => std::env::remove_var(&self.key),
                }
            }
        }
    }
}

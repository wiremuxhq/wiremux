//! Profile AST (data only). No IR.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::ProfileError;

/// Highest `schema_version` this crate understands.
pub const SCHEMA_VERSION_MAX: u32 = 1;

/// Catalog load inputs. Same-id layers merge field-wise
/// (shipped < user dir < explicit file).
#[derive(Debug, Clone)]
pub struct LoadOptions<'a> {
    /// Optional id hint (required unless an explicit file supplies `id`).
    pub id: Option<&'a str>,
    /// Extra file to include, keyed by its document `id`.
    pub explicit_file: Option<&'a Path>,
    /// Extra directories of profile files (`WIREMUX_PROFILE_DIR`).
    pub extra_profile_dirs: Vec<PathBuf>,
    /// When false, skip crate-shipped presets.
    pub include_shipped: bool,
    /// When false, skip XDG/HOME user dirs and `WIREMUX_PROFILE_DIR`.
    pub include_user_config: bool,
}

impl Default for LoadOptions<'_> {
    fn default() -> Self {
        Self {
            id: None,
            explicit_file: None,
            extra_profile_dirs: Vec::new(),
            include_shipped: true,
            include_user_config: true,
        }
    }
}

/// Fully resolved profile document (one `id`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    /// Schema version after parse (`<= SCHEMA_VERSION_MAX`).
    pub schema_version: u32,
    /// Stable catalog key.
    pub id: String,
    /// Optional human label.
    pub display_name: Option<String>,
    /// Dialect table (root-level fields in the file).
    pub dialect: Dialect,
    /// HTTP table (root-level fields in the file).
    pub http: Http,
    /// Optional OAuth pack.
    pub oauth: Option<OauthPack>,
    /// Optional fingerprint table.
    pub fingerprint: Option<Fingerprint>,
    /// Beta header list and merge policy.
    pub betas: Betas,
}

/// Dialect selection and stream/tool policies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialect {
    /// `chat-completions` | `messages` | `responses`.
    pub wire: Option<Wire>,
    /// Recognized SSE event names (built-in list when omitted).
    pub stream_events: Vec<String>,
    /// Merge policy for `stream_events` (and `oauth.scopes` when set there).
    pub list_merge: ListMerge,
    /// How to treat namespaced / hosted tool types.
    pub tool_type_policy: ToolTypePolicy,
    /// How to treat unrecognized stream events.
    pub stream_unknown_policy: StreamUnknownPolicy,
}

/// v1 wire dialects. Not a host registry name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wire {
    /// OpenAI Chat Completions.
    ChatCompletions,
    /// Anthropic Messages.
    Messages,
    /// OpenAI Responses.
    Responses,
}

impl Wire {
    /// Built-in SSE event names when `stream_events` is omitted.
    #[must_use]
    pub fn default_stream_events(self) -> &'static [&'static str] {
        match self {
            Self::ChatCompletions => &["chunk", "[DONE]"],
            Self::Messages => &[
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
                "ping",
                "error",
            ],
            Self::Responses => &[
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.function_call_arguments.delta",
                "response.output_item.done",
                "response.completed",
                "response.failed",
                "response.incomplete",
            ],
        }
    }

    /// Dialect default chat path.
    #[must_use]
    pub fn default_chat_path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/v1/chat/completions",
            Self::Messages => "/v1/messages",
            Self::Responses => "/v1/responses",
        }
    }

    /// Dialect default auth scheme.
    #[must_use]
    pub fn default_auth_scheme(self) -> AuthScheme {
        match self {
            Self::Messages => AuthScheme::XApiKey,
            Self::ChatCompletions | Self::Responses => AuthScheme::Bearer,
        }
    }
}

/// List merge policy. `verbatim` is an alias of `replace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListMerge {
    /// Later items overlay; earlier unique values remain.
    #[default]
    Union,
    /// Later list or map is the entire set.
    Replace,
}

impl ListMerge {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "union" => Some(Self::Union),
            "replace" | "verbatim" => Some(Self::Replace),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for ListMerge {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s)
            .ok_or_else(|| D::Error::unknown_variant(&s, &["union", "replace", "verbatim"]))
    }
}

/// How encode treats unknown / namespaced tool types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolTypePolicy {
    /// Forward the tool type as-is.
    Passthrough,
    /// Flatten `namespace.tool` into the target dialect.
    FlattenNamespace,
    /// Fail the map.
    #[default]
    HardError,
}

/// How decode treats unrecognized SSE events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamUnknownPolicy {
    /// Fail the map.
    #[default]
    HardError,
    /// Forward the raw SSE frame.
    Passthrough,
}

/// HTTP request surface (API calls, not the token POST).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Http {
    /// `https` or loopback `http`.
    pub base_url: Option<String>,
    /// Request path, or an absolute URL.
    pub chat_path: Option<String>,
    /// How the access token is sent.
    pub auth_scheme: Option<AuthScheme>,
    /// Extra API headers (after env subst).
    pub headers: BTreeMap<String, String>,
    /// Merge policy for `headers`.
    pub header_merge: ListMerge,
}

/// API auth scheme. `none` sends no `Authorization` and no `x-api-key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthScheme {
    /// `Authorization: Bearer <token>`.
    Bearer,
    /// `x-api-key: <token>`.
    XApiKey,
    /// Named header (`header:<name>`).
    Header(String),
    /// No auth header.
    None,
}

impl AuthScheme {
    pub(crate) fn parse(s: &str) -> Result<Self, ProfileError> {
        if let Some(name) = s.strip_prefix("header:") {
            if name.is_empty() {
                return Err(ProfileError::Parse(
                    "auth_scheme header: requires a header name".into(),
                ));
            }
            return Ok(Self::Header(name.to_string()));
        }
        match s {
            "bearer" => Ok(Self::Bearer),
            "x-api-key" => Ok(Self::XApiKey),
            "none" => Ok(Self::None),
            other => Err(ProfileError::Parse(format!(
                "unknown auth_scheme `{other}`"
            ))),
        }
    }
}

impl<'de> Deserialize<'de> for AuthScheme {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(D::Error::custom)
    }
}

impl Serialize for AuthScheme {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let owned: String;
        let s = match self {
            Self::Bearer => "bearer",
            Self::XApiKey => "x-api-key",
            Self::Header(name) => {
                owned = format!("header:{name}");
                owned.as_str()
            }
            Self::None => "none",
        };
        serializer.serialize_str(s)
    }
}

/// The `[oauth]` table: token POST and login surface.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthPack {
    /// Token endpoint. Required when `[oauth]` is present.
    pub token_url: String,
    /// Used only on HTTP 404 from `token_url`.
    pub token_url_fallback: Option<String>,
    /// Authorize URL. Absent means no login in this profile.
    pub authorize_url: Option<String>,
    /// Extra authorize query parameters.
    pub authorize_params: BTreeMap<String, String>,
    /// Device-flow endpoint.
    pub device_auth_url: Option<String>,
    /// Public client id. May be empty; login then exits 2.
    pub client_id: Option<String>,
    /// PKCE loopback redirect.
    pub redirect_uri: Option<String>,
    /// OAuth scopes.
    pub scopes: Vec<String>,
    /// Merge policy for `scopes`.
    pub list_merge: ListMerge,
    /// PKCE flag. Default true when `authorize_url` is set.
    pub pkce: Option<bool>,
    /// `grant_type` when `refresh_body` omits it.
    pub refresh_grant: Option<String>,
    /// Extra token-POST body fields (data only).
    pub refresh_body: BTreeMap<String, String>,
    /// Token POST encoding.
    pub token_request_format: Option<TokenRequestFormat>,
    /// Headers on the token POST only.
    pub token_headers: BTreeMap<String, String>,
    /// Credential store path (`~` and `{env:}` allowed).
    pub creds_path: Option<String>,
    /// How to read/write the credential store.
    pub creds_format: Option<CredsFormat>,
    /// JSON Pointer into the store for the access token.
    pub access_token_ptr: Option<String>,
    /// JSON Pointer into the store for the refresh token.
    pub refresh_token_ptr: Option<String>,
    /// JSON Pointer into the store for expiry.
    pub expires_ptr: Option<String>,
    /// Unit of the store expiry field.
    pub expires_unit: Option<ExpiresUnit>,
    /// Env fallback for the access token.
    pub access_env: Option<String>,
    /// Login mode.
    pub login: Option<Login>,
    /// Free text printed for `login = "setup-token"`. Backticks allowed.
    pub setup_token_hint: Option<String>,
    /// Optional macOS keychain service.
    pub keychain_service: Option<String>,
    /// Optional keychain account names.
    pub keychain_accounts: Vec<String>,
    /// Pointers into the HTTP refresh response (not the store).
    pub token_response: Option<TokenResponse>,
}

/// `[oauth.token_response]` pointers into the refresh HTTP body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenResponse {
    /// Access token pointer. RFC 6749 default `/access_token`.
    pub access_token_ptr: Option<String>,
    /// Refresh token pointer. RFC 6749 default `/refresh_token`.
    pub refresh_token_ptr: Option<String>,
    /// Expiry pointer. RFC 6749 default `/expires_in`.
    pub expires_ptr: Option<String>,
    /// Expiry unit. RFC 6749 default `s`.
    pub expires_unit: Option<ExpiresUnit>,
}

/// Token POST body encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenRequestFormat {
    /// JSON body (Anthropic).
    Json,
    /// Form body (typical OIDC).
    Form,
}

/// Named credential-store layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredsFormat {
    /// Explicit JSON Pointers on the pack.
    JsonPointer,
    /// Claude credentials.json alias map.
    ClaudeCredentials,
    /// Named profile object inside an auth.json.
    OidcAuthJson,
    /// GitHub Copilot hosts.json layout.
    CopilotHosts,
}

/// Expiry field unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExpiresUnit {
    /// Milliseconds since epoch (Claude credentials).
    Ms,
    /// Seconds (`expires_in` / unix time).
    S,
}

/// Login engine selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Login {
    /// Print `setup_token_hint` and exit 2.
    SetupToken,
    /// PKCE loopback.
    Pkce,
    /// Device authorization grant.
    Device,
    /// Not ready / disabled.
    None,
}

/// Encode-time fingerprint (API request only).
#[derive(Debug, Clone, PartialEq)]
pub struct Fingerprint {
    /// `User-Agent` header.
    pub user_agent: Option<String>,
    /// `x-app` header.
    pub x_app: Option<String>,
    /// Prepended to the first system item.
    pub system_prompt_prefix: Option<String>,
    /// Tool name case conversion.
    pub tool_name_case: Option<ToolNameCase>,
    /// Keys the encode path must not send.
    pub forbidden_body_fields: Vec<String>,
    /// What to do when a forbidden field is present.
    pub forbidden_field_policy: ForbiddenFieldPolicy,
    /// Data-only JSON keys merged into the API body.
    pub extra_body: BTreeMap<String, serde_json::Value>,
}

/// Tool-name case conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolNameCase {
    /// Leave names unchanged.
    AsIs,
    /// `snake_case`.
    Snake,
    /// `kebab-case`.
    Kebab,
}

/// Encode-time policy for `forbidden_body_fields`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForbiddenFieldPolicy {
    /// Drop the field.
    Strip,
    /// Fail encode.
    #[default]
    HardError,
}

/// Beta header values and merge policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Betas {
    /// Header values, e.g. `oauth-2025-04-20`.
    pub values: Vec<String>,
    /// Header name. Default `anthropic-beta`.
    pub header: String,
    /// `union` or `replace` (`verbatim` alias).
    pub merge: ListMerge,
}

impl Betas {
    pub(crate) fn default_for(wire: Option<Wire>, has_oauth: bool) -> Self {
        let merge = if matches!(wire, Some(Wire::Messages)) && has_oauth {
            ListMerge::Replace
        } else {
            ListMerge::Union
        };
        Self {
            values: Vec::new(),
            header: "anthropic-beta".into(),
            merge,
        }
    }
}

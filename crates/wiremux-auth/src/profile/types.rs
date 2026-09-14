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
    /// Env names for a static key when `[oauth]` is absent. First non-empty wins.
    pub access_env: Vec<String>,
    /// Optional fingerprint table.
    pub fingerprint: Option<Fingerprint>,
    /// Beta header list and merge policy.
    pub betas: Betas,
}

/// Dialect selection and stream/tool policies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialect {
    /// `chat-completions` | `messages` | `responses` | `gemini`.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wire {
    /// OpenAI Chat Completions.
    ChatCompletions,
    /// Anthropic Messages.
    Messages,
    /// OpenAI Responses.
    Responses,
    /// Google Gemini generateContent.
    Gemini,
}

impl Wire {
    /// Catalog spellings. CLI `--from` also accepts `chat` for `chat-completions`.
    pub const NAMES: &'static [&'static str] =
        &["chat-completions", "messages", "responses", "gemini"];

    /// Catalog / file spelling (`messages`, `chat-completions`, `responses`, `gemini`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat-completions",
            Self::Messages => "messages",
            Self::Responses => "responses",
            Self::Gemini => "gemini",
        }
    }

    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "chat-completions" => Ok(Self::ChatCompletions),
            "messages" => Ok(Self::Messages),
            "responses" => Ok(Self::Responses),
            "gemini" => Ok(Self::Gemini),
            other => Err(unknown_kebab("wire", other, Self::NAMES)),
        }
    }

    /// Close match after folding case and `_`/`-`.
    #[must_use]
    pub fn suggest(s: &str) -> Option<&'static str> {
        suggest_kebab(s, Self::NAMES)
    }

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
                "response.reasoning_summary_text.delta",
                "response.reasoning.delta",
                "response.refusal.delta",
                "response.function_call_arguments.delta",
                "response.output_item.done",
                "response.completed",
                "response.failed",
                "response.incomplete",
            ],
            Self::Gemini => &["chunk"],
        }
    }

    /// Dialect default chat path.
    #[must_use]
    pub fn default_chat_path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/v1/chat/completions",
            Self::Messages => "/v1/messages",
            Self::Responses => "/v1/responses",
            Self::Gemini => "/v1beta/models/{model}:generateContent",
        }
    }

    /// Dialect default auth scheme.
    #[must_use]
    pub fn default_auth_scheme(self) -> AuthScheme {
        match self {
            Self::Messages => AuthScheme::XApiKey,
            Self::Gemini => AuthScheme::Header("x-goog-api-key".into()),
            Self::ChatCompletions | Self::Responses => AuthScheme::Bearer,
        }
    }
}

impl<'de> Deserialize<'de> for Wire {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
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
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "union" => Ok(Self::Union),
            "replace" | "verbatim" => Ok(Self::Replace),
            other => Err(unknown_kebab(
                "list_merge",
                other,
                &["union", "replace", "verbatim"],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for ListMerge {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
}

/// How encode treats unknown / namespaced tool types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
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

impl ToolTypePolicy {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "passthrough" => Ok(Self::Passthrough),
            "flatten-namespace" => Ok(Self::FlattenNamespace),
            "hard-error" => Ok(Self::HardError),
            other => Err(unknown_kebab(
                "tool_type_policy",
                other,
                &["passthrough", "flatten-namespace", "hard-error"],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for ToolTypePolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
}

/// How decode treats unrecognized SSE events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamUnknownPolicy {
    /// Fail the map.
    #[default]
    HardError,
    /// Forward the raw SSE frame.
    Passthrough,
}

impl StreamUnknownPolicy {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "hard-error" => Ok(Self::HardError),
            "passthrough" => Ok(Self::Passthrough),
            other => Err(unknown_kebab(
                "stream_unknown_policy",
                other,
                &["hard-error", "passthrough"],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for StreamUnknownPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
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
            other => {
                let listed = "bearer|x-api-key|none|header:<name>";
                let mut msg = format!("unknown auth_scheme `{other}` ({listed})");
                if let Some(suggest) = suggest_auth_scheme(other) {
                    msg.push_str(&format!("; did you mean `{suggest}`"));
                }
                Err(ProfileError::Parse(msg))
            }
        }
    }
}

const AUTH_SCHEME_NAMES: &[&str] = &["bearer", "x-api-key", "none", "header:<name>"];

fn suggest_auth_scheme(s: &str) -> Option<&'static str> {
    suggest_kebab(s, AUTH_SCHEME_NAMES)
}

fn fold_enum_key(s: &str) -> String {
    s.to_ascii_lowercase().replace(['_', '-'], "")
}

/// Close match after folding case and `_`/`-`.
pub(crate) fn suggest_kebab<'a>(input: &str, legal: &[&'a str]) -> Option<&'a str> {
    let folded = fold_enum_key(input);
    if folded.is_empty() {
        return None;
    }
    let mut close = Vec::new();
    for &opt in legal {
        let candidate = fold_enum_key(opt);
        if candidate == folded {
            return Some(opt);
        }
        if candidate.starts_with(&folded)
            || folded.starts_with(&candidate)
            || candidate.ends_with(&folded)
            || folded.ends_with(&candidate)
        {
            close.push(opt);
        }
    }
    match close.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

fn unknown_kebab(field: &str, got: &str, legal: &[&str]) -> String {
    let listed = legal.join("|");
    let mut msg = format!("unknown {field} `{got}` ({listed})");
    if let Some(suggest) = suggest_kebab(got, legal) {
        msg.push_str(&format!("; did you mean `{suggest}`"));
    }
    msg
}

fn deserialize_kebab<'de, T, D>(
    deserializer: D,
    parse: fn(&str) -> Result<T, String>,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse(&s).map_err(D::Error::custom)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenRequestFormat {
    /// JSON body (Anthropic).
    Json,
    /// Form body (typical OIDC).
    Form,
}

impl TokenRequestFormat {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "json" => Ok(Self::Json),
            "form" => Ok(Self::Form),
            other => Err(unknown_kebab(
                "token_request_format",
                other,
                &["json", "form"],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for TokenRequestFormat {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
}

/// Named credential-store layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredsFormat {
    /// Explicit JSON Pointers on the pack.
    JsonPointer,
    /// Claude credentials.json alias map.
    ClaudeCredentials,
    /// Named `{issuer}::{client_id}` object inside an auth.json.
    ///
    /// Pointers: `/{issuer}::{client_id}/key` (access), `/refresh_token`,
    /// `/expires_at` (RFC 3339). `issuer` is the origin of `authorize_url`,
    /// else `token_url`. A `{issuer}::{client_id}@{name}` suffix matches
    /// when the exact key is absent.
    OidcAuthJson,
    /// GitHub Copilot `hosts.json` / `apps.json` layout.
    ///
    /// Pointer: `/{host}/oauth_token` (first object that has a non-empty
    /// token; default host `github.com`). GitHub Copilot product terms
    /// apply to the token. This crate does not ship a Copilot preset.
    CopilotHosts,
}

impl CredsFormat {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "json-pointer" => Ok(Self::JsonPointer),
            "claude-credentials" => Ok(Self::ClaudeCredentials),
            "oidc-auth-json" => Ok(Self::OidcAuthJson),
            "copilot-hosts" => Ok(Self::CopilotHosts),
            other => Err(unknown_kebab(
                "creds_format",
                other,
                &[
                    "json-pointer",
                    "claude-credentials",
                    "oidc-auth-json",
                    "copilot-hosts",
                ],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for CredsFormat {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
}

/// Expiry field unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExpiresUnit {
    /// Milliseconds since epoch (Claude credentials).
    Ms,
    /// Seconds (`expires_in` / unix time).
    S,
}

impl ExpiresUnit {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "ms" => Ok(Self::Ms),
            "s" => Ok(Self::S),
            other => Err(unknown_kebab("expires_unit", other, &["ms", "s"])),
        }
    }
}

impl<'de> Deserialize<'de> for ExpiresUnit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
}

/// Login engine selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

impl Login {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "setup-token" => Ok(Self::SetupToken),
            "pkce" => Ok(Self::Pkce),
            "device" => Ok(Self::Device),
            "none" => Ok(Self::None),
            other => Err(unknown_kebab(
                "login",
                other,
                &["setup-token", "pkce", "device", "none"],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for Login {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolNameCase {
    /// Leave names unchanged.
    AsIs,
    /// `snake_case`.
    Snake,
    /// `kebab-case`.
    Kebab,
}

impl ToolNameCase {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "as-is" => Ok(Self::AsIs),
            "snake" => Ok(Self::Snake),
            "kebab" => Ok(Self::Kebab),
            other => Err(unknown_kebab(
                "tool_name_case",
                other,
                &["as-is", "snake", "kebab"],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for ToolNameCase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
}

/// Encode-time policy for `forbidden_body_fields`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForbiddenFieldPolicy {
    /// Drop the field.
    Strip,
    /// Fail encode.
    #[default]
    HardError,
}

impl ForbiddenFieldPolicy {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "strip" => Ok(Self::Strip),
            "hard-error" => Ok(Self::HardError),
            other => Err(unknown_kebab(
                "forbidden_field_policy",
                other,
                &["strip", "hard-error"],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for ForbiddenFieldPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_kebab(deserializer, Self::parse)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown_text(input: &str) -> String {
        AuthScheme::parse(input)
            .expect_err("must reject")
            .to_string()
    }

    #[test]
    fn unknown_auth_scheme_lists_legal_values() {
        let text = unknown_text("oauth");
        assert!(text.contains("unknown auth_scheme `oauth`"), "{text}");
        assert!(
            text.contains("bearer")
                && text.contains("x-api-key")
                && text.contains("none")
                && text.contains("header:<name>"),
            "must list legal auth_scheme values, got {text}"
        );
    }

    #[test]
    fn unknown_auth_scheme_suggests_close_matches() {
        let bearer = unknown_text("Bearer");
        assert!(
            bearer.contains("bearer")
                && bearer.contains("x-api-key")
                && bearer.contains("none")
                && bearer.contains("header:<name>"),
            "must list legal values, got {bearer}"
        );
        assert!(
            bearer.contains("bearer"),
            "Bearer must point at bearer, got {bearer}"
        );
        assert!(
            bearer.to_ascii_lowercase().contains("did you mean") && bearer.contains("`bearer`"),
            "Bearer should suggest bearer, got {bearer}"
        );

        for input in ["api-key", "x_api_key", "X-Api-Key"] {
            let text = unknown_text(input);
            assert!(
                text.contains("x-api-key")
                    && text.contains("bearer")
                    && text.contains("none")
                    && text.contains("header:<name>"),
                "{input} must list legal values, got {text}"
            );
            assert!(
                text.to_ascii_lowercase().contains("did you mean") && text.contains("`x-api-key`"),
                "{input} should suggest x-api-key, got {text}"
            );
        }
    }
}

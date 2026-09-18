//! Turn a public vendor catalog into user-dir profile TOML.
//!
//! Default source is [models.dev](https://models.dev/api.json) (id, env,
//! npm, optional `api`). LiteLLM `providers.json` is a second, long-tail
//! openai-like list. First-party Vercel packages omit `api` by schema;
//! those popular OpenAI-compat hosts use the well-known table below
//! (same defaults as `@ai-sdk/groq` and siblings).
//!
//! This fetches a catalog of endpoints, not a remote profile document.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wiremux_auth::{LoadOptions, list_profiles, parse_profile_str};

/// Popular OpenAI-compat hosts to emit when `--vendor` is omitted.
pub const POPULAR_VENDORS: &[&str] = &[
    "groq",
    "deepseek",
    "togetherai",
    "fireworks-ai",
    "mistral",
    "cerebras",
];

/// models.dev living catalog.
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// LiteLLM openai-like slug list (not the 216 Python providers).
pub const LITELLM_PROVIDERS_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/litellm/llms/openai_like/providers.json";

/// Which public catalog to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogKind {
    /// <https://models.dev/api.json>
    ModelsDev,
    /// LiteLLM `openai_like/providers.json`
    LiteLlm,
}

impl CatalogKind {
    /// Parse `--source`.
    pub fn parse_name(name: &str) -> Result<Self, IngestError> {
        match name {
            "models-dev" | "models.dev" | "modelsdev" => Ok(Self::ModelsDev),
            "litellm" => Ok(Self::LiteLlm),
            other => Err(IngestError::Message(format!(
                "unknown catalog source `{other}` (models-dev or litellm)"
            ))),
        }
    }

    /// Allowlisted fetch URL for this source.
    pub fn default_url(self) -> &'static str {
        match self {
            Self::ModelsDev => MODELS_DEV_URL,
            Self::LiteLlm => LITELLM_PROVIDERS_URL,
        }
    }
}

/// One catalog row we can maybe turn into a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogVendor {
    /// Catalog key (`groq`, `deepseek`, …).
    pub id: String,
    /// Human label.
    pub display_name: String,
    /// OpenAI-compat API root when the catalog publishes one.
    pub api: Option<String>,
    /// Env names for a static key. First non-empty wins at runtime.
    pub env: Vec<String>,
    /// Vercel / community npm package, when present.
    pub npm: Option<String>,
}

/// What ingest did for one vendor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestAction {
    /// Wrote `{dir}/{id}.toml`.
    Wrote(PathBuf),
    /// Would write (dry-run).
    DryRun(PathBuf),
    /// Did not write.
    Skipped { vendor: String, reason: String },
}

/// Batch result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    /// Per-vendor outcomes, request order.
    pub actions: Vec<IngestAction>,
}

/// Ingest failure.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// CLI / catalog / vendor problem with a stable message.
    #[error("{0}")]
    Message(String),
    /// Filesystem.
    #[error("{path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
}

/// How to pick vendors and where to write.
#[derive(Debug, Clone)]
pub struct IngestRequest {
    /// Catalog kind (selects parser).
    pub kind: CatalogKind,
    /// Catalog ids. Empty means [`POPULAR_VENDORS`].
    pub vendors: Vec<String>,
    /// Emit every resolvable openai-compat row (not 216 shipped presets).
    pub all_compatible: bool,
    /// Destination directory. None uses the last user overlay dir.
    pub dir: Option<PathBuf>,
    /// Print paths; do not write.
    pub dry_run: bool,
    /// Overwrite an existing file or a shipped id.
    pub force: bool,
}

impl Default for IngestRequest {
    fn default() -> Self {
        Self {
            kind: CatalogKind::ModelsDev,
            vendors: Vec::new(),
            all_compatible: false,
            dir: None,
            dry_run: false,
            force: false,
        }
    }
}

/// Parse catalog JSON (models.dev object or LiteLLM object).
pub fn parse_catalog(kind: CatalogKind, text: &str) -> Result<Vec<CatalogVendor>, IngestError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| IngestError::Message(format!("catalog is not JSON: {e}")))?;
    match kind {
        CatalogKind::ModelsDev => parse_models_dev(&value),
        CatalogKind::LiteLlm => parse_litellm(&value),
    }
}

/// Build profile TOML for one catalog row, or explain why not.
pub fn profile_toml(vendor: &CatalogVendor) -> Result<String, IngestError> {
    let draft = draft_for(vendor)?;
    refuse_bad_id(&vendor.id)?;
    if draft.access_env.is_empty() {
        return Err(vendor_err(&vendor.id, "catalog row has no env key"));
    }
    let display = if vendor.display_name.trim().is_empty() {
        vendor.id.clone()
    } else {
        vendor.display_name.clone()
    };
    let mut out = String::new();
    out.push_str("schema_version = 1\n");
    out.push_str(&format!("id = {}\n", toml_string(&vendor.id)));
    out.push_str(&format!("display_name = {}\n", toml_string(&display)));
    out.push_str(&format!("wire = {}\n", toml_string(draft.wire.toml_name())));
    out.push_str(&format!("base_url = {}\n", toml_string(&draft.base_url)));
    out.push_str(&format!("chat_path = {}\n", toml_string(&draft.chat_path)));
    out.push_str(&format!(
        "auth_scheme = {}\n",
        toml_string(draft.auth_scheme)
    ));
    out.push_str(&format!(
        "access_env = {}\n",
        toml_access_env(&draft.access_env)
    ));
    if let Some(service) = draft.aws_service {
        out.push_str(&format!("aws_service = {}\n", toml_string(service)));
    }
    if let Some(env) = draft.gcp_key_env {
        out.push_str(&format!("gcp_key_env = {}\n", toml_string(env)));
    }
    if let Some(region) = draft.aws_region {
        out.push_str(&format!("aws_region = {}\n", toml_string(&region)));
    }
    if !draft.headers.is_empty() {
        out.push_str("\n[headers]\n");
        for (k, v) in &draft.headers {
            out.push_str(&format!("{k} = {}\n", toml_string(v)));
        }
    }
    if !draft.extra_body.is_empty() {
        out.push_str("\n[fingerprint.extra_body]\n");
        for (k, v) in &draft.extra_body {
            out.push_str(&format!("{k} = {}\n", toml_string(v)));
        }
    }
    Ok(out)
}

struct ProfileDraft {
    wire: EmitWire,
    base_url: String,
    chat_path: String,
    auth_scheme: &'static str,
    access_env: Vec<String>,
    headers: Vec<(String, String)>,
    extra_body: Vec<(String, String)>,
    aws_service: Option<&'static str>,
    aws_region: Option<String>,
    gcp_key_env: Option<&'static str>,
}

fn draft_for(vendor: &CatalogVendor) -> Result<ProfileDraft, IngestError> {
    if let Some(reason) = skip_reason_opt(&vendor.id, vendor.npm.as_deref()) {
        return Err(vendor_err(&vendor.id, reason));
    }
    if vendor.id == "azure" || vendor.id == "azure-cognitive-services" {
        return azure_draft(vendor);
    }
    if vendor.id == "google-vertex" {
        return vertex_gemini_draft(vendor);
    }
    if vendor.id == "google-vertex-anthropic" {
        return vertex_anthropic_draft(vendor);
    }
    if vendor.id == "amazon-bedrock" {
        return Ok(bedrock_draft(vendor));
    }
    let wire = emit_wire(vendor)?;
    let (base_url, chat_path) = endpoint_for(vendor, wire)?;
    let access_env = drop_url_placeholder_env(&vendor.env, &base_url, &chat_path);
    let auth_scheme = if wire == EmitWire::Messages && vendor.api.is_some() {
        "bearer"
    } else {
        wire.auth_scheme()
    };
    Ok(ProfileDraft {
        wire,
        base_url,
        chat_path,
        auth_scheme,
        access_env,
        headers: Vec::new(),
        extra_body: Vec::new(),
        aws_service: None,
        aws_region: None,
        gcp_key_env: None,
    })
}

fn azure_draft(vendor: &CatalogVendor) -> Result<ProfileDraft, IngestError> {
    let resource = vendor
        .env
        .iter()
        .find(|n| n.contains("RESOURCE_NAME"))
        .ok_or_else(|| vendor_err(&vendor.id, "catalog env has no RESOURCE_NAME"))?;
    let key = vendor
        .env
        .iter()
        .find(|n| n.ends_with("API_KEY") || n.ends_with("_KEY"))
        .cloned()
        .unwrap_or_else(|| "AZURE_API_KEY".into());
    Ok(ProfileDraft {
        wire: EmitWire::ChatCompletions,
        base_url: format!("https://{{env:{resource}}}.openai.azure.com"),
        chat_path: "/openai/deployments/{model}/chat/completions?api-version=2024-10-21".into(),
        auth_scheme: "header:api-key",
        access_env: vec![key],
        headers: Vec::new(),
        extra_body: Vec::new(),
        aws_service: None,
        aws_region: None,
        gcp_key_env: None,
    })
}

fn vertex_gemini_draft(vendor: &CatalogVendor) -> Result<ProfileDraft, IngestError> {
    require_vertex_project(vendor)?;
    Ok(ProfileDraft {
        wire: EmitWire::Gemini,
        base_url: "https://{env:GOOGLE_VERTEX_LOCATION}-aiplatform.googleapis.com".into(),
        chat_path: "/v1/projects/{env:GOOGLE_VERTEX_PROJECT}/locations/{env:GOOGLE_VERTEX_LOCATION}/publishers/google/models/{model}:generateContent".into(),
        auth_scheme: "bearer",
        access_env: vertex_access_env(vendor),
        headers: Vec::new(),
        extra_body: Vec::new(),
        aws_service: None,
        aws_region: None,
        gcp_key_env: Some("GOOGLE_APPLICATION_CREDENTIALS"),
    })
}

fn vertex_anthropic_draft(vendor: &CatalogVendor) -> Result<ProfileDraft, IngestError> {
    require_vertex_project(vendor)?;
    Ok(ProfileDraft {
        wire: EmitWire::Messages,
        base_url: "https://{env:GOOGLE_VERTEX_LOCATION}-aiplatform.googleapis.com".into(),
        chat_path: "/v1/projects/{env:GOOGLE_VERTEX_PROJECT}/locations/{env:GOOGLE_VERTEX_LOCATION}/publishers/anthropic/models/{model}:rawPredict".into(),
        auth_scheme: "bearer",
        access_env: vertex_access_env(vendor),
        headers: vec![("anthropic-version".into(), "vertex-2023-10-16".into())],
        extra_body: vec![("anthropic_version".into(), "vertex-2023-10-16".into())],
        aws_service: None,
        aws_region: None,
        gcp_key_env: Some("GOOGLE_APPLICATION_CREDENTIALS"),
    })
}

fn require_vertex_project(vendor: &CatalogVendor) -> Result<(), IngestError> {
    if vendor
        .env
        .iter()
        .any(|n| n == "GOOGLE_VERTEX_PROJECT" || n.contains("PROJECT"))
    {
        Ok(())
    } else {
        Err(vendor_err(
            &vendor.id,
            "catalog env has no GOOGLE_VERTEX_PROJECT",
        ))
    }
}

/// Token names only. Host/account placeholders stay in the URL.
fn drop_url_placeholder_env(env: &[String], base_url: &str, chat_path: &str) -> Vec<String> {
    let haystack = format!("{base_url}{chat_path}");
    env.iter()
        .filter(|name| !haystack.contains(&format!("{{env:{name}}}")))
        .cloned()
        .collect()
}

fn vertex_access_env(vendor: &CatalogVendor) -> Vec<String> {
    let mut env = vec!["GOOGLE_OAUTH_ACCESS_TOKEN".into()];
    for name in &vendor.env {
        if name.contains("API_KEY") || name.contains("TOKEN") {
            env.push(name.clone());
        }
    }
    env
}

fn bedrock_draft(vendor: &CatalogVendor) -> ProfileDraft {
    let mut env = vec!["AWS_BEARER_TOKEN_BEDROCK".into()];
    for name in &vendor.env {
        if name.contains("BEARER") || name.contains("TOKEN") {
            env.push(name.clone());
        }
    }
    env.dedup();
    ProfileDraft {
        wire: EmitWire::Converse,
        base_url: "https://bedrock-runtime.{env:AWS_REGION}.amazonaws.com".into(),
        chat_path: "/model/{model}/converse".into(),
        auth_scheme: "bearer",
        access_env: env,
        headers: Vec::new(),
        extra_body: Vec::new(),
        aws_service: Some("bedrock"),
        aws_region: Some("{env:AWS_REGION}".into()),
        gcp_key_env: None,
    }
}

fn emit_wire(vendor: &CatalogVendor) -> Result<EmitWire, IngestError> {
    match vendor.npm.as_deref() {
        Some("@ai-sdk/anthropic") => Ok(EmitWire::Messages),
        Some("@ai-sdk/google") => Ok(EmitWire::Gemini),
        Some("@ai-sdk/cohere") => Ok(EmitWire::ChatCompletions),
        Some(npm) if openai_compat_npm(npm) => Ok(EmitWire::ChatCompletions),
        Some(npm) => Err(vendor_err(
            &vendor.id,
            &format!("npm package `{npm}` is not a known HTTP dialect"),
        )),
        None => Ok(EmitWire::ChatCompletions),
    }
}

/// Emit profiles from an already-loaded catalog body.
pub fn ingest_catalog(text: &str, req: &IngestRequest) -> Result<IngestReport, IngestError> {
    let rows = parse_catalog(req.kind, text)?;
    let by_id: BTreeMap<String, CatalogVendor> =
        rows.into_iter().map(|v| (v.id.clone(), v)).collect();
    let wanted = wanted_ids(req, &by_id);
    let dir = match &req.dir {
        Some(dir) => dir.clone(),
        None => ingest_profile_dir().ok_or_else(|| {
            IngestError::Message(
                "no user profile dir (set --dir, HOME, or WIREMUX_PROFILE_DIR)".into(),
            )
        })?,
    };
    if !req.dry_run {
        fs::create_dir_all(&dir).map_err(|source| IngestError::Io {
            path: dir.clone(),
            source,
        })?;
    }
    let mut actions = Vec::new();
    let mut hard: Vec<String> = Vec::new();
    for id in wanted {
        match ingest_one(&by_id, &id, &dir, req) {
            Ok(action) => {
                if let IngestAction::Skipped { reason, .. } = &action
                    && req.vendors.iter().any(|v| v == &id)
                    && reason_is_hard(reason)
                {
                    hard.push(format!("{id}: {reason}"));
                }
                actions.push(action);
            }
            Err(err) => hard.push(err.to_string()),
        }
    }
    if !hard.is_empty() {
        return Err(IngestError::Message(hard.join("\n")));
    }
    Ok(IngestReport { actions })
}

/// Fetch an allowlisted catalog URL.
pub async fn fetch_catalog_url(url: &str) -> Result<String, IngestError> {
    if url != MODELS_DEV_URL && url != LITELLM_PROVIDERS_URL {
        return Err(IngestError::Message(
            "refused: ingest only fetches the models.dev or LiteLLM catalog URLs".into(),
        ));
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("wiremux/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| IngestError::Message(format!("http client: {e}")))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| IngestError::Message(format!("fetch {url}: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(IngestError::Message(format!("fetch {url}: HTTP {status}")));
    }
    response
        .text()
        .await
        .map_err(|e| IngestError::Message(format!("fetch {url}: {e}")))
}

fn wanted_ids(req: &IngestRequest, by_id: &BTreeMap<String, CatalogVendor>) -> Vec<String> {
    if req.all_compatible {
        return by_id
            .values()
            .filter(|v| openai_compat_row(v) && profile_toml(v).is_ok() && !skip_id(&v.id))
            .map(|v| v.id.clone())
            .collect();
    }
    if req.vendors.is_empty() {
        return POPULAR_VENDORS.iter().map(|s| (*s).to_string()).collect();
    }
    req.vendors.clone()
}

fn ingest_one(
    by_id: &BTreeMap<String, CatalogVendor>,
    id: &str,
    dir: &Path,
    req: &IngestRequest,
) -> Result<IngestAction, IngestError> {
    if skip_id(id) {
        return Ok(IngestAction::Skipped {
            vendor: id.to_string(),
            reason: skip_reason(id).to_string(),
        });
    }
    let Some(vendor) = by_id.get(id) else {
        return Err(vendor_err(id, "not in catalog"));
    };
    if shipped(id) && !req.force {
        return Ok(IngestAction::Skipped {
            vendor: id.to_string(),
            reason: "already shipped (pass --force to write a user overlay)".into(),
        });
    }
    let toml = profile_toml(vendor)?;
    parse_profile_str(&toml)
        .map_err(|e| vendor_err(id, &format!("emitted TOML failed parse: {e}")))?;
    let path = dir.join(format!("{id}.toml"));
    if path.exists() && !req.force {
        return Ok(IngestAction::Skipped {
            vendor: id.to_string(),
            reason: format!("already exists ({})", path.display()),
        });
    }
    if req.dry_run {
        return Ok(IngestAction::DryRun(path));
    }
    fs::write(&path, toml).map_err(|source| IngestError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(IngestAction::Wrote(path))
}

fn reason_is_hard(reason: &str) -> bool {
    reason.starts_with("not a simple") || reason.starts_with("subscription")
}

fn parse_models_dev(value: &Value) -> Result<Vec<CatalogVendor>, IngestError> {
    let obj = value
        .as_object()
        .ok_or_else(|| IngestError::Message("models.dev catalog must be a JSON object".into()))?;
    let mut out = Vec::new();
    for (key, row) in obj {
        let Some(row) = row.as_object() else {
            continue;
        };
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(key)
            .to_string();
        let display_name = row
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string();
        let api = row
            .get("api")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let env = match row.get("env") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        };
        let npm = row
            .get("npm")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        out.push(CatalogVendor {
            id,
            display_name,
            api,
            env,
            npm,
        });
    }
    Ok(out)
}

fn parse_litellm(value: &Value) -> Result<Vec<CatalogVendor>, IngestError> {
    let obj = value.as_object().ok_or_else(|| {
        IngestError::Message("LiteLLM providers.json must be a JSON object".into())
    })?;
    let mut out = Vec::new();
    for (key, row) in obj {
        let Some(row) = row.as_object() else {
            continue;
        };
        let api = row
            .get("base_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let env = row
            .get("api_key_env")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default();
        out.push(CatalogVendor {
            id: key.clone(),
            display_name: key.clone(),
            api,
            env,
            npm: None,
        });
    }
    Ok(out)
}

fn endpoint_for(vendor: &CatalogVendor, wire: EmitWire) -> Result<(String, String), IngestError> {
    let raw = vendor
        .api
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| well_known_api(&vendor.id, vendor.npm.as_deref()).map(str::to_string))
        .ok_or_else(|| {
            vendor_err(
                &vendor.id,
                "no api URL in catalog and no well-known OpenAI-compat endpoint",
            )
        })?;
    let raw = rewrite_catalog_placeholders(&raw);
    split_openai_compat_api(&raw, wire).map_err(|reason| vendor_err(&vendor.id, &reason))
}

/// Catalog `${VAR}` (and `$VAR`) become `{env:VAR}` so load-time subst matches shipped profiles.
fn rewrite_catalog_placeholders(input: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    let bytes = input.as_bytes();
    while i < input.len() {
        if input[i..].starts_with("{env:")
            && let Some(end) = input[i + 5..].find('}')
        {
            out.push_str(&input[i..i + 5 + end + 1]);
            i += 5 + end + 1;
            continue;
        }
        if input[i..].starts_with("${")
            && let Some(end) = input[i + 2..].find('}')
        {
            let var = &input[i + 2..i + 2 + end];
            if is_env_ident(var) {
                out.push_str("{env:");
                out.push_str(var);
                out.push('}');
                i += 2 + end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn is_env_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn openai_compat_row(vendor: &CatalogVendor) -> bool {
    if vendor.id.starts_with("azure")
        || vendor.id == "amazon-bedrock"
        || vendor.id.starts_with("google-vertex")
    {
        return false;
    }
    match vendor.npm.as_deref() {
        Some(npm) => openai_compat_npm(npm),
        None => true,
    }
}

fn openai_compat_npm(npm: &str) -> bool {
    matches!(
        npm,
        "@ai-sdk/openai-compatible"
            | "@ai-sdk/openai"
            | "@ai-sdk/groq"
            | "@ai-sdk/togetherai"
            | "@ai-sdk/mistral"
            | "@ai-sdk/cerebras"
            | "@ai-sdk/perplexity"
            | "@ai-sdk/xai"
            | "@ai-sdk/deepinfra"
            | "@openrouter/ai-sdk-provider"
            | "@ai-sdk/cohere"
    )
}

/// Official OpenAI-compat roots for Vercel packages that omit `api`.
fn well_known_api(id: &str, npm: Option<&str>) -> Option<&'static str> {
    match (npm, id) {
        (Some("@ai-sdk/groq"), _) | (_, "groq") => Some("https://api.groq.com/openai/v1"),
        (Some("@ai-sdk/togetherai"), _) | (_, "togetherai" | "together") => {
            Some("https://api.together.xyz/v1")
        }
        (Some("@ai-sdk/mistral"), _) | (_, "mistral") => Some("https://api.mistral.ai/v1"),
        (Some("@ai-sdk/cerebras"), _) | (_, "cerebras") => Some("https://api.cerebras.ai/v1"),
        (Some("@ai-sdk/perplexity"), _) | (_, "perplexity") => Some("https://api.perplexity.ai"),
        (Some("@ai-sdk/xai"), _) | (_, "xai") => Some("https://api.x.ai/v1"),
        (Some("@ai-sdk/deepinfra"), _) | (_, "deepinfra") => Some("https://api.deepinfra.com/v1"),
        (Some("@ai-sdk/cohere"), _) | (_, "cohere") => {
            Some("https://api.cohere.ai/compatibility/v1")
        }
        _ => None,
    }
}

/// Catalog dialect we can emit without a new compiled map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmitWire {
    ChatCompletions,
    Messages,
    Gemini,
    Converse,
}

impl EmitWire {
    fn toml_name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat-completions",
            Self::Messages => "messages",
            Self::Gemini => "gemini",
            Self::Converse => "converse",
        }
    }

    fn auth_scheme(self) -> &'static str {
        match self {
            Self::Messages => "x-api-key",
            Self::Gemini => "header:x-goog-api-key",
            Self::ChatCompletions | Self::Converse => "bearer",
        }
    }

    fn chat_suffix(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/chat/completions",
            Self::Messages => "/messages",
            Self::Gemini => "/models/{model}:generateContent",
            Self::Converse => "/converse",
        }
    }
}

fn split_openai_compat_api(raw: &str, wire: EmitWire) -> Result<(String, String), String> {
    let raw = raw.trim().trim_end_matches('/');
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| format!("not an absolute URL: {raw}"))?;
    if scheme != "https" && scheme != "http" {
        return Err(format!(
            "URL scheme must be https (or http on loopback): {raw}"
        ));
    }
    let (host_port, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, String::new()),
    };
    let host = host_port
        .split(':')
        .next()
        .unwrap_or(host_port)
        .split('{')
        .next()
        .unwrap_or(host_port);
    if scheme == "http" && !is_loopback_host(host) && !host.is_empty() {
        return Err("http is only allowed for loopback hosts".into());
    }
    if host_port.is_empty() {
        return Err("URL has no host".into());
    }
    let origin = format!("{scheme}://{host_port}");
    let suffix = if host.eq_ignore_ascii_case("api.perplexity.ai")
        && matches!(wire, EmitWire::ChatCompletions)
        && (path.is_empty() || path == "/")
    {
        "/chat/completions".into()
    } else if path.is_empty() || path == "/" {
        match wire {
            EmitWire::ChatCompletions => "/v1/chat/completions".into(),
            EmitWire::Messages => "/v1/messages".into(),
            EmitWire::Gemini => "/v1beta/models/{model}:generateContent".into(),
            EmitWire::Converse => "/model/{model}/converse".into(),
        }
    } else {
        format!("{path}{}", wire.chat_suffix())
    };
    Ok((origin, suffix))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

fn skip_id(id: &str) -> bool {
    skip_reason_opt(id, None).is_some()
}

fn skip_reason(id: &str) -> &'static str {
    skip_reason_opt(id, None).unwrap_or("skipped")
}

fn skip_reason_opt(id: &str, npm: Option<&str>) -> Option<&'static str> {
    if id == "github-copilot" || id.contains("copilot") {
        return Some("subscription / product-terms host; no shipped Copilot preset");
    }
    let _ = npm;
    None
}

fn ingest_profile_dir() -> Option<PathBuf> {
    user_overlay_dirs().into_iter().next_back()
}

fn user_overlay_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        dirs.push(PathBuf::from(xdg).join("wiremux").join("profiles"));
    } else if let Some(home) = home_dir() {
        dirs.push(home.join(".config").join("wiremux").join("profiles"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = home_dir() {
        dirs.push(
            home.join("Library")
                .join("Application Support")
                .join("wiremux")
                .join("profiles"),
        );
    }
    if let Ok(extra) = std::env::var("WIREMUX_PROFILE_DIR") {
        for part in std::env::split_paths(&extra) {
            if !part.as_os_str().is_empty() {
                dirs.push(part);
            }
        }
    }
    dirs
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn shipped(id: &str) -> bool {
    // `list_profiles` exists on crates.io 0.4.0. `shipped_profile_ids`
    // does not; package verify builds this crate against that release.
    let opts = LoadOptions {
        include_user_config: false,
        include_shipped: true,
        ..LoadOptions::default()
    };
    list_profiles(&opts)
        .ok()
        .is_some_and(|ids| ids.iter().any(|s| s == id))
}

fn refuse_bad_id(id: &str) -> Result<(), IngestError> {
    if id.is_empty() || id.len() > 64 {
        return Err(vendor_err(id, "id must be 1..=64 characters"));
    }
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return Err(vendor_err(id, "empty id"));
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(vendor_err(id, "id must start with [a-z0-9]"));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(vendor_err(id, "id must match [a-z0-9][a-z0-9-]*"));
    }
    Ok(())
}

fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn toml_access_env(env: &[String]) -> String {
    if env.len() == 1 {
        return toml_string(&env[0]);
    }
    let parts: Vec<String> = env.iter().map(|s| toml_string(s)).collect();
    format!("[{}]", parts.join(", "))
}

fn vendor_err(id: &str, reason: &str) -> IngestError {
    IngestError::Message(format!("{id}: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS_DEV_FIXTURE: &str = r#"
    {
      "groq": {
        "id": "groq",
        "name": "Groq",
        "env": ["GROQ_API_KEY"],
        "npm": "@ai-sdk/groq",
        "doc": "https://console.groq.com/docs/models"
      },
      "deepseek": {
        "id": "deepseek",
        "name": "DeepSeek",
        "env": ["DEEPSEEK_API_KEY"],
        "npm": "@ai-sdk/openai-compatible",
        "api": "https://api.deepseek.com"
      },
      "togetherai": {
        "id": "togetherai",
        "name": "Together AI",
        "env": ["TOGETHER_API_KEY"],
        "npm": "@ai-sdk/togetherai"
      },
      "fireworks-ai": {
        "id": "fireworks-ai",
        "name": "Fireworks AI",
        "env": ["FIREWORKS_API_KEY"],
        "npm": "@ai-sdk/openai-compatible",
        "api": "https://api.fireworks.ai/inference/v1/"
      },
      "mistral": {
        "id": "mistral",
        "name": "Mistral",
        "env": ["MISTRAL_API_KEY"],
        "npm": "@ai-sdk/mistral"
      },
      "cerebras": {
        "id": "cerebras",
        "name": "Cerebras",
        "env": ["CEREBRAS_API_KEY"],
        "npm": "@ai-sdk/cerebras"
      },
      "azure": {
        "id": "azure",
        "name": "Azure",
        "env": ["AZURE_RESOURCE_NAME", "AZURE_API_KEY"],
        "npm": "@ai-sdk/azure"
      },
      "minimax": {
        "id": "minimax",
        "name": "MiniMax",
        "env": ["MINIMAX_API_KEY"],
        "npm": "@ai-sdk/anthropic",
        "api": "https://api.minimax.io/anthropic/v1"
      },
      "kimi-for-coding": {
        "id": "kimi-for-coding",
        "name": "Kimi for coding",
        "env": ["KIMI_API_KEY"],
        "npm": "@ai-sdk/anthropic",
        "api": "https://api.kimi.com/coding/v1"
      },
      "cloudflare-workers-ai": {
        "id": "cloudflare-workers-ai",
        "name": "Cloudflare Workers AI",
        "env": ["CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_API_KEY"],
        "npm": "@ai-sdk/openai-compatible",
        "api": "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/ai/v1"
      },
      "databricks": {
        "id": "databricks",
        "name": "Databricks",
        "env": ["DATABRICKS_HOST", "DATABRICKS_TOKEN"],
        "npm": "@ai-sdk/openai-compatible",
        "api": "https://${DATABRICKS_HOST}/ai-gateway/mlflow/v1"
      },
      "google-vertex": {
        "id": "google-vertex",
        "name": "Vertex",
        "env": ["GOOGLE_VERTEX_PROJECT", "GOOGLE_VERTEX_LOCATION"],
        "npm": "@ai-sdk/google-vertex"
      },
      "google-vertex-anthropic": {
        "id": "google-vertex-anthropic",
        "name": "Vertex Anthropic",
        "env": ["GOOGLE_VERTEX_PROJECT", "GOOGLE_VERTEX_LOCATION"],
        "npm": "@ai-sdk/google-vertex/anthropic"
      },
      "amazon-bedrock": {
        "id": "amazon-bedrock",
        "name": "Amazon Bedrock",
        "env": ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_REGION", "AWS_BEARER_TOKEN_BEDROCK"],
        "npm": "@ai-sdk/amazon-bedrock"
      },
      "cohere": {
        "id": "cohere",
        "name": "Cohere",
        "env": ["COHERE_API_KEY"],
        "npm": "@ai-sdk/cohere"
      },
      "openai": {
        "id": "openai",
        "name": "OpenAI",
        "env": ["OPENAI_API_KEY"],
        "npm": "@ai-sdk/openai"
      },
      "github-copilot": {
        "id": "github-copilot",
        "name": "GitHub Copilot",
        "env": ["GITHUB_TOKEN"],
        "npm": "@ai-sdk/openai-compatible",
        "api": "https://api.githubcopilot.com"
      }
    }
    "#;

    const LITELLM_FIXTURE: &str = r#"
    {
      "publicai": {
        "base_url": "https://api.publicai.co/v1",
        "api_key_env": "PUBLICAI_API_KEY"
      }
    }
    "#;

    #[test]
    fn groq_uses_vercel_default_url() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let groq = rows.iter().find(|v| v.id == "groq").unwrap();
        let toml = profile_toml(groq).unwrap();
        assert!(toml.contains("id = \"groq\""));
        assert!(toml.contains("base_url = \"https://api.groq.com\""));
        assert!(toml.contains("chat_path = \"/openai/v1/chat/completions\""));
        assert!(toml.contains("access_env = \"GROQ_API_KEY\""));
        parse_profile_str(&toml).unwrap();
    }

    #[test]
    fn deepseek_splits_catalog_api() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let v = rows.iter().find(|r| r.id == "deepseek").unwrap();
        let toml = profile_toml(v).unwrap();
        assert!(toml.contains("base_url = \"https://api.deepseek.com\""));
        assert!(toml.contains("chat_path = \"/v1/chat/completions\""));
    }

    #[test]
    fn fireworks_keeps_inference_prefix() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let v = rows.iter().find(|r| r.id == "fireworks-ai").unwrap();
        let toml = profile_toml(v).unwrap();
        assert!(toml.contains("base_url = \"https://api.fireworks.ai\""));
        assert!(toml.contains("chat_path = \"/inference/v1/chat/completions\""));
    }

    #[test]
    fn copilot_still_fail_closed() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let copilot = rows.iter().find(|r| r.id == "github-copilot").unwrap();
        let err = profile_toml(copilot).unwrap_err().to_string();
        assert!(
            err.contains("product-terms") || err.contains("Copilot"),
            "{err}"
        );
    }

    #[test]
    fn minimax_and_kimi_emit_messages_wire() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let mini = rows.iter().find(|r| r.id == "minimax").unwrap();
        let toml = profile_toml(mini).unwrap();
        assert!(toml.contains("wire = \"messages\""), "{toml}");
        assert!(toml.contains("base_url = \"https://api.minimax.io\""));
        assert!(toml.contains("chat_path = \"/anthropic/v1/messages\""));
        assert!(toml.contains("auth_scheme = \"bearer\""));
        parse_profile_str(&toml).unwrap();

        let kimi = rows.iter().find(|r| r.id == "kimi-for-coding").unwrap();
        let toml = profile_toml(kimi).unwrap();
        assert!(toml.contains("wire = \"messages\""));
        assert!(toml.contains("https://api.kimi.com"));
        assert!(toml.contains("/coding/v1/messages"));
        parse_profile_str(&toml).unwrap();
    }

    #[test]
    fn catalog_dollar_env_becomes_env_placeholder() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let cf = rows
            .iter()
            .find(|r| r.id == "cloudflare-workers-ai")
            .unwrap();
        let toml = profile_toml(cf).unwrap();
        assert!(
            toml.contains("/accounts/{env:CLOUDFLARE_ACCOUNT_ID}/ai/v1/chat/completions"),
            "{toml}"
        );
        assert!(!toml.contains("${CLOUDFLARE_ACCOUNT_ID}"), "{toml}");
        parse_profile_str(&toml).unwrap();

        let db = rows.iter().find(|r| r.id == "databricks").unwrap();
        let toml = profile_toml(db).unwrap();
        assert!(
            toml.contains("base_url = \"https://{env:DATABRICKS_HOST}\""),
            "{toml}"
        );
        assert!(toml.contains("/ai-gateway/mlflow/v1/chat/completions"));
        parse_profile_str(&toml).unwrap();
    }

    #[test]
    fn azure_emits_deployment_path_template() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let azure = rows.iter().find(|r| r.id == "azure").unwrap();
        let toml = profile_toml(azure).unwrap();
        assert!(toml.contains("https://{env:AZURE_RESOURCE_NAME}.openai.azure.com"));
        assert!(toml.contains("/openai/deployments/{model}/chat/completions?api-version="));
        assert!(toml.contains("auth_scheme = \"header:api-key\""));
        parse_profile_str(&toml).unwrap();
    }

    #[test]
    fn vertex_and_bedrock_and_cohere_emit() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let vertex = rows.iter().find(|r| r.id == "google-vertex").unwrap();
        let toml = profile_toml(vertex).unwrap();
        assert!(toml.contains("wire = \"gemini\""), "{toml}");
        assert!(toml.contains("gcp_key_env = \"GOOGLE_APPLICATION_CREDENTIALS\""));
        assert!(toml.contains("{env:GOOGLE_VERTEX_PROJECT}"));
        assert!(toml.contains(":generateContent"));
        parse_profile_str(&toml).unwrap();

        let vanth = rows
            .iter()
            .find(|r| r.id == "google-vertex-anthropic")
            .unwrap();
        let toml = profile_toml(vanth).unwrap();
        assert!(toml.contains("wire = \"messages\""));
        assert!(toml.contains(":rawPredict"));
        assert!(toml.contains("anthropic-version"));
        assert!(toml.contains("anthropic_version = \"vertex-2023-10-16\""));
        parse_profile_str(&toml).unwrap();

        let bedrock = rows.iter().find(|r| r.id == "amazon-bedrock").unwrap();
        let toml = profile_toml(bedrock).unwrap();
        assert!(toml.contains("wire = \"converse\""), "{toml}");
        assert!(toml.contains("/model/{model}/converse"));
        assert!(toml.contains("aws_service = \"bedrock\""));
        parse_profile_str(&toml).unwrap();

        let cohere = rows.iter().find(|r| r.id == "cohere").unwrap();
        let toml = profile_toml(cohere).unwrap();
        assert!(toml.contains("https://api.cohere.ai"));
        assert!(toml.contains("/compatibility/v1/chat/completions"));
        parse_profile_str(&toml).unwrap();
    }

    #[test]
    fn popular_write_skips_shipped_openai() {
        let dir = tempfile::tempdir().unwrap();
        let report = ingest_catalog(
            MODELS_DEV_FIXTURE,
            &IngestRequest {
                dir: Some(dir.path().to_path_buf()),
                ..IngestRequest::default()
            },
        )
        .unwrap();
        let wrote: Vec<String> = report
            .actions
            .iter()
            .filter_map(|a| match a {
                IngestAction::Wrote(p) => p.file_stem()?.to_str().map(str::to_string),
                _ => None,
            })
            .collect();
        let expected: Vec<&str> = POPULAR_VENDORS
            .iter()
            .copied()
            .filter(|id| !shipped(id))
            .collect();
        assert_eq!(wrote, expected);
        for id in &expected {
            let path = dir.path().join(format!("{id}.toml"));
            assert!(path.is_file(), "missing {id}");
            parse_profile_str(&fs::read_to_string(path).unwrap()).unwrap();
        }
        for id in POPULAR_VENDORS.iter().copied().filter(|id| shipped(id)) {
            assert!(
                !dir.path().join(format!("{id}.toml")).exists(),
                "shipped {id} must not be rewritten"
            );
        }
        assert!(!dir.path().join("openai.toml").exists());
        assert!(!dir.path().join("azure.toml").exists());
    }

    #[test]
    fn shipped_id_absent_from_catalog_is_not_in_catalog() {
        assert!(
            shipped("groq"),
            "test needs a shipped id missing from LITELLM_FIXTURE"
        );
        let dir = tempfile::tempdir().unwrap();
        let err = ingest_catalog(
            LITELLM_FIXTURE,
            &IngestRequest {
                kind: CatalogKind::LiteLlm,
                vendors: vec!["groq".into()],
                dir: Some(dir.path().to_path_buf()),
                dry_run: true,
                ..IngestRequest::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("groq"), "{err}");
        assert!(err.contains("not in catalog"), "{err}");
        assert!(
            !err.contains("already shipped"),
            "catalog miss must win over shipped skip, got {err}"
        );
    }

    #[test]
    fn requested_unknown_vendor_is_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = ingest_catalog(
            MODELS_DEV_FIXTURE,
            &IngestRequest {
                vendors: vec!["no-such-vendor".into()],
                dir: Some(dir.path().to_path_buf()),
                ..IngestRequest::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no-such-vendor"), "{err}");
        assert!(err.contains("not in catalog"), "{err}");
    }

    #[test]
    fn refuse_http_non_loopback() {
        let v = CatalogVendor {
            id: "evil".into(),
            display_name: "evil".into(),
            api: Some("http://example.invalid/v1".into()),
            env: vec!["EVIL_KEY".into()],
            npm: Some("@ai-sdk/openai-compatible".into()),
        };
        let err = profile_toml(&v).unwrap_err().to_string();
        assert!(err.contains("loopback"), "{err}");
    }

    #[test]
    fn refuse_unknown_fetch_url() {
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fetch_catalog_url("https://example.invalid/catalog.json"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("refused"), "{err}");
    }

    #[test]
    fn litellm_long_tail_row() {
        let rows = parse_catalog(CatalogKind::LiteLlm, LITELLM_FIXTURE).unwrap();
        assert_eq!(rows.len(), 1);
        let toml = profile_toml(&rows[0]).unwrap();
        assert!(toml.contains("id = \"publicai\""));
        assert!(toml.contains("base_url = \"https://api.publicai.co\""));
        assert!(toml.contains("chat_path = \"/v1/chat/completions\""));
        assert!(toml.contains("access_env = \"PUBLICAI_API_KEY\""));
    }

    #[test]
    fn existing_file_skipped_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("togetherai.toml");
        fs::write(&path, "stale").unwrap();
        let report = ingest_catalog(
            MODELS_DEV_FIXTURE,
            &IngestRequest {
                vendors: vec!["togetherai".into()],
                dir: Some(dir.path().to_path_buf()),
                ..IngestRequest::default()
            },
        )
        .unwrap();
        match &report.actions[0] {
            IngestAction::Skipped { reason, .. } => {
                assert!(reason.contains("already exists"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read_to_string(path).unwrap(), "stale");
    }

    #[test]
    fn all_compatible_writes_openai_compat_only() {
        let dir = tempfile::tempdir().unwrap();
        let report = ingest_catalog(
            MODELS_DEV_FIXTURE,
            &IngestRequest {
                all_compatible: true,
                dir: Some(dir.path().to_path_buf()),
                ..IngestRequest::default()
            },
        )
        .unwrap();
        let wrote: Vec<String> = report
            .actions
            .iter()
            .filter_map(|a| match a {
                IngestAction::Wrote(p) => p.file_stem()?.to_str().map(str::to_string),
                _ => None,
            })
            .collect();
        assert!(
            wrote.contains(&"togetherai".into()),
            "togetherai missing: {wrote:?}"
        );
        assert!(dir.path().join("togetherai.toml").is_file());
        assert!(
            !wrote.contains(&"groq".into()),
            "shipped groq must be skipped: {wrote:?}"
        );
        assert!(!dir.path().join("azure.toml").exists());
        assert!(!dir.path().join("amazon-bedrock.toml").exists());
        assert!(!dir.path().join("google-vertex.toml").exists());
        assert!(!dir.path().join("google-vertex-anthropic.toml").exists());
        assert!(!wrote.iter().any(|id| id.starts_with("azure")));
        assert!(!wrote.iter().any(|id| id == "amazon-bedrock"));
        assert!(!wrote.iter().any(|id| id.starts_with("google-vertex")));
    }

    #[test]
    fn explicit_vendor_azure_still_writes() {
        let dir = tempfile::tempdir().unwrap();
        ingest_catalog(
            MODELS_DEV_FIXTURE,
            &IngestRequest {
                vendors: vec!["azure".into()],
                dir: Some(dir.path().to_path_buf()),
                ..IngestRequest::default()
            },
        )
        .unwrap();
        assert!(dir.path().join("azure.toml").is_file());
    }

    #[test]
    fn databricks_access_env_is_token_only() {
        let rows = parse_catalog(CatalogKind::ModelsDev, MODELS_DEV_FIXTURE).unwrap();
        let db = rows.iter().find(|r| r.id == "databricks").unwrap();
        let toml = profile_toml(db).unwrap();
        let access = toml
            .lines()
            .find(|l| l.starts_with("access_env"))
            .expect("access_env");
        assert!(access.contains("DATABRICKS_TOKEN"), "{access}");
        assert!(
            !access.contains("DATABRICKS_HOST"),
            "host belongs in the URL, not access_env: {access}"
        );
        parse_profile_str(&toml).unwrap();
    }

    #[test]
    fn azure_without_resource_name_fails() {
        let v = CatalogVendor {
            id: "azure".into(),
            display_name: "Azure".into(),
            api: None,
            env: vec!["AZURE_API_KEY".into()],
            npm: Some("@ai-sdk/azure".into()),
        };
        let err = profile_toml(&v).unwrap_err().to_string();
        assert!(err.contains("RESOURCE_NAME"), "{err}");
    }

    #[test]
    fn vertex_without_project_fails() {
        let v = CatalogVendor {
            id: "google-vertex".into(),
            display_name: "Vertex".into(),
            api: None,
            env: vec!["GOOGLE_VERTEX_LOCATION".into()],
            npm: Some("@ai-sdk/google-vertex".into()),
        };
        let err = profile_toml(&v).unwrap_err().to_string();
        assert!(
            err.contains("GOOGLE_VERTEX_PROJECT") || err.contains("PROJECT"),
            "{err}"
        );
    }
}

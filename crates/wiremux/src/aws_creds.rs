//! Default AWS credential chain for SigV4 (client / proxy only).
//!
//! Order: env keys, then `AWS_PROFILE` / `~/.aws` (static keys,
//! `credential_process`, SSO token cache), then ECS, then IMDS.
//! Temporary credentials are cached until 80% of their TTL.
//! Maps-only builds do not compile this module.

use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde_json::Value;
use tokio::time::timeout;
use wiremux_auth::{AwsCredentials, ResolvedProfile};

use crate::aws_sign::AwsSignError;

const DEFAULT_REGION: &str = "us-east-1";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const METADATA_CONNECT: Duration = Duration::from_millis(200);
const METADATA_TOTAL: Duration = Duration::from_secs(2);
const SSO_CONNECT: Duration = Duration::from_secs(2);
const SSO_TOTAL: Duration = Duration::from_secs(10);

struct CachedCreds {
    creds: AwsCredentials,
    acquired_at: Instant,
    /// `None` means reuse until process exit (no `Expiration` in the document).
    lifetime: Option<Duration>,
}

impl CachedCreds {
    fn fresh(&self) -> bool {
        if self
            .creds
            .expiration
            .is_some_and(|exp| SystemTime::now() >= exp)
        {
            return false;
        }
        match self.lifetime {
            None => true,
            Some(life) if life.is_zero() => false,
            Some(life) => self.acquired_at.elapsed() < life.mul_f64(0.8),
        }
    }
}

fn cache() -> std::sync::MutexGuard<'static, HashMap<String, CachedCreds>> {
    static CACHE: std::sync::LazyLock<Mutex<HashMap<String, CachedCreds>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

static IMDS_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn clear_aws_credential_cache() {
    cache().clear();
    IMDS_UNAVAILABLE.store(false, Ordering::SeqCst);
}

fn cache_get(key: &str) -> Option<AwsCredentials> {
    cache()
        .get(key)
        .filter(|c| c.fresh())
        .map(|c| c.creds.clone())
}

fn cache_put(key: String, creds: AwsCredentials) {
    let lifetime = match creds.expiration {
        None => None,
        Some(exp) => match exp.duration_since(SystemTime::now()) {
            Ok(life) if !life.is_zero() => Some(life),
            _ => return,
        },
    };
    cache().insert(
        key,
        CachedCreds {
            creds,
            acquired_at: Instant::now(),
            lifetime,
        },
    );
}

fn auth_err(message: impl Into<String>) -> AwsSignError {
    AwsSignError::Auth(message.into())
}

/// Env keys first, then shared profile, `credential_process`, SSO, ECS, IMDS.
pub(crate) async fn resolve_aws_credentials() -> Result<Option<AwsCredentials>, AwsSignError> {
    if let Some(creds) = creds_from_env() {
        return Ok(Some(creds));
    }
    if let Some(creds) = creds_from_shared_profile().await? {
        return Ok(Some(creds));
    }
    if let Some(creds) = creds_from_ecs().await? {
        return Ok(Some(creds));
    }
    creds_from_imds().await
}

fn creds_from_env() -> Option<AwsCredentials> {
    let access = env_nonempty("AWS_ACCESS_KEY_ID")?;
    let secret = env_nonempty("AWS_SECRET_ACCESS_KEY")?;
    Some(AwsCredentials {
        access_key_id: access,
        secret_access_key: secret,
        session_token: env_nonempty("AWS_SESSION_TOKEN").unwrap_or_default(),
        expiration: None,
    })
}

async fn creds_from_shared_profile() -> Result<Option<AwsCredentials>, AwsSignError> {
    let Some(section) = merged_profile_section()? else {
        return Ok(None);
    };
    if let Some(creds) = static_keys_from_section(&section) {
        return Ok(Some(creds));
    }
    if let Some(cmd) = section
        .get("credential_process")
        .cloned()
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(run_credential_process(&cmd).await?));
    }
    if section.contains_key("sso_start_url")
        || section.contains_key("sso_session")
        || section.contains_key("sso_account_id")
    {
        return Ok(Some(creds_from_sso(&section).await?));
    }
    Ok(None)
}

fn static_keys_from_section(section: &BTreeMap<String, String>) -> Option<AwsCredentials> {
    let access = section
        .get("aws_access_key_id")
        .cloned()
        .filter(|s| !s.is_empty())?;
    let secret = section
        .get("aws_secret_access_key")
        .cloned()
        .filter(|s| !s.is_empty())?;
    Some(AwsCredentials {
        access_key_id: access,
        secret_access_key: secret,
        session_token: section
            .get("aws_session_token")
            .cloned()
            .unwrap_or_default(),
        expiration: None,
    })
}

async fn run_credential_process(cmd: &str) -> Result<AwsCredentials, AwsSignError> {
    let cache_key = format!("process:{}", aws_profile_name());
    if let Some(cached) = cache_get(&cache_key) {
        return Ok(cached);
    }
    let argv = split_command(cmd)?;
    let (exe, args) = argv
        .split_first()
        .ok_or_else(|| auth_err("credential_process is empty"))?;
    let child = tokio::process::Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| auth_err(format!("credential_process spawn `{exe}`: {err}")))?;
    let output = match timeout(PROCESS_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(auth_err(format!("credential_process wait: {err}"))),
        Err(_) => return Err(auth_err("credential_process timed out after 10s")),
    };
    if !output.status.success() {
        return Err(auth_err(format!(
            "credential_process exited {}",
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        )));
    }
    let creds = creds_from_process_json(&output.stdout)?;
    cache_put(cache_key, creds.clone());
    Ok(creds)
}

fn creds_from_process_json(bytes: &[u8]) -> Result<AwsCredentials, AwsSignError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|err| auth_err(format!("credential_process JSON: {err}")))?;
    if let Some(version) = value.get("Version").and_then(Value::as_u64)
        && version != 1
    {
        return Err(auth_err(format!(
            "credential_process Version {version} is not supported"
        )));
    }
    creds_from_json_object(&value)
        .ok_or_else(|| auth_err("credential_process JSON missing AccessKeyId or SecretAccessKey"))
}

async fn creds_from_sso(
    section: &BTreeMap<String, String>,
) -> Result<AwsCredentials, AwsSignError> {
    let (start_url, sso_region, account_id, role_name) = sso_fields(section)?;
    let cache_key = format!("sso:{account_id}:{role_name}:{start_url}");
    if let Some(cached) = cache_get(&cache_key) {
        return Ok(cached);
    }
    let token = sso_access_token(&start_url)?;
    let portal = sso_portal_base(&sso_region);
    let url = format!(
        "{portal}/federation/credentials?account_id={}&role_name={}",
        urlencoding_query(&account_id),
        urlencoding_query(&role_name)
    );
    let (status, body) = http_get(
        &url,
        &[("x-amz-sso_bearer_token", token.as_str())],
        SSO_CONNECT,
        SSO_TOTAL,
        false,
    )
    .await?;
    if !(200..300).contains(&status) {
        return Err(auth_err(format!(
            "SSO GetRoleCredentials HTTP {status} for account {account_id} role {role_name}"
        )));
    }
    let value: Value = serde_json::from_str(&body)
        .map_err(|err| auth_err(format!("SSO GetRoleCredentials JSON: {err}")))?;
    let creds = value
        .get("roleCredentials")
        .and_then(creds_from_json_object)
        .ok_or_else(|| auth_err("SSO GetRoleCredentials missing roleCredentials"))?;
    cache_put(cache_key, creds.clone());
    Ok(creds)
}

fn sso_fields(
    section: &BTreeMap<String, String>,
) -> Result<(String, String, String, String), AwsSignError> {
    let mut start_url = section.get("sso_start_url").cloned().unwrap_or_default();
    let mut sso_region = section.get("sso_region").cloned().unwrap_or_default();
    if let Some(session) = section.get("sso_session").filter(|s| !s.is_empty()) {
        let config_text = read_optional(&config_path()?)?.unwrap_or_default();
        if let Some(sess) = ini_section(&config_text, &format!("sso-session {session}")) {
            if start_url.is_empty() {
                start_url = sess.get("sso_start_url").cloned().unwrap_or_default();
            }
            if sso_region.is_empty() {
                sso_region = sess.get("sso_region").cloned().unwrap_or_default();
            }
        }
    }
    let account_id = section
        .get("sso_account_id")
        .cloned()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| auth_err("SSO profile is missing sso_account_id"))?;
    let role_name = section
        .get("sso_role_name")
        .cloned()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| auth_err("SSO profile is missing sso_role_name"))?;
    if start_url.is_empty() {
        return Err(auth_err("SSO profile is missing sso_start_url"));
    }
    if sso_region.is_empty() {
        return Err(auth_err("SSO profile is missing sso_region"));
    }
    Ok((start_url, sso_region, account_id, role_name))
}

fn sso_access_token(start_url: &str) -> Result<String, AwsSignError> {
    let dir = sso_cache_dir()?;
    let mut best: Option<(SystemTime, String)> = None;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(auth_err(format!(
                "SSO token cache missing for {start_url}; run aws sso login"
            )));
        }
        Err(err) => {
            return Err(auth_err(format!("read {}: {err}", dir.display())));
        }
    };
    for entry in entries {
        let path = match entry {
            Ok(e) => e.path(),
            Err(_) => continue,
        };
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get("startUrl").and_then(Value::as_str) != Some(start_url) {
            continue;
        }
        let Some(token) = value
            .get("accessToken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let exp = value
            .get("expiresAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        if exp.is_some_and(|t| SystemTime::now() >= t) {
            continue;
        }
        let rank = exp.unwrap_or(UNIX_EPOCH);
        if best.as_ref().is_none_or(|(prev, _)| rank > *prev) {
            best = Some((rank, token.to_string()));
        }
    }
    best.map(|(_, token)| token).ok_or_else(|| {
        auth_err(format!(
            "SSO token cache missing or expired for {start_url}; run aws sso login"
        ))
    })
}

fn sso_cache_dir() -> Result<PathBuf, AwsSignError> {
    if let Some(path) = env_nonempty("AWS_SSO_CACHE_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(aws_home()?.join(".aws").join("sso").join("cache"))
}

fn sso_portal_base(region: &str) -> String {
    env_nonempty("AWS_ENDPOINT_URL_SSO")
        .or_else(|| env_nonempty("AWS_ENDPOINT_URL"))
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("https://portal.sso.{region}.amazonaws.com"))
}

async fn creds_from_ecs() -> Result<Option<AwsCredentials>, AwsSignError> {
    let uri = if let Some(full) = env_nonempty("AWS_CONTAINER_CREDENTIALS_FULL_URI") {
        let parsed = Url::parse(&full)
            .map_err(|err| auth_err(format!("AWS_CONTAINER_CREDENTIALS_FULL_URI: {err}")))?;
        if !ecs_full_uri_allowed(&parsed) {
            return Err(auth_err(
                "AWS_CONTAINER_CREDENTIALS_FULL_URI must be loopback, 169.254.170.2, or 169.254.170.23",
            ));
        }
        full
    } else if let Some(rel) = env_nonempty("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI") {
        if !rel.starts_with('/') {
            return Err(auth_err(
                "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI must start with /",
            ));
        }
        format!("http://169.254.170.2{rel}")
    } else {
        return Ok(None);
    };
    let cache_key = format!("ecs:{uri}");
    if let Some(cached) = cache_get(&cache_key) {
        return Ok(Some(cached));
    }
    let token = ecs_auth_token()?;
    let mut headers = Vec::new();
    if let Some(token) = token.as_deref() {
        headers.push(("authorization", token));
    }
    let header_refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, *v)).collect();
    let (status, body) =
        http_get(&uri, &header_refs, METADATA_CONNECT, METADATA_TOTAL, true).await?;
    if !(200..300).contains(&status) {
        return Err(auth_err(format!("ECS credentials HTTP {status}")));
    }
    let value: Value = serde_json::from_str(&body)
        .map_err(|err| auth_err(format!("ECS credentials JSON: {err}")))?;
    let creds = creds_from_json_object(&value)
        .ok_or_else(|| auth_err("ECS credentials JSON missing AccessKeyId or SecretAccessKey"))?;
    cache_put(cache_key, creds.clone());
    Ok(Some(creds))
}

fn ecs_full_uri_allowed(url: &Url) -> bool {
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            ip.is_loopback()
                || ip == Ipv4Addr::new(169, 254, 170, 2)
                || ip == Ipv4Addr::new(169, 254, 170, 23)
        }
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}

fn ecs_auth_token() -> Result<Option<String>, AwsSignError> {
    if let Some(token) = env_nonempty("AWS_CONTAINER_AUTHORIZATION_TOKEN") {
        return Ok(Some(token));
    }
    let Some(path) = env_nonempty("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE") else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path).map_err(|err| {
        auth_err(format!(
            "read AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE: {err}"
        ))
    })?;
    Ok(Some(text.trim().to_string()).filter(|s| !s.is_empty()))
}

async fn creds_from_imds() -> Result<Option<AwsCredentials>, AwsSignError> {
    if env_flag("AWS_EC2_METADATA_DISABLED") || IMDS_UNAVAILABLE.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let cache_key = "imds".to_string();
    if let Some(cached) = cache_get(&cache_key) {
        return Ok(Some(cached));
    }
    let base = env_nonempty("AWS_EC2_METADATA_SERVICE_ENDPOINT")
        .unwrap_or_else(|| "http://169.254.169.254".into())
        .trim_end_matches('/')
        .to_string();
    let token = match imds_session_token(&base).await {
        Ok(token) => Some(token),
        Err(_) => {
            IMDS_UNAVAILABLE.store(true, Ordering::SeqCst);
            return Ok(None);
        }
    };
    let mut headers = Vec::new();
    if let Some(token) = token.as_deref() {
        headers.push(("x-aws-ec2-metadata-token", token));
    }
    let header_refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, *v)).collect();
    let list_url = format!("{base}/latest/meta-data/iam/security-credentials/");
    let (status, role_body) = match http_get(
        &list_url,
        &header_refs,
        METADATA_CONNECT,
        METADATA_TOTAL,
        true,
    )
    .await
    {
        Ok(pair) => pair,
        Err(_) => {
            IMDS_UNAVAILABLE.store(true, Ordering::SeqCst);
            return Ok(None);
        }
    };
    if !(200..300).contains(&status) {
        IMDS_UNAVAILABLE.store(true, Ordering::SeqCst);
        return Ok(None);
    }
    let role = role_body
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .ok_or_else(|| auth_err("IMDS listed no IAM role"))?;
    let cred_url = format!("{base}/latest/meta-data/iam/security-credentials/{role}");
    let (status, body) = http_get(
        &cred_url,
        &header_refs,
        METADATA_CONNECT,
        METADATA_TOTAL,
        true,
    )
    .await?;
    if !(200..300).contains(&status) {
        return Err(auth_err(format!("IMDS credentials HTTP {status}")));
    }
    let value: Value = serde_json::from_str(&body)
        .map_err(|err| auth_err(format!("IMDS credentials JSON: {err}")))?;
    if value
        .get("Code")
        .and_then(Value::as_str)
        .is_some_and(|c| c != "Success")
    {
        return Err(auth_err(format!(
            "IMDS credentials Code={}",
            value.get("Code").and_then(Value::as_str).unwrap_or("?")
        )));
    }
    let creds = creds_from_json_object(&value)
        .ok_or_else(|| auth_err("IMDS credentials JSON missing AccessKeyId or SecretAccessKey"))?;
    cache_put(cache_key, creds.clone());
    Ok(Some(creds))
}

async fn imds_session_token(base: &str) -> Result<String, AwsSignError> {
    let url = format!("{base}/latest/api/token");
    let (status, body) = http_put(
        &url,
        &[("x-aws-ec2-metadata-token-ttl-seconds", "21600")],
        METADATA_CONNECT,
        METADATA_TOTAL,
        true,
    )
    .await?;
    if !(200..300).contains(&status) || body.trim().is_empty() {
        return Err(auth_err(format!("IMDSv2 token HTTP {status}")));
    }
    Ok(body.trim().to_string())
}

fn creds_from_json_object(value: &Value) -> Option<AwsCredentials> {
    let access = json_str(value, &["AccessKeyId", "accessKeyId", "access_key_id"])?;
    let secret = json_str(
        value,
        &["SecretAccessKey", "secretAccessKey", "secret_access_key"],
    )?;
    if access.is_empty() || secret.is_empty() {
        return None;
    }
    let session_token = json_str(
        value,
        &["Token", "SessionToken", "sessionToken", "session_token"],
    )
    .unwrap_or_default();
    let expiration = json_str(value, &["Expiration", "expiration"])
        .and_then(|s| parse_rfc3339(&s))
        .or_else(|| {
            ["Expiration", "expiration"]
                .iter()
                .find_map(|k| value.get(*k).and_then(Value::as_i64))
                .and_then(epoch_number_to_time)
        });
    Some(AwsCredentials {
        access_key_id: access,
        secret_access_key: secret,
        session_token,
        expiration,
    })
}

fn json_str(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| value.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn epoch_number_to_time(n: i64) -> Option<SystemTime> {
    if n <= 0 {
        return None;
    }
    let n = n as u64;
    if n > 1_000_000_000_000 {
        UNIX_EPOCH.checked_add(Duration::from_millis(n))
    } else {
        UNIX_EPOCH.checked_add(Duration::from_secs(n))
    }
}

pub(crate) fn resolve_aws_region(profile: &ResolvedProfile) -> String {
    if let Some(region) = profile.http.aws_region.as_deref().filter(|s| !s.is_empty()) {
        return region.to_string();
    }
    if let Some(region) = env_nonempty("AWS_REGION").or_else(|| env_nonempty("AWS_DEFAULT_REGION"))
    {
        return region;
    }
    region_from_config().unwrap_or_else(|| DEFAULT_REGION.into())
}

fn region_from_config() -> Option<String> {
    let text = read_optional(&config_path().ok()?).ok().flatten()?;
    let profile = aws_profile_name();
    let section = ini_section(&text, &format!("profile {profile}"))
        .or_else(|| ini_section(&text, &profile))?;
    section.get("region").cloned().filter(|s| !s.is_empty())
}

pub(crate) fn missing_creds_message(profile: &ResolvedProfile) -> String {
    format!(
        "profile `{}` aws_service={} needs a bearer token or IAM credentials (AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, AWS_PROFILE / ~/.aws/credentials, credential_process, SSO cache, ECS, or IMDS)",
        profile.id,
        profile.http.aws_service.as_deref().unwrap_or("bedrock")
    )
}

fn merged_profile_section() -> Result<Option<BTreeMap<String, String>>, AwsSignError> {
    let name = aws_profile_name();
    let creds_text = read_optional(&credentials_path()?)?;
    let config_text = read_optional(&config_path()?)?;
    let from_creds = creds_text
        .as_deref()
        .and_then(|t| ini_section(t, &name).or_else(|| ini_section(t, &format!("profile {name}"))));
    let from_config = config_text
        .as_deref()
        .and_then(|t| ini_section(t, &format!("profile {name}")).or_else(|| ini_section(t, &name)));
    Ok(match (from_creds, from_config) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (Some(mut a), Some(b)) => {
            for (key, value) in b {
                a.entry(key).or_insert(value);
            }
            Some(a)
        }
    })
}

fn aws_profile_name() -> String {
    env_nonempty("AWS_PROFILE")
        .or_else(|| env_nonempty("AWS_DEFAULT_PROFILE"))
        .unwrap_or_else(|| "default".into())
}

fn credentials_path() -> Result<PathBuf, AwsSignError> {
    if let Some(path) = env_nonempty("AWS_SHARED_CREDENTIALS_FILE") {
        return Ok(PathBuf::from(path));
    }
    Ok(aws_home()?.join(".aws").join("credentials"))
}

fn config_path() -> Result<PathBuf, AwsSignError> {
    if let Some(path) = env_nonempty("AWS_CONFIG_FILE") {
        return Ok(PathBuf::from(path));
    }
    Ok(aws_home()?.join(".aws").join("config"))
}

fn aws_home() -> Result<PathBuf, AwsSignError> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| auth_err("HOME is unset; cannot read ~/.aws/credentials"))
}

fn read_optional(path: &Path) -> Result<Option<String>, AwsSignError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(auth_err(format!("read {}: {err}", path.display()))),
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn env_flag(name: &str) -> bool {
    env_nonempty(name).is_some_and(|v| {
        let v = v.to_ascii_lowercase();
        v == "1" || v == "true" || v == "yes"
    })
}

fn ini_section(text: &str, name: &str) -> Option<BTreeMap<String, String>> {
    let want = name.trim();
    let mut current = None;
    let mut out = BTreeMap::new();
    let mut found = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = Some(section.trim().to_string());
            continue;
        }
        if current.as_deref() != Some(want) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        found = true;
        out.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    found.then_some(out)
}

fn split_command(cmd: &str) -> Result<Vec<String>, AwsSignError> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Err(auth_err("credential_process is empty"));
    }
    if cmd.chars().any(|c| c.is_control() && c != '\t') {
        return Err(auth_err("credential_process contains a control character"));
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, c) if c == '"' || c == '\'' => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, '|')
            | (None, '&')
            | (None, ';')
            | (None, '`')
            | (None, '$')
            | (None, '(')
            | (None, ')')
            | (None, '>')
            | (None, '<') => {
                return Err(auth_err(
                    "credential_process must be an argv list, not a shell command",
                ));
            }
            (Some('"'), '\\') => match chars.peek() {
                Some('"') | Some('\\') => {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                }
                _ => cur.push('\\'),
            },
            (_, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err(auth_err("credential_process has an unclosed quote"));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        return Err(auth_err("credential_process is empty"));
    }
    Ok(out)
}

fn urlencoding_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    if s.len() < 19 || !s.is_ascii() {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let min: u32 = s.get(14..16)?.parse().ok()?;
    let sec: u32 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let y = i64::from(if month <= 2 { year - 1 } else { year });
    let m = if month <= 2 {
        i64::from(month) + 9
    } else {
        i64::from(month) - 3
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * m + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64;
    let days = era * 146_097 + doe as i64 - 719_468;
    let base_secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec);
    let tz_start = if s.len() > 19 && s.as_bytes()[19] == b'.' {
        s[20..].find(['Z', '+', '-']).map(|p| p + 20)
    } else if s.len() > 19 {
        Some(19)
    } else {
        None
    };
    let offset = match tz_start {
        Some(pos) if pos < s.len() && s.as_bytes()[pos] == b'Z' => 0,
        Some(pos) if pos + 5 <= s.len() => {
            let sign: i64 = if s.as_bytes()[pos] == b'+' { 1 } else { -1 };
            let oh: i64 = s.get(pos + 1..pos + 3)?.parse().ok()?;
            let om_start = if s.as_bytes()[pos + 3] == b':' {
                pos + 4
            } else {
                pos + 3
            };
            let om: i64 = s.get(om_start..om_start + 2)?.parse().ok()?;
            sign * (oh * 3600 + om * 60)
        }
        _ => 0,
    };
    let total = (base_secs - offset).max(0) as u64;
    Some(UNIX_EPOCH + Duration::from_secs(total))
}

async fn http_get(
    url: &str,
    headers: &[(&str, &str)],
    connect: Duration,
    total: Duration,
    no_proxy: bool,
) -> Result<(u16, String), AwsSignError> {
    http_send(reqwest::Method::GET, url, headers, connect, total, no_proxy).await
}

async fn http_put(
    url: &str,
    headers: &[(&str, &str)],
    connect: Duration,
    total: Duration,
    no_proxy: bool,
) -> Result<(u16, String), AwsSignError> {
    http_send(reqwest::Method::PUT, url, headers, connect, total, no_proxy).await
}

async fn http_send(
    method: reqwest::Method,
    url: &str,
    headers: &[(&str, &str)],
    connect: Duration,
    total: Duration,
    no_proxy: bool,
) -> Result<(u16, String), AwsSignError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(total)
        .redirect(reqwest::redirect::Policy::none());
    if no_proxy {
        builder = builder.no_proxy();
    }
    let client = builder
        .build()
        .map_err(|err| auth_err(format!("aws credential HTTP client: {err}")))?;
    let mut req = client.request(method, url);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let resp = req
        .send()
        .await
        .map_err(|err| auth_err(format!("aws credential request {url}: {err}")))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|err| auth_err(format!("aws credential body {url}: {err}")))?;
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use wiremux_auth::IsolatedHome;

    fn extra_aws_envs() -> &'static [&'static str] {
        &[
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            "AWS_CONTAINER_AUTHORIZATION_TOKEN",
            "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
            "AWS_EC2_METADATA_DISABLED",
            "AWS_EC2_METADATA_SERVICE_ENDPOINT",
            "AWS_EC2_METADATA_V1_DISABLED",
            "AWS_ENDPOINT_URL",
            "AWS_ENDPOINT_URL_SSO",
            "AWS_SSO_CACHE_DIR",
        ]
    }

    fn write_http(stream: &mut impl Write, status: u16, body: &str) {
        let resp = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    }

    fn read_headers(stream: &mut impl Read) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        loop {
            match stream.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn spawn_script(responses: Vec<(u16, String)>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                seen.push(read_headers(&mut stream));
                write_http(&mut stream, status, &body);
            }
            seen
        });
        (format!("http://{addr}"), handle)
    }

    fn spawn_counting(body: &'static str, hits: &'static AtomicUsize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                hits.fetch_add(1, Ordering::SeqCst);
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let _ = read_headers(&mut stream);
                write_http(&mut stream, 200, body);
            }
        });
        format!("http://{addr}/creds")
    }

    fn write_cred_process(dir: &Path, json: &str) -> PathBuf {
        #[cfg(windows)]
        {
            let json_path = dir.join("proc.json");
            std::fs::write(&json_path, json).expect("write json");
            let cmd_path = dir.join("credproc.cmd");
            std::fs::write(
                &cmd_path,
                format!("@echo off\r\ntype \"{}\"\r\n", json_path.display()),
            )
            .expect("write cmd");
            cmd_path
        }
        #[cfg(not(windows))]
        {
            let path = dir.join("credproc.sh");
            std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{json}'\n"))
                .expect("write sh");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            path
        }
    }

    #[test]
    fn split_command_rejects_shell_metacharacters() {
        let err = split_command("echo hi | cat").expect_err("pipe");
        assert!(err.to_string().contains("argv"), "{err}");
    }

    #[test]
    fn split_command_keeps_quoted_windows_path() {
        let argv = split_command(r#""C:\Program Files\tool.exe" --json"#).expect("split");
        assert_eq!(argv, ["C:\\Program Files\\tool.exe", "--json"], "{argv:?}");
    }

    #[test]
    fn ecs_full_uri_rejects_public_hosts() {
        let url = Url::parse("http://192.0.2.1/creds").expect("url");
        assert!(!ecs_full_uri_allowed(&url));
        let loopback = Url::parse("http://127.0.0.1:9/creds").expect("url");
        assert!(ecs_full_uri_allowed(&loopback));
    }

    #[test]
    fn process_json_requires_version_one() {
        let err =
            creds_from_process_json(br#"{"Version":2,"AccessKeyId":"A","SecretAccessKey":"B"}"#)
                .expect_err("v2");
        assert!(err.to_string().contains("Version 2"), "{err}");
    }

    #[tokio::test]
    async fn credential_process_resolves_keys() {
        let home = IsolatedHome::with_extra_envs(extra_aws_envs());
        clear_aws_credential_cache();
        home.set_env("AWS_EC2_METADATA_DISABLED", "true");
        let script = write_cred_process(
            home.path(),
            r#"{"Version":1,"AccessKeyId":"AKIAPROC","SecretAccessKey":"procsecret","SessionToken":"proctok"}"#,
        );
        let config = home.path().join("config");
        std::fs::write(
            &config,
            format!("[profile dev]\ncredential_process = {}\n", script.display()),
        )
        .expect("write config");
        home.set_env("AWS_PROFILE", "dev");
        home.set_env("AWS_CONFIG_FILE", config.to_str().expect("utf8"));
        let creds = resolve_aws_credentials()
            .await
            .expect("resolve")
            .expect("some");
        assert_eq!(creds.access_key_id, "AKIAPROC");
        assert_eq!(creds.secret_access_key, "procsecret");
        assert_eq!(creds.session_token, "proctok");
        let _ = home;
    }

    #[tokio::test]
    async fn sso_cache_and_portal_resolve_role_credentials() {
        let home = IsolatedHome::with_extra_envs(extra_aws_envs());
        clear_aws_credential_cache();
        home.set_env("AWS_EC2_METADATA_DISABLED", "true");
        let start = "https://example.awsapps.com/start";
        let cache_dir = home.path().join("sso-cache");
        std::fs::create_dir_all(&cache_dir).expect("mkdir cache");
        std::fs::write(
            cache_dir.join("token.json"),
            format!(
                r#"{{"startUrl":"{start}","accessToken":"sso-access","expiresAt":"2099-01-01T00:00:00Z"}}"#
            ),
        )
        .expect("write token");
        let role = r#"{"roleCredentials":{"accessKeyId":"AKIASSO","secretAccessKey":"ssosecret","sessionToken":"ssotok","expiration":4102444800000}}"#;
        let (portal, handle) = spawn_script(vec![(200, role.into())]);
        let config = home.path().join("config");
        std::fs::write(
            &config,
            format!(
                "[profile dev]\nsso_start_url = {start}\nsso_region = us-east-1\nsso_account_id = 123456789012\nsso_role_name = Admin\n"
            ),
        )
        .expect("write config");
        home.set_env("AWS_PROFILE", "dev");
        home.set_env("AWS_CONFIG_FILE", config.to_str().expect("utf8"));
        home.set_env("AWS_SSO_CACHE_DIR", cache_dir.to_str().expect("utf8"));
        home.set_env("AWS_ENDPOINT_URL_SSO", &portal);
        let creds = resolve_aws_credentials()
            .await
            .expect("resolve")
            .expect("some");
        let seen = handle.join().expect("join");
        assert_eq!(creds.access_key_id, "AKIASSO");
        assert_eq!(creds.session_token, "ssotok");
        assert!(
            seen.iter().any(|r| r
                .to_ascii_lowercase()
                .contains("x-amz-sso_bearer_token: sso-access")),
            "SSO must send the cached access token, got {seen:?}"
        );
        let _ = home;
    }

    #[tokio::test]
    async fn ecs_full_uri_resolves_and_caches() {
        static HITS: AtomicUsize = AtomicUsize::new(0);
        let home = IsolatedHome::with_extra_envs(extra_aws_envs());
        clear_aws_credential_cache();
        HITS.store(0, Ordering::SeqCst);
        home.set_env("AWS_EC2_METADATA_DISABLED", "true");
        let body = r#"{"AccessKeyId":"AKIAECS","SecretAccessKey":"ecssecret","Token":"ecstok","Expiration":"2099-01-01T00:00:00Z"}"#;
        let uri = spawn_counting(body, &HITS);
        home.set_env("AWS_CONTAINER_CREDENTIALS_FULL_URI", &uri);
        let first = resolve_aws_credentials()
            .await
            .expect("resolve1")
            .expect("some");
        let second = resolve_aws_credentials()
            .await
            .expect("resolve2")
            .expect("some");
        assert_eq!(first.access_key_id, "AKIAECS");
        assert_eq!(second.session_token, "ecstok");
        assert_eq!(
            HITS.load(Ordering::SeqCst),
            1,
            "unexpired ECS creds must cache"
        );
        let _ = home;
    }

    #[tokio::test]
    async fn expired_ecs_credentials_refresh() {
        static HITS: AtomicUsize = AtomicUsize::new(0);
        let home = IsolatedHome::with_extra_envs(extra_aws_envs());
        clear_aws_credential_cache();
        HITS.store(0, Ordering::SeqCst);
        home.set_env("AWS_EC2_METADATA_DISABLED", "true");
        let body = r#"{"AccessKeyId":"AKIAOLD","SecretAccessKey":"oldsecret","Token":"oldtok","Expiration":"2000-01-01T00:00:00Z"}"#;
        let uri = spawn_counting(body, &HITS);
        home.set_env("AWS_CONTAINER_CREDENTIALS_FULL_URI", &uri);
        let _ = resolve_aws_credentials().await.expect("resolve1");
        let _ = resolve_aws_credentials().await.expect("resolve2");
        assert!(
            HITS.load(Ordering::SeqCst) >= 2,
            "expired ECS creds must refresh, hits={}",
            HITS.load(Ordering::SeqCst)
        );
        let _ = home;
    }

    #[tokio::test]
    async fn imdsv2_resolves_role_credentials() {
        let home = IsolatedHome::with_extra_envs(extra_aws_envs());
        clear_aws_credential_cache();
        let creds_json = r#"{"Code":"Success","AccessKeyId":"AKIAIMDS","SecretAccessKey":"imdssecret","Token":"imdstok","Expiration":"2099-01-01T00:00:00Z"}"#;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..3 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let req = read_headers(&mut stream);
                let body = if req.starts_with("PUT ") {
                    "imds-token"
                } else if req.contains("security-credentials/")
                    && !req.contains("security-credentials/demo")
                {
                    "demo"
                } else {
                    creds_json
                };
                write_http(&mut stream, 200, body);
                seen.push(req);
            }
            seen
        });
        home.set_env(
            "AWS_EC2_METADATA_SERVICE_ENDPOINT",
            &format!("http://{addr}"),
        );
        let creds = resolve_aws_credentials()
            .await
            .expect("resolve")
            .expect("some");
        let seen = handle.join().expect("join");
        assert_eq!(creds.access_key_id, "AKIAIMDS");
        assert!(
            seen.iter().any(|r| r.starts_with("PUT ")),
            "must try IMDSv2 token first: {seen:?}"
        );
        let _ = home;
    }

    #[tokio::test]
    async fn missing_chain_is_none_when_imds_disabled() {
        let home = IsolatedHome::with_extra_envs(extra_aws_envs());
        clear_aws_credential_cache();
        home.set_env("AWS_EC2_METADATA_DISABLED", "true");
        let creds = resolve_aws_credentials().await.expect("resolve");
        assert!(creds.is_none(), "expected None, got {creds:?}");
        let _ = home;
    }

    #[test]
    fn cached_creds_stale_after_eighty_percent_of_ttl() {
        let creds = AwsCredentials {
            access_key_id: "A".into(),
            secret_access_key: "B".into(),
            session_token: String::new(),
            expiration: Some(SystemTime::now() + Duration::from_secs(100)),
        };
        let stale = CachedCreds {
            creds: creds.clone(),
            acquired_at: Instant::now() - Duration::from_secs(81),
            lifetime: Some(Duration::from_secs(100)),
        };
        assert!(!stale.fresh(), "creds must refresh after 80 percent of TTL");
        let fresh = CachedCreds {
            creds,
            acquired_at: Instant::now() - Duration::from_secs(10),
            lifetime: Some(Duration::from_secs(100)),
        };
        assert!(fresh.fresh(), "creds inside 80 percent of TTL stay cached");
    }

    #[tokio::test]
    async fn imds_token_failure_is_remembered() {
        static HITS: AtomicUsize = AtomicUsize::new(0);
        let home = IsolatedHome::with_extra_envs(extra_aws_envs());
        clear_aws_credential_cache();
        HITS.store(0, Ordering::SeqCst);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                HITS.fetch_add(1, Ordering::SeqCst);
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let _ = read_headers(&mut stream);
                write_http(&mut stream, 500, "");
            }
        });
        home.set_env(
            "AWS_EC2_METADATA_SERVICE_ENDPOINT",
            &format!("http://{addr}"),
        );
        let first = resolve_aws_credentials().await.expect("resolve1");
        assert!(
            first.is_none(),
            "failed IMDS must yield None, got {first:?}"
        );
        let second = resolve_aws_credentials().await.expect("resolve2");
        assert!(
            second.is_none(),
            "remembered IMDS miss must stay None, got {second:?}"
        );
        assert_eq!(
            HITS.load(Ordering::SeqCst),
            1,
            "IMDS miss must be remembered for the process"
        );
        let _ = home;
    }

    #[tokio::test]
    async fn ecs_credential_http_does_not_follow_redirect() {
        let home = IsolatedHome::with_extra_envs(extra_aws_envs());
        clear_aws_credential_cache();
        home.set_env("AWS_EC2_METADATA_DISABLED", "true");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let secret =
            r#"{"AccessKeyId":"AKIAREDIR","SecretAccessKey":"redirsecret","Token":"redir"}"#;
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let _ = read_headers(&mut stream);
                let loc = format!("http://{addr}/stolen");
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(resp.as_bytes());
            }
            if let Ok((mut stream, _)) = listener.accept() {
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let _ = read_headers(&mut stream);
                write_http(&mut stream, 200, secret);
            }
        });
        home.set_env(
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            &format!("http://{addr}/creds"),
        );
        let err = resolve_aws_credentials()
            .await
            .expect_err("redirect must not yield credentials");
        assert!(
            err.to_string().contains("302"),
            "expected ECS HTTP 302, got {err}"
        );
        let _ = home;
    }
}

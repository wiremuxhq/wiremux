//! AWS STS AssumeRole TokenProvider and SigV4 signing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;

use crate::TokenProvider;
use crate::error::AuthError;
use crate::helpers::{
    InFlight, format_oauth_transport_error, hex_encode, lead_or_follow, oauth_http_client,
    parse_rfc3339, parse_token_endpoint, percent_encode, read_oauth_body, vendor_rejected_summary,
};

const DEFAULT_REGION: &str = "us-east-1";
const STS_VERSION: &str = "2011-06-15";
const SIGV4_ALGO: &str = "AWS4-HMAC-SHA256";

/// Temporary credentials from STS AssumeRole.
#[derive(Clone)]
pub struct AwsCredentials {
    /// STS access key id.
    pub access_key_id: String,
    /// STS secret. Redacted in [`Debug`].
    pub secret_access_key: String,
    /// Session token for SigV4 `x-amz-security-token`.
    pub session_token: String,
    /// When the session expires, if STS sent `Expiration`.
    pub expiration: Option<SystemTime>,
}

impl std::fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsCredentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field("session_token", &"[REDACTED]")
            .field("expiration", &self.expiration)
            .finish()
    }
}

/// Inputs for [`sign_aws_request`].
#[derive(Clone, Debug)]
pub struct AwsSignParams<'a> {
    /// HTTP method (`GET`, `POST`).
    pub method: &'a str,
    /// Host header value (`sts.amazonaws.com`).
    pub host: &'a str,
    /// Canonical path (`/` when empty).
    pub path: &'a str,
    /// Already-canonical query string (no leading `?`).
    pub query: &'a str,
    /// AWS region (`us-east-1`).
    pub region: &'a str,
    /// AWS service (`sts`, `iam`).
    pub service: &'a str,
    /// Extra signed headers (`content-type`, ...). `host` and `x-amz-date` are added.
    pub extra_headers: &'a [(&'a str, &'a str)],
    /// Raw body bytes (hashed into the canonical request).
    pub payload: &'a [u8],
    /// `YYYYMMDDTHHMMSSZ`.
    pub amz_date: &'a str,
}

/// Long-lived keys used to call STS AssumeRole.
#[derive(Clone)]
pub struct AwsStsConfig {
    /// IAM access key id. Must be non-empty.
    pub access_key_id: String,
    /// IAM secret. Must be non-empty. Redacted in [`Debug`].
    pub secret_access_key: String,
    /// Role to assume.
    pub role_arn: String,
    /// STS session name. Empty becomes `wiremux`.
    pub role_session_name: String,
    /// Region for SigV4 and the default STS endpoint. Empty becomes `us-east-1`.
    pub region: String,
    /// Optional session token if the long-lived keys are themselves temporary.
    pub session_token: Option<String>,
    /// Override STS URL (tests, VPC endpoints).
    pub endpoint: Option<String>,
}

impl std::fmt::Debug for AwsStsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsStsConfig")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field("role_arn", &self.role_arn)
            .field("role_session_name", &self.role_session_name)
            .field("region", &self.region)
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

struct CachedCreds {
    creds: AwsCredentials,
    acquired_at: Instant,
    lifetime: Duration,
}

impl CachedCreds {
    fn needs_refresh(&self) -> bool {
        self.acquired_at.elapsed() >= self.lifetime.mul_f64(0.8)
    }
}

/// STS AssumeRole. Session keys are not a Bearer API token.
#[derive(Clone)]
pub struct AwsStsTokenProvider {
    inner: Arc<Inner>,
}

struct Inner {
    config: AwsStsConfig,
    http: reqwest::Client,
    force_refresh: AtomicBool,
    state: RwLock<Option<CachedCreds>>,
    inflight: InFlight,
}

impl std::fmt::Debug for AwsStsTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsStsTokenProvider")
            .field("config", &self.inner.config)
            .finish()
    }
}

impl AwsStsTokenProvider {
    /// Construct from long-lived keys. Fails if the access key or secret is empty.
    pub fn new(mut config: AwsStsConfig) -> Result<Self, AuthError> {
        if config.access_key_id.trim().is_empty() {
            return Err(AuthError::MissingField("AWS access_key_id".into()));
        }
        if config.secret_access_key.trim().is_empty() {
            return Err(AuthError::MissingField("AWS secret_access_key".into()));
        }
        if config.role_arn.trim().is_empty() {
            return Err(AuthError::MissingField("AWS role_arn".into()));
        }
        if config.role_session_name.trim().is_empty() {
            config.role_session_name = "wiremux".into();
        }
        if config.region.trim().is_empty() {
            config.region = DEFAULT_REGION.into();
        }
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                http: oauth_http_client()?,
                force_refresh: AtomicBool::new(false),
                state: RwLock::new(None),
                inflight: InFlight::new(),
            }),
        })
    }

    /// Temporary credentials from AssumeRole (cached at 80% of TTL).
    pub async fn get_credentials(&self) -> Result<AwsCredentials, AuthError> {
        // load() not swap: a concurrent get_credentials must not see a cleared
        // force flag before the InFlight claim.
        {
            let state = self.inner.state.read().await;
            if !self.inner.force_refresh.load(Ordering::SeqCst)
                && let Some(cached) = state.as_ref()
                && !cached.needs_refresh()
                && !self.inner.inflight.is_busy()
            {
                return Ok(cached.creds.clone());
            }
        }
        let token = lead_or_follow(&self.inner.inflight, || async {
            let force = self.inner.force_refresh.swap(false, Ordering::SeqCst);
            let result = self.refresh_as_leader(force).await;
            if result.is_err() && force {
                self.inner.force_refresh.store(true, Ordering::SeqCst);
            }
            result
        })
        .await?;
        let state = self.inner.state.read().await;
        state.as_ref().map(|c| c.creds.clone()).ok_or_else(|| {
            AuthError::TokenProvider(format!("STS cache empty after refresh ({token})"))
        })
    }

    async fn refresh_as_leader(&self, force: bool) -> Result<String, AuthError> {
        {
            let state = self.inner.state.read().await;
            if !force
                && let Some(cached) = state.as_ref()
                && !cached.needs_refresh()
            {
                return Ok(cached.creds.access_key_id.clone());
            }
        }
        let creds = self.assume_role().await?;
        let marker = creds.access_key_id.clone();
        let lifetime = creds
            .expiration
            .and_then(|exp| exp.duration_since(SystemTime::now()).ok())
            .unwrap_or(Duration::from_secs(3600));
        *self.inner.state.write().await = Some(CachedCreds {
            creds,
            acquired_at: Instant::now(),
            lifetime,
        });
        Ok(marker)
    }

    async fn assume_role(&self) -> Result<AwsCredentials, AuthError> {
        let cfg = &self.inner.config;
        let endpoint = cfg
            .endpoint
            .clone()
            .unwrap_or_else(|| default_sts_endpoint(&cfg.region));
        let url = parse_token_endpoint(&endpoint)?;
        let host = url.host_str().unwrap_or("sts.amazonaws.com").to_string();
        let path = if url.path().is_empty() {
            "/".to_string()
        } else {
            url.path().to_string()
        };
        let body = format!(
            "Action=AssumeRole&Version={}&RoleArn={}&RoleSessionName={}",
            percent_encode(STS_VERSION),
            percent_encode(&cfg.role_arn),
            percent_encode(&cfg.role_session_name),
        );
        let amz_date = amz_date_now();
        let sign_creds = AwsCredentials {
            access_key_id: cfg.access_key_id.clone(),
            secret_access_key: cfg.secret_access_key.clone(),
            session_token: cfg.session_token.clone().unwrap_or_default(),
            expiration: None,
        };
        let extra = [("content-type", "application/x-www-form-urlencoded")];
        let authorization = sign_aws_request(
            &sign_creds,
            &AwsSignParams {
                method: "POST",
                host: &host,
                path: &path,
                query: "",
                region: &cfg.region,
                service: "sts",
                extra_headers: &extra,
                payload: body.as_bytes(),
                amz_date: &amz_date,
            },
        )?;
        let parsed = parse_token_endpoint(&endpoint)?;
        let mut req = self
            .inner
            .http
            .post(parsed)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("host", &host)
            .header("x-amz-date", &amz_date)
            .header("authorization", &authorization)
            .body(body);
        if let Some(token) = cfg.session_token.as_deref().filter(|s| !s.is_empty()) {
            req = req.header("x-amz-security-token", token);
        }
        let resp = req.send().await.map_err(|e| {
            AuthError::TokenProvider(format_oauth_transport_error(
                "STS AssumeRole request failed",
                &e,
                &endpoint,
            ))
        })?;
        let status = resp.status().as_u16();
        let text = read_oauth_body(resp).await?;
        if !(200..300).contains(&status) {
            return Err(AuthError::VendorRejected {
                status,
                summary: vendor_rejected_summary("STS AssumeRole failed", &text, &endpoint),
            });
        }
        parse_assume_role_xml(&text)
    }
}

fn default_sts_endpoint(region: &str) -> String {
    if region == DEFAULT_REGION {
        "https://sts.amazonaws.com/".into()
    } else {
        format!("https://sts.{region}.amazonaws.com/")
    }
}

fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let rest = body.get(start..)?;
    let end = rest.find(&close)?;
    Some(rest[..end].trim().to_string())
}

fn parse_assume_role_xml(body: &str) -> Result<AwsCredentials, AuthError> {
    let access_key_id = xml_tag(body, "AccessKeyId").filter(|s| !s.is_empty());
    let secret_access_key = xml_tag(body, "SecretAccessKey").filter(|s| !s.is_empty());
    let session_token = xml_tag(body, "SessionToken").unwrap_or_default();
    let (Some(access_key_id), Some(secret_access_key)) = (access_key_id, secret_access_key) else {
        return Err(AuthError::TokenProvider(
            "STS AssumeRole XML missing AccessKeyId or SecretAccessKey".into(),
        ));
    };
    let expiration = xml_tag(body, "Expiration")
        .as_deref()
        .and_then(|s| parse_rfc3339(s).ok());
    Ok(AwsCredentials {
        access_key_id,
        secret_access_key,
        session_token,
        expiration,
    })
}

/// Sign a request with SigV4. Returns the `Authorization` header value.
pub fn sign_aws_request(
    creds: &AwsCredentials,
    params: &AwsSignParams<'_>,
) -> Result<String, AuthError> {
    if params.amz_date.len() < 8 {
        return Err(AuthError::TokenProvider("x-amz-date is too short".into()));
    }
    let date_stamp = &params.amz_date[..8];
    let mut headers: Vec<(String, String)> = Vec::new();
    headers.push(("host".into(), params.host.to_string()));
    headers.push(("x-amz-date".into(), params.amz_date.to_string()));
    if !creds.session_token.is_empty()
        && !params
            .extra_headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("x-amz-security-token"))
    {
        headers.push(("x-amz-security-token".into(), creds.session_token.clone()));
    }
    for (k, v) in params.extra_headers {
        headers.push((k.to_ascii_lowercase(), (*v).to_string()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    headers.dedup_by(|a, b| a.0 == b.0);

    let canonical_headers = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", trim_header_value(v)))
        .collect::<String>();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let path = if params.path.is_empty() {
        "/"
    } else {
        params.path
    };
    let payload_hash = hex_encode(&sha256(params.payload));
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        params.method.to_ascii_uppercase(),
        canonical_uri(path),
        params.query,
        canonical_headers,
        signed_headers,
        payload_hash
    );
    let credential_scope = format!(
        "{}/{}/{}/aws4_request",
        date_stamp, params.region, params.service
    );
    let string_to_sign = format!(
        "{SIGV4_ALGO}\n{}\n{credential_scope}\n{}",
        params.amz_date,
        hex_encode(&sha256(canonical.as_bytes()))
    );
    let signing_key = signing_key(
        &creds.secret_access_key,
        date_stamp,
        params.region,
        params.service,
    )?;
    let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
    Ok(format!(
        "{SIGV4_ALGO} Credential={}/{}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key_id, credential_scope
    ))
}

fn trim_header_value(v: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for ch in v.trim().chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".into();
    }
    path.split('/')
        .map(|seg| {
            if seg.is_empty() {
                String::new()
            } else {
                percent_encode(seg)
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>, AuthError> {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key)
        .map_err(|e| AuthError::TokenProvider(format!("HMAC-SHA256 key: {e}")))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn signing_key(
    secret: &str,
    date: &str,
    region: &str,
    service: &str,
) -> Result<Vec<u8>, AuthError> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, service.as_bytes())?;
    hmac_sha256(&k_service, b"aws4_request")
}

fn amz_date_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_amz_date(secs).0
}

/// `(YYYYMMDDTHHMMSSZ, YYYYMMDD)` from a Unix timestamp.
pub(crate) fn format_amz_date(unix_secs: u64) -> (String, String) {
    let days = (unix_secs / 86_400) as i64;
    let sod = unix_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = sod / 3600;
    let min = (sod % 3600) / 60;
    let sec = sod % 60;
    (
        format!("{year:04}{month:02}{day:02}T{hour:02}{min:02}{sec:02}Z"),
        format!("{year:04}{month:02}{day:02}"),
    )
}

/// Howard Hinnant civil-from-days. `z` is days since 1970-01-01.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

impl TokenProvider for AwsStsTokenProvider {
    fn mark_stale(&self) {
        self.inner.force_refresh.store(true, Ordering::SeqCst);
    }

    async fn get_token(&self) -> Result<String, AuthError> {
        Err(AuthError::TokenProvider(
            "AWS STS session is not a Bearer API key; use get_credentials + sign_aws_request"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    const STS_XML: &str = r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleResult>
    <Credentials>
      <AccessKeyId>ASIAEXAMPLE</AccessKeyId>
      <SecretAccessKey>secretExample</SecretAccessKey>
      <SessionToken>tokenExample</SessionToken>
      <Expiration>2099-01-01T00:00:00Z</Expiration>
    </Credentials>
  </AssumeRoleResult>
</AssumeRoleResponse>"#;

    fn read_http_headers(stream: &mut impl Read) -> Vec<u8> {
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
        buf
    }

    fn write_http_xml(stream: &mut impl Write, status: u16, body: &str) {
        let resp = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    }

    fn spawn_http_server(status: u16, body: &str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("local_addr");
        let body = body.to_owned();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let buf = read_http_headers(&mut stream);
            write_http_xml(&mut stream, status, &body);
            String::from_utf8_lossy(&buf).into_owned()
        });
        (format!("http://{addr}/"), handle)
    }

    /// Accepts at most one request, then returns `None` if nobody connected.
    fn spawn_http_server_optional(
        status: u16,
        body: &str,
        accept_timeout: Duration,
    ) -> (String, thread::JoinHandle<Option<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("local_addr");
        let body = body.to_owned();
        let handle = thread::spawn(move || {
            let start = Instant::now();
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                        let buf = read_http_headers(&mut stream);
                        write_http_xml(&mut stream, status, &body);
                        return Some(String::from_utf8_lossy(&buf).into_owned());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if start.elapsed() >= accept_timeout {
                            return None;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return None,
                }
            }
        });
        (format!("http://{addr}/"), handle)
    }

    fn spawn_http_script(responses: &[(u16, &str)]) -> (String, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("local_addr");
        let responses: Vec<(u16, String)> = responses
            .iter()
            .map(|(status, body)| (*status, (*body).to_owned()))
            .collect();
        let handle = thread::spawn(move || {
            let mut served = 0usize;
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let _ = read_http_headers(&mut stream);
                write_http_xml(&mut stream, status, &body);
                served += 1;
            }
            served
        });
        (format!("http://{addr}/"), handle)
    }

    fn cfg(endpoint: &str) -> AwsStsConfig {
        AwsStsConfig {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            role_arn: "arn:aws:iam::123456789012:role/demo".into(),
            role_session_name: "wiremux-test".into(),
            region: "us-east-1".into(),
            session_token: None,
            endpoint: Some(endpoint.into()),
        }
    }

    #[test]
    fn empty_access_key_fails_construction() {
        let err = AwsStsTokenProvider::new(AwsStsConfig {
            access_key_id: String::new(),
            secret_access_key: "secret".into(),
            role_arn: "arn:aws:iam::1:role/x".into(),
            role_session_name: "s".into(),
            region: "us-east-1".into(),
            session_token: None,
            endpoint: None,
        })
        .expect_err("empty key");
        assert!(err.to_string().contains("access_key_id"), "{err}");
    }

    #[test]
    fn empty_secret_fails_construction() {
        let err = AwsStsTokenProvider::new(AwsStsConfig {
            access_key_id: "AKI".into(),
            secret_access_key: "  ".into(),
            role_arn: "arn:aws:iam::1:role/x".into(),
            role_session_name: "s".into(),
            region: "us-east-1".into(),
            session_token: None,
            endpoint: None,
        })
        .expect_err("empty secret");
        assert!(err.to_string().contains("secret_access_key"), "{err}");
    }

    #[test]
    fn debug_redacts_secrets() {
        let p = AwsStsTokenProvider::new(cfg("https://sts.amazonaws.com/")).expect("new");
        let debug = format!("{p:?}");
        assert!(!debug.contains("wJalrXUtnFEMI"), "{debug}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
    }

    #[test]
    fn format_amz_date_known_vector() {
        let (full, date) = format_amz_date(1_440_938_160);
        assert_eq!(full, "20150830T123600Z");
        assert_eq!(date, "20150830");
        let (epoch, epoch_day) = format_amz_date(0);
        assert_eq!(epoch, "19700101T000000Z");
        assert_eq!(epoch_day, "19700101");
    }

    #[test]
    fn signing_key_matches_aws_docs() {
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        )
        .expect("signing_key");
        assert_eq!(
            hex_encode(&key),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn hmac_sha256_empty_key_does_not_panic() {
        let mac = hmac_sha256(b"", b"data").expect("HMAC-SHA256 accepts empty key");
        assert_eq!(mac.len(), 32);
    }

    #[test]
    fn sign_aws_request_matches_iam_get_example() {
        let creds = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: String::new(),
            expiration: None,
        };
        let extra = [(
            "content-type",
            "application/x-www-form-urlencoded; charset=utf-8",
        )];
        let auth = sign_aws_request(
            &creds,
            &AwsSignParams {
                method: "GET",
                host: "iam.amazonaws.com",
                path: "/",
                query: "Action=ListUsers&Version=2010-05-08",
                region: "us-east-1",
                service: "iam",
                extra_headers: &extra,
                payload: b"",
                amz_date: "20150830T123600Z",
            },
        )
        .expect("sign");
        assert!(
            auth.contains(
                "Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
            ),
            "{auth}"
        );
        assert!(
            auth.contains("Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request"),
            "{auth}"
        );
    }

    #[tokio::test]
    async fn assume_role_returns_temp_keys_and_get_token_refuses() {
        let (url, handle) = spawn_http_server(200, STS_XML);
        let p = AwsStsTokenProvider::new(cfg(&url)).expect("new");
        let creds = p.get_credentials().await.expect("creds");
        assert_eq!(creds.access_key_id, "ASIAEXAMPLE");
        assert_eq!(creds.secret_access_key, "secretExample");
        assert_eq!(creds.session_token, "tokenExample");
        let err = p.get_token().await.expect_err("not a bearer");
        let msg = err.to_string();
        assert!(msg.contains("get_credentials"), "{msg}");
        assert!(msg.contains("sign_aws_request"), "{msg}");
        let req = handle.join().expect("join");
        assert!(
            req.contains("Action=AssumeRole") || req.contains("Action%3DAssumeRole"),
            "{req}"
        );
    }

    #[test]
    fn parse_sts_xml() {
        let creds = parse_assume_role_xml(STS_XML).expect("xml");
        assert_eq!(creds.access_key_id, "ASIAEXAMPLE");
        assert!(creds.expiration.is_some());
    }

    #[tokio::test]
    async fn get_token_does_not_call_assume_role() {
        let (url, handle) = spawn_http_server_optional(200, STS_XML, Duration::from_millis(400));
        let p = AwsStsTokenProvider::new(cfg(&url)).expect("new");
        let err = p.get_token().await.expect_err("not a bearer");
        let msg = err.to_string();
        assert!(msg.contains("not a Bearer"), "{msg}");
        let accepted = handle.join().expect("join");
        assert!(
            accepted.is_none(),
            "get_token must not call STS, got {accepted:?}"
        );
    }

    const STS_XML_RETRY: &str = r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleResult>
    <Credentials>
      <AccessKeyId>ASIARETRY</AccessKeyId>
      <SecretAccessKey>secretRetry</SecretAccessKey>
      <SessionToken>tokenRetry</SessionToken>
      <Expiration>2099-01-01T00:00:00Z</Expiration>
    </Credentials>
  </AssumeRoleResult>
</AssumeRoleResponse>"#;

    #[tokio::test]
    async fn get_credentials_retries_after_forced_refresh_http_error() {
        let (url, handle) = spawn_http_script(&[
            (200, STS_XML),
            (
                403,
                "<ErrorResponse><Error><Code>AccessDenied</Code></Error></ErrorResponse>",
            ),
            (200, STS_XML_RETRY),
        ]);
        let p = AwsStsTokenProvider::new(cfg(&url)).expect("new");
        let first = p.get_credentials().await.expect("cache");
        assert_eq!(first.access_key_id, "ASIAEXAMPLE");
        p.mark_stale();
        let err = p.get_credentials().await.expect_err("forced 403");
        let msg = err.to_string();
        assert!(msg.contains("403") || msg.contains("AssumeRole"), "{msg}");
        assert_eq!(
            msg.matches("(HTTP ").count(),
            1,
            "VendorRejected must not re-wrap HTTP: {msg}"
        );
        let retry = p.get_credentials().await.expect("retry after failed force");
        assert_eq!(retry.access_key_id, "ASIARETRY");
        assert_eq!(handle.join().expect("join"), 3);
    }
}

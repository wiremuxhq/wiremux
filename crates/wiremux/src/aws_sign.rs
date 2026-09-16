//! Shared SigV4 signing for [`crate::client::WireClient`] and the proxy.

use reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue};
use wiremux_auth::{AwsCredentials, AwsSignParams, ResolvedProfile, sign_aws_request};

use crate::aws_creds::{missing_creds_message, resolve_aws_credentials, resolve_aws_region};

const AMZ_DATE: HeaderName = HeaderName::from_static("x-amz-date");
const AMZ_SECURITY_TOKEN: HeaderName = HeaderName::from_static("x-amz-security-token");

#[derive(Debug)]
pub(crate) enum AwsSignError {
    Auth(String),
    Transport(String),
}

impl std::fmt::Display for AwsSignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth(message) | Self::Transport(message) => f.write_str(message),
        }
    }
}

#[derive(Debug)]
pub(crate) struct AwsSigV4Headers {
    authorization: String,
    amz_date: String,
    session_token: String,
}

/// Skip when `aws_service` is absent. Fail closed if it is set and keys
/// are missing, the URL cannot be parsed, or signing fails.
pub(crate) fn prepare_aws_sigv4(
    profile: &ResolvedProfile,
    url: &str,
    method: &str,
    body: &[u8],
    creds: Option<&AwsCredentials>,
    amz_date: &str,
) -> Result<Option<AwsSigV4Headers>, AwsSignError> {
    let Some(service) = profile
        .http
        .aws_service
        .as_deref()
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let Some(creds) =
        creds.filter(|c| !c.access_key_id.is_empty() && !c.secret_access_key.is_empty())
    else {
        return Err(AwsSignError::Auth(missing_creds_message(profile)));
    };
    let parsed = reqwest::Url::parse(url)
        .map_err(|err| AwsSignError::Transport(format!("aws sigv4 url: {err}")))?;
    let host = parsed
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| AwsSignError::Transport("aws sigv4 url has no host".into()))?;
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let query = parsed.query().unwrap_or("");
    let region = resolve_aws_region(profile);
    let extra = [("content-type", "application/json")];
    let authorization = sign_aws_request(
        creds,
        &AwsSignParams {
            method,
            host,
            path,
            query,
            region: &region,
            service,
            extra_headers: &extra,
            payload: body,
            amz_date,
        },
    )
    .map_err(|err| AwsSignError::Auth(err.to_string()))?;
    Ok(Some(AwsSigV4Headers {
        authorization,
        amz_date: amz_date.to_string(),
        session_token: creds.session_token.clone(),
    }))
}

/// Sign after the final body is known. Bearer wins: skip SigV4 when a
/// bearer token was already applied. Header insert replaces, never appends.
pub(crate) async fn apply_aws_sigv4(
    profile: &ResolvedProfile,
    url: &str,
    method: &str,
    body: &[u8],
    req: reqwest::RequestBuilder,
    bearer_applied: bool,
) -> Result<reqwest::Request, AwsSignError> {
    let mut built = req
        .build()
        .map_err(|err| AwsSignError::Transport(err.to_string()))?;
    if bearer_applied {
        return Ok(built);
    }
    if profile
        .http
        .aws_service
        .as_deref()
        .is_none_or(str::is_empty)
    {
        return Ok(built);
    }
    let creds = resolve_aws_credentials()
        .await?
        .ok_or_else(|| AwsSignError::Auth(missing_creds_message(profile)))?;
    let Some(headers) =
        prepare_aws_sigv4(profile, url, method, body, Some(&creds), &amz_date_now())?
    else {
        return Ok(built);
    };
    insert_sigv4(&mut built, headers)?;
    Ok(built)
}

fn insert_sigv4(req: &mut reqwest::Request, headers: AwsSigV4Headers) -> Result<(), AwsSignError> {
    let map = req.headers_mut();
    map.insert(AUTHORIZATION, parse_header(&headers.authorization)?);
    map.insert(AMZ_DATE, parse_header(&headers.amz_date)?);
    if !headers.session_token.is_empty() {
        map.insert(AMZ_SECURITY_TOKEN, parse_header(&headers.session_token)?);
    }
    Ok(())
}

fn parse_header(value: &str) -> Result<HeaderValue, AwsSignError> {
    HeaderValue::from_str(value)
        .map_err(|err| AwsSignError::Transport(format!("aws sigv4 header: {err}")))
}

pub(crate) fn amz_date_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let hour = sod / 3600;
    let min = (sod % 3600) / 60;
    let sec = sod % 60;
    format!("{year:04}{m:02}{d:02}T{hour:02}{min:02}{sec:02}Z")
}

pub(crate) fn bearer_token_applied(profile: &ResolvedProfile, token: Option<&str>) -> bool {
    let Some(token) = token.filter(|t| !t.trim().is_empty()) else {
        return false;
    };
    match profile
        .http
        .auth_scheme
        .clone()
        .unwrap_or(wiremux_auth::AuthScheme::Bearer)
    {
        wiremux_auth::AuthScheme::None => false,
        wiremux_auth::AuthScheme::Bearer => true,
        wiremux_auth::AuthScheme::XApiKey => token.starts_with("sk-ant-oat"),
        wiremux_auth::AuthScheme::Header(name) => name.eq_ignore_ascii_case("authorization"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;
    use wiremux_auth::parse_profile_str;

    fn bedrock_sigv4_profile() -> ResolvedProfile {
        parse_profile_str(
            r#"
schema_version = 1
id = "amazon-bedrock"
wire = "converse"
aws_service = "bedrock"
aws_region = "us-east-1"
base_url = "https://bedrock-runtime.us-east-1.amazonaws.com"
"#,
        )
        .expect("parse")
    }

    fn test_aws_creds() -> AwsCredentials {
        AwsCredentials {
            access_key_id: "AKIATEST".into(),
            secret_access_key: "secret".into(),
            session_token: String::new(),
            expiration: None,
        }
    }

    fn assert_sign_closed(result: Result<Option<AwsSigV4Headers>, AwsSignError>, what: &str) {
        match result {
            Err(AwsSignError::Auth(msg) | AwsSignError::Transport(msg)) => {
                assert!(!msg.is_empty(), "{what}: empty error");
            }
            Ok(v) => panic!("{what}: expected Err, got Ok({v:?})"),
        }
    }

    #[test]
    fn aws_sigv4_bad_url_with_keys_is_error() {
        let result = prepare_aws_sigv4(
            &bedrock_sigv4_profile(),
            "not a url",
            "POST",
            b"{}",
            Some(&test_aws_creds()),
            "20260915T000000Z",
        );
        assert_sign_closed(result, "keys + bad url");
    }

    #[test]
    fn aws_sigv4_missing_host_with_keys_is_error() {
        let result = prepare_aws_sigv4(
            &bedrock_sigv4_profile(),
            "file:///tmp/bedrock",
            "POST",
            b"{}",
            Some(&test_aws_creds()),
            "20260915T000000Z",
        );
        assert_sign_closed(result, "keys + missing host");
    }

    #[test]
    fn aws_sigv4_sign_failure_with_keys_is_error() {
        let result = prepare_aws_sigv4(
            &bedrock_sigv4_profile(),
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/x/converse",
            "POST",
            b"{}",
            Some(&test_aws_creds()),
            "short",
        );
        assert_sign_closed(result, "keys + sign failure");
    }

    #[test]
    fn aws_sigv4_missing_keys_is_error() {
        let result = prepare_aws_sigv4(
            &bedrock_sigv4_profile(),
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/x/converse",
            "POST",
            b"{}",
            None,
            "20260915T000000Z",
        );
        assert_sign_closed(result, "aws_service without keys");
    }

    #[test]
    fn aws_sigv4_known_vector_sets_authorization() {
        let out = prepare_aws_sigv4(
            &bedrock_sigv4_profile(),
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/x/converse",
            "POST",
            b"{}",
            Some(&AwsCredentials {
                access_key_id: "AKIDEXAMPLE".into(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
                session_token: String::new(),
                expiration: None,
            }),
            "20150830T123600Z",
        )
        .expect("sign")
        .expect("headers");
        assert!(
            out.authorization.starts_with("AWS4-HMAC-SHA256 "),
            "{}",
            out.authorization
        );
        assert!(
            out.authorization
                .contains("Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request"),
            "{}",
            out.authorization
        );
        assert_eq!(out.amz_date, "20150830T123600Z");
    }

    #[tokio::test]
    async fn apply_skips_sigv4_when_bearer_applied() {
        let http = reqwest::Client::new();
        let url = "https://bedrock-runtime.us-east-1.amazonaws.com/model/x/converse";
        let req = http
            .post(url)
            .header(AUTHORIZATION, "Bearer bedrock-bearer-tok")
            .body("{}");
        let built = apply_aws_sigv4(&bedrock_sigv4_profile(), url, "POST", b"{}", req, true)
            .await
            .expect("build");
        let auths: Vec<_> = built
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(|v| v.to_str().unwrap_or(""))
            .collect();
        assert_eq!(auths, ["Bearer bedrock-bearer-tok"]);
    }
}

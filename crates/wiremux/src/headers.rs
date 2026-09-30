//! Shared profile header / auth_scheme / betas application.

use std::time::Duration;

use wiremux_auth::{AnyTokenProvider, AuthScheme, ResolvedProfile};

pub(crate) fn default_http_client(profile: &ResolvedProfile) -> Result<reqwest::Client, String> {
    let read_secs = profile
        .http
        .read_timeout_secs
        .filter(|&s| s > 0)
        .unwrap_or(120);
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(read_secs))
        .build()
        .map_err(|err| err.to_string())
}

const GROK_BUILD_PROXY_HOST: &str = "cli-chat-proxy.grok.com";
const GROK_CLIENT_VERSION: &str = "0.1.202";
const GROK_CLIENT_IDENTIFIER: &str = "wiremux";

pub(crate) fn apply_profile_headers(
    mut req: reqwest::RequestBuilder,
    profile: &ResolvedProfile,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    let ua_override = profile
        .fingerprint
        .as_ref()
        .and_then(|fp| fp.user_agent.as_ref())
        .is_some();
    let beta_override = !profile.betas.values.is_empty();
    let mut profile_auth = false;
    for (name, value) in &profile.http.headers {
        if ua_override && name.eq_ignore_ascii_case("user-agent") {
            continue;
        }
        if beta_override && name.eq_ignore_ascii_case(&profile.betas.header) {
            continue;
        }
        if is_profile_auth_header(profile, name) {
            if value.trim().is_empty() {
                continue;
            }
            // Profile auth is the credential. Send it raw, once.
            profile_auth = true;
        }
        req = req.header(name, value);
    }
    for (name, value) in grok_build_default_headers(profile) {
        req = req.header(name, value);
    }
    if let Some(fp) = &profile.fingerprint {
        if let Some(ua) = fp.user_agent.as_deref() {
            req = req.header("user-agent", ua);
        }
        if let Some(app) = fp.x_app.as_deref() {
            req = req.header("x-app", app);
        }
    }
    if beta_override {
        req = req.header(&profile.betas.header, profile.betas.values.join(","));
    }
    if !profile_auth && let Some(token) = token {
        match profile
            .http
            .auth_scheme
            .clone()
            .unwrap_or(AuthScheme::Bearer)
        {
            AuthScheme::None => {}
            AuthScheme::Bearer => {
                req = req.header("authorization", format!("Bearer {token}"));
            }
            AuthScheme::XApiKey => {
                if token.starts_with("sk-ant-oat") {
                    req = req.header("authorization", format!("Bearer {token}"));
                } else {
                    req = req.header("x-api-key", token);
                }
            }
            AuthScheme::Header(name) => {
                req = req.header(name, token);
            }
        }
    }
    req
}

pub(crate) fn is_profile_auth_header(profile: &ResolvedProfile, name: &str) -> bool {
    if name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key") {
        return true;
    }
    matches!(
        &profile.http.auth_scheme,
        Some(AuthScheme::Header(header)) if name.eq_ignore_ascii_case(header)
    )
}

/// Provider-derived headers after profile headers. Profile keys win.
pub(crate) fn apply_provider_headers(
    mut req: reqwest::RequestBuilder,
    profile: &ResolvedProfile,
    provider: &AnyTokenProvider,
) -> reqwest::RequestBuilder {
    if header_present(profile, "x-goog-user-project") {
        return req;
    }
    if let Some(quota) = provider.quota_project_id() {
        req = req.header("x-goog-user-project", quota);
    }
    req
}

fn header_present(profile: &ResolvedProfile, name: &str) -> bool {
    profile
        .http
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case(name))
}

fn grok_build_proxy_host(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    host.trim_end_matches('.')
        .eq_ignore_ascii_case(GROK_BUILD_PROXY_HOST)
}

/// Extra Grok Build CLI headers when `base_url` is the proxy and the
/// profile did not already set them. Never applied to `api.x.ai`.
pub(crate) fn grok_build_default_headers(
    profile: &ResolvedProfile,
) -> Vec<(&'static str, &'static str)> {
    let Some(base) = profile.http.base_url.as_deref() else {
        return Vec::new();
    };
    if !grok_build_proxy_host(base) {
        return Vec::new();
    }
    let mut extra = Vec::new();
    if !header_present(profile, "x-grok-client-version") {
        extra.push(("x-grok-client-version", GROK_CLIENT_VERSION));
    }
    if !header_present(profile, "x-grok-client-identifier") {
        extra.push(("x-grok-client-identifier", GROK_CLIENT_IDENTIFIER));
    }
    extra
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremux_auth::parse_profile_str;

    fn profile(toml: &str) -> ResolvedProfile {
        parse_profile_str(toml).expect("test profile")
    }

    #[test]
    fn handmade_cli_chat_proxy_gets_grok_build_headers() {
        let p = profile(
            r#"
schema_version = 1
id = "handmade-proxy"
wire = "chat-completions"
base_url = "https://cli-chat-proxy.grok.com"
chat_path = "/v1/chat/completions"
"#,
        );
        let extra = grok_build_default_headers(&p);
        assert!(
            extra.contains(&("x-grok-client-version", "0.1.202")),
            "missing version, got {extra:?}"
        );
        assert!(
            extra.contains(&("x-grok-client-identifier", "wiremux")),
            "missing identifier, got {extra:?}"
        );
    }

    #[test]
    fn trailing_dot_cli_chat_proxy_gets_grok_build_headers() {
        let p = profile(
            r#"
schema_version = 1
id = "handmade-proxy-fqdn"
wire = "chat-completions"
base_url = "https://cli-chat-proxy.grok.com."
chat_path = "/v1/chat/completions"
"#,
        );
        let extra = grok_build_default_headers(&p);
        assert!(
            extra.contains(&("x-grok-client-version", "0.1.202")),
            "trailing-dot host must match, got {extra:?}"
        );
    }

    #[test]
    fn api_x_ai_does_not_get_grok_build_headers() {
        let p = profile(
            r#"
schema_version = 1
id = "handmade-xai"
wire = "chat-completions"
base_url = "https://api.x.ai"
chat_path = "/v1/chat/completions"
"#,
        );
        assert!(
            grok_build_default_headers(&p).is_empty(),
            "api.x.ai must not send Grok CLI headers"
        );
    }

    #[test]
    fn overlay_version_is_not_replaced() {
        let p = profile(
            r#"
schema_version = 1
id = "overlay-version"
wire = "chat-completions"
base_url = "https://cli-chat-proxy.grok.com"
chat_path = "/v1/chat/completions"

[headers]
x-grok-client-version = "9.9.9"
"#,
        );
        let extra = grok_build_default_headers(&p);
        assert!(
            !extra
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("x-grok-client-version")),
            "must keep overlay version, got {extra:?}"
        );
        assert!(
            extra.contains(&("x-grok-client-identifier", "wiremux")),
            "missing identifier still filled, got {extra:?}"
        );
    }

    fn built(toml: &str, token: Option<&str>) -> reqwest::Request {
        let client = reqwest::Client::new();
        apply_profile_headers(
            client.request(reqwest::Method::POST, "http://127.0.0.1/v1"),
            &profile(toml),
            token,
        )
        .build()
        .expect("request builds")
    }

    #[test]
    fn profile_x_api_key_stays_on_that_header() {
        let req = built(
            r#"
schema_version = 1
id = "k"
wire = "chat-completions"
base_url = "http://127.0.0.1"
auth_scheme = "bearer"
[headers]
x-api-key = "k"
"#,
            Some("k"),
        );
        assert!(
            req.headers().get("authorization").is_none(),
            "{:?}",
            req.headers()
        );
        assert_eq!(req.headers().get("x-api-key").unwrap(), "k");
    }

    #[test]
    fn profile_basic_authorization_is_not_rewrapped() {
        let req = built(
            r#"
schema_version = 1
id = "basic"
wire = "chat-completions"
base_url = "http://127.0.0.1"
auth_scheme = "bearer"
[headers]
Authorization = "Basic abc"
"#,
            Some("Basic abc"),
        );
        assert_eq!(req.headers().get("authorization").unwrap(), "Basic abc");
        assert_eq!(req.headers().get_all("authorization").iter().count(), 1);
    }

    #[test]
    fn profile_oat_key_is_not_forced_to_bearer() {
        let req = built(
            r#"
schema_version = 1
id = "oat"
wire = "messages"
base_url = "http://127.0.0.1"
auth_scheme = "x-api-key"
[headers]
x-api-key = "sk-ant-oat01-test"
"#,
            Some("sk-ant-oat01-test"),
        );
        assert!(
            req.headers().get("authorization").is_none(),
            "{:?}",
            req.headers()
        );
        assert_eq!(req.headers().get("x-api-key").unwrap(), "sk-ant-oat01-test");
    }

    #[test]
    fn provider_oat_token_still_uses_bearer() {
        let req = built(
            r#"
schema_version = 1
id = "provider-oat"
wire = "messages"
base_url = "http://127.0.0.1"
auth_scheme = "x-api-key"
"#,
            Some("sk-ant-oat01-test"),
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer sk-ant-oat01-test"
        );
    }

    #[test]
    fn fingerprint_user_agent_replaces_profile_user_agent() {
        let req = built(
            r#"
schema_version = 1
id = "ua"
wire = "chat-completions"
base_url = "http://127.0.0.1"
auth_scheme = "none"
[headers]
user-agent = "profile-ua"
[fingerprint]
user_agent = "fingerprint-ua"
"#,
            None,
        );
        let values: Vec<_> = req.headers().get_all("user-agent").iter().collect();
        assert_eq!(values.len(), 1, "{values:?}");
        assert_eq!(values[0], "fingerprint-ua");
    }

    #[test]
    fn betas_header_is_not_sent_twice() {
        let req = built(
            r#"
schema_version = 1
id = "betas"
wire = "messages"
base_url = "http://127.0.0.1"
auth_scheme = "none"
[headers]
anthropic-beta = "from-profile"
[betas]
values = ["from-betas"]
header = "anthropic-beta"
"#,
            None,
        );
        let values: Vec<_> = req.headers().get_all("anthropic-beta").iter().collect();
        assert_eq!(values.len(), 1, "{values:?}");
        assert_eq!(values[0], "from-betas");
    }

    #[test]
    fn goog_api_key_header_is_sent_once() {
        let req = built(
            r#"
schema_version = 1
id = "goog"
wire = "gemini"
base_url = "http://127.0.0.1"
auth_scheme = "header:x-goog-api-key"
[headers]
x-goog-api-key = "goog"
"#,
            Some("goog"),
        );
        let values: Vec<_> = req.headers().get_all("x-goog-api-key").iter().collect();
        assert_eq!(values.len(), 1, "{values:?}");
        assert_eq!(values[0], "goog");
    }
}

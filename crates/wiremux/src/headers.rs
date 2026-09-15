//! Shared profile header / auth_scheme / betas application.

use wiremux_auth::{AuthScheme, ResolvedProfile};

const GROK_BUILD_PROXY_HOST: &str = "cli-chat-proxy.grok.com";
const GROK_CLIENT_VERSION: &str = "0.1.202";
const GROK_CLIENT_IDENTIFIER: &str = "wiremux";

pub(crate) fn apply_profile_headers(
    mut req: reqwest::RequestBuilder,
    profile: &ResolvedProfile,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    for (name, value) in &profile.http.headers {
        if name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key") {
            continue;
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
    if !profile.betas.values.is_empty() {
        req = req.header(&profile.betas.header, profile.betas.values.join(","));
    }
    if let Some(token) = token {
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
}

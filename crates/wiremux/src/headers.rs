//! Shared profile header / auth_scheme / betas application.

use wiremux_auth::{AuthScheme, ResolvedProfile};

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

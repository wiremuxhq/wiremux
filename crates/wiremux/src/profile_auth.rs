//! Credential header names shared by status and request building.
//!
//! The `cli` feature does not enable `proxy`. `wiremux auth status` still
//! has to classify the same header names that the proxy and client send,
//! including `auth_scheme = "header:..."`.

use wiremux_auth::{AuthScheme, ResolvedProfile};

pub(crate) fn is_profile_auth_header(profile: &ResolvedProfile, name: &str) -> bool {
    if name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key") {
        return true;
    }
    matches!(
        &profile.http.auth_scheme,
        Some(AuthScheme::Header(header)) if name.eq_ignore_ascii_case(header)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremux_auth::parse_profile_str;

    fn profile(toml: &str) -> ResolvedProfile {
        parse_profile_str(toml).expect("profile")
    }

    #[test]
    fn authorization_and_x_api_key_are_credentials() {
        let profile = profile("schema_version = 1\nid = \"a\"\nwire = \"messages\"\n");
        assert!(is_profile_auth_header(&profile, "Authorization"));
        assert!(is_profile_auth_header(&profile, "X-API-Key"));
        assert!(!is_profile_auth_header(&profile, "anthropic-beta"));
    }

    #[test]
    fn custom_header_scheme_matches_that_name_only() {
        let profile = profile(
            "schema_version = 1\nid = \"a\"\nwire = \"gemini\"\nauth_scheme = \"header:x-goog-api-key\"\n",
        );
        assert!(is_profile_auth_header(&profile, "X-Goog-Api-Key"));
        assert!(!is_profile_auth_header(&profile, "x-goog-api-key-other"));
    }
}

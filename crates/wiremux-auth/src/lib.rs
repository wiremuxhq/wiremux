//! TokenProvider and profile catalog. Not ready.

mod profile;

pub use profile::{
    AuthScheme, Betas, CredsFormat, Dialect, ExpiresUnit, Fingerprint, ForbiddenFieldPolicy, Http,
    ListMerge, LoadOptions, Login, OauthPack, ProfileError, ResolvedProfile, SCHEMA_VERSION_MAX,
    StreamUnknownPolicy, TokenRequestFormat, TokenResponse, ToolNameCase, ToolTypePolicy, Wire,
    list_profiles, load_profile, load_profile_from_cli, parse_profile_str,
};

/// Crate version from Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}

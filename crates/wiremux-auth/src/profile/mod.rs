//! Profile AST, refuse scanners, and id-keyed catalog load.

mod envsubst;
mod error;
mod load;
mod overlay;
mod parse;
mod refuse;
mod shipped;
mod types;

pub use error::{ProfileError, not_found_message};
pub use load::{list_profiles, load_profile, load_profile_for_wire, load_profile_from_cli};
pub use parse::parse_profile_str;
#[cfg(any(test, feature = "test-util"))]
pub(crate) use refuse::is_loopback_http;
pub use types::{
    AuthScheme, Betas, CredsFormat, Dialect, ExpiresUnit, Fingerprint, ForbiddenFieldPolicy, Http,
    ListMerge, LoadOptions, Login, OauthPack, ResolvedProfile, SCHEMA_VERSION_MAX,
    StreamUnknownPolicy, TokenRequestFormat, TokenResponse, ToolNameCase, ToolTypePolicy, Wire,
};

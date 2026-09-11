//! Dialect maps. Not ready.

pub mod ir;
pub mod map;

pub use ir::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrStreamEvent, IrTool, IrToolChoice,
    LossAction, LossEvent, LossReport,
};
pub use map::{MapError, decode, encode};
pub use wiremux_auth::VERSION;
pub use wiremux_auth::{
    LoadOptions, ResolvedProfile, ToolTypePolicy, Wire, load_profile, parse_profile_str,
};

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}

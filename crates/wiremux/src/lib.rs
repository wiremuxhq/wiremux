//! Dialect maps. Not ready.

pub mod ir;
pub mod map;
pub mod stream;

pub use ir::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrStreamEvent, IrTool, IrToolChoice,
    LossAction, LossEvent, LossReport,
};
pub use map::{MapError, decode, encode};
pub use stream::{
    MAX_CONTENT_BLOCK_INDEX, MAX_TOOL_CALL_INDEX, RawSse, decode_stream_event, encode_stream_event,
};
pub use wiremux_auth::VERSION;
pub use wiremux_auth::{
    LoadOptions, ResolvedProfile, StreamUnknownPolicy, ToolTypePolicy, Wire, load_profile,
    parse_profile_str,
};

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}

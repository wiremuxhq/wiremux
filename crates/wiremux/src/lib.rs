//! Dialect maps. Not ready.

#[cfg(feature = "cli")]
pub mod cli;
pub mod ir;
pub mod map;
#[cfg(feature = "proxy")]
pub mod proxy;
pub mod stream;

pub use ir::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrStreamEvent, IrTool, IrToolChoice,
    LossAction, LossEvent, LossReport, estimate_prompt_tokens,
};
pub use map::{MapError, decode, encode};
pub use stream::{
    MAX_CONTENT_BLOCK_INDEX, MAX_SSE_PENDING, MAX_TOOL_CALL_INDEX, RawSse, SseFrameReader,
    ToolCallAssembler, decode_stream_event, decode_stream_events, encode_stream_event,
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

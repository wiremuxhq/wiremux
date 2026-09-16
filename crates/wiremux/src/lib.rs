//! Dialect maps. Not ready.

#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "client")]
pub mod client;
#[cfg(any(feature = "proxy", feature = "client"))]
mod headers;
#[cfg(feature = "cli")]
pub mod ingest;
pub mod ir;
pub mod map;
#[cfg(feature = "proxy")]
pub mod proxy;
pub mod stream;
#[cfg(any(feature = "cli", feature = "proxy", feature = "client"))]
mod upstream;

pub use ir::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrStreamEvent, IrTool, IrToolChoice,
    LossAction, LossEvent, LossReport, estimate_prompt_tokens,
};
pub use map::{MapError, decode, encode};
pub use stream::{
    EventStreamReader, MAX_CONTENT_BLOCK_INDEX, MAX_EVENTSTREAM_PENDING, MAX_SSE_PENDING,
    MAX_TOOL_CALL_INDEX, RawSse, SseFrameReader, ToolCallAssembler, decode_response,
    decode_stream_event, decode_stream_events, encode_eventstream_message, encode_response,
    encode_stream_event,
};
pub use wiremux_auth::VERSION;
pub use wiremux_auth::{
    LoadOptions, ResolvedProfile, StreamUnknownPolicy, ToolTypePolicy, Wire, load_profile,
    parse_profile_str,
};

#[cfg(feature = "client")]
pub use client::{ClientError, ListedModel, WireClient};

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}

//! Dialect maps for Chat Completions, Messages, Responses, Gemini, and Converse.
//!
//! Decode a vendor JSON body into [`IrRequest`], then encode another wire.
//! Maps-only hosts set `default-features = false`.
//!
//! ```
//! use wiremux::{Wire, decode, parse_profile_str};
//!
//! let src = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"ping"}]}"#;
//! let (ir, _loss) = decode(Wire::ChatCompletions, src).unwrap();
//! assert_eq!(ir.model, "gpt-4o");
//! let profile = parse_profile_str(
//!     "schema_version = 1\nid = \"example\"\nwire = \"messages\"\n",
//! )
//! .unwrap();
//! assert_eq!(profile.id, "example");
//! ```

#[cfg(any(feature = "proxy", feature = "client"))]
mod aws_creds;
#[cfg(any(feature = "proxy", feature = "client"))]
mod aws_sign;
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
    IrCache, IrDocumentSource, IrItem, IrPart, IrRequest, IrSampling, IrStreamEvent, IrTool,
    IrToolChoice, LossAction, LossEvent, LossReport, estimate_prompt_tokens,
};
pub use map::{MapError, decode, encode};
pub use stream::{
    EventStreamReader, MAX_CONTENT_BLOCK_INDEX, MAX_EVENTSTREAM_PENDING, MAX_SSE_PENDING,
    MAX_TOOL_CALL_INDEX, RawSse, SseFrameReader, StreamEncoder, ToolCallAssembler, decode_response,
    decode_stream_event, decode_stream_events, encode_eventstream_message, encode_response,
    encode_response_with_model, encode_stream_event,
};
pub use wiremux_auth::VERSION;
pub use wiremux_auth::{
    LoadOptions, ResolvedProfile, StreamUnknownPolicy, ToolTypePolicy, Wire, load_profile,
    parse_profile_str,
};

#[cfg(feature = "client")]
pub use client::{ClientError, ListedModel, TransientKind, WireClient};

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}

//! Gemini function-call ids stay on one decoder and reset for stateless decode.

use wiremux::{
    IrStreamEvent, RawSse, StreamDecoder, Wire, decode_stream_events, parse_profile_str,
};

fn gemini_profile() -> wiremux::ResolvedProfile {
    parse_profile_str(
        r#"
schema_version = 1
id = "gemini-seq"
wire = "gemini"
"#,
    )
    .expect("profile")
}

fn weather_chunk(city: &str) -> RawSse {
    RawSse {
        event: None,
        data: format!(
            r#"{{"candidates":[{{"content":{{"parts":[{{"functionCall":{{"name":"weather","args":{{"city":"{city}"}}}}}}]}}}}]}}"#
        ),
    }
}

fn call_id(events: &[IrStreamEvent]) -> String {
    events
        .iter()
        .find_map(|ev| match ev {
            IrStreamEvent::ToolCallStart { id, .. } => Some(id.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn gemini_decoder_advances_idless_function_calls() {
    let profile = gemini_profile();
    let mut decoder = StreamDecoder::new();
    let first = decoder
        .decode(Wire::Gemini, &weather_chunk("Paris"), &profile)
        .expect("first chunk");
    let second = decoder
        .decode(Wire::Gemini, &weather_chunk("Lyon"), &profile)
        .expect("second chunk");
    assert_eq!(call_id(&first), "weather#0", "{first:?}");
    assert_eq!(call_id(&second), "weather#1", "{second:?}");

    let stateless =
        decode_stream_events(Wire::Gemini, &weather_chunk("Nice"), &profile).expect("stateless");
    assert_eq!(call_id(&stateless), "weather#0", "{stateless:?}");
    let again = decode_stream_events(Wire::Gemini, &weather_chunk("Nice"), &profile)
        .expect("stateless again");
    assert_eq!(
        call_id(&again),
        "weather#0",
        "stateless decode must start at 0 on every call, got {again:?}"
    );
}

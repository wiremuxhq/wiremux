//! Decode Chat Completions JSON and encode Anthropic Messages.
//!
//! ```bash
//! cargo run -p wiremux --example remap --no-default-features
//! ```

use wiremux::{Wire, decode, encode, parse_profile_str};

fn main() {
    let src = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"ping"}]}"#;
    let (ir, decode_loss) = decode(Wire::ChatCompletions, src).expect("decode Chat Completions");
    let profile = parse_profile_str(
        r#"
schema_version = 1
id = "example-messages"
wire = "messages"
"#,
    )
    .expect("example profile");
    let (out, encode_loss) = encode(Wire::Messages, &ir, &profile).expect("encode Messages");
    let value: serde_json::Value = serde_json::from_slice(&out).expect("JSON body");
    println!(
        "{}",
        serde_json::to_string_pretty(&value).expect("pretty JSON")
    );
    if !decode_loss.events.is_empty() {
        eprintln!("decode loss: {decode_loss:?}");
    }
    if !encode_loss.events.is_empty() {
        eprintln!("encode loss: {encode_loss:?}");
    }
}

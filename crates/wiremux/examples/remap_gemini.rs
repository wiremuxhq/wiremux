//! Decode Gemini generateContent JSON and encode Chat Completions.
//!
//! ```bash
//! cargo run -p wiremux --example remap_gemini --no-default-features
//! ```
//!
//! Gemini `generationConfig.responseModalities` IMAGE has no Chat
//! `modalities` image slot and Drops. Gemini usage
//! `promptTokensDetails` IMAGE has no Chat `image_tokens` (same official
//! Drop). AUDIO next to IMAGE still remaps.

use wiremux::{LossAction, Wire, decode, encode, parse_profile_str};

fn main() {
    let src = br#"{
        "model": "gemini-2.5-flash",
        "contents": [{"role": "user", "parts": [{"text": "ping"}]}],
        "generationConfig": {"responseModalities": ["IMAGE"]}
    }"#;
    let (ir, decode_loss) = decode(Wire::Gemini, src).expect("decode Gemini");
    let profile = parse_profile_str(
        r#"
schema_version = 1
id = "example-chat"
wire = "chat-completions"
"#,
    )
    .expect("example profile");
    let (out, encode_loss) = encode(Wire::ChatCompletions, &ir, &profile).expect("encode Chat");
    let value: serde_json::Value = serde_json::from_slice(&out).expect("JSON body");
    println!(
        "{}",
        serde_json::to_string_pretty(&value).expect("pretty JSON")
    );
    print_loss("decode", &decode_loss);
    print_loss("encode", &encode_loss);
}

fn print_loss(label: &str, report: &wiremux::LossReport) {
    for event in &report.events {
        if event.action == LossAction::Preserve {
            continue;
        }
        eprintln!(
            "{label} {}: {:?} ({})",
            event.path, event.action, event.detail
        );
    }
}

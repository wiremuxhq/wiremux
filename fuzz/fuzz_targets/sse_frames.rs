#![no_main]

use libfuzzer_sys::fuzz_target;
use wiremux::SseFrameReader;

fuzz_target!(|data: &[u8]| {
    let mut reader = SseFrameReader::new();
    let _ = reader.feed(data);
    let _ = reader.drain();
});

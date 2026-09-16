#![no_main]

use libfuzzer_sys::fuzz_target;
use wiremux::EventStreamReader;

fuzz_target!(|data: &[u8]| {
    let mut reader = EventStreamReader::new();
    let _ = reader.feed(data);
    let _ = reader.drain();
});

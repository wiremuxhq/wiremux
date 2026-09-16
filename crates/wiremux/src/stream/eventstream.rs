//! AWS Event Stream (Bedrock `application/vnd.amazon.eventstream`).

use super::RawSse;

/// Hostile or buggy stream must not grow unbounded.
pub const MAX_EVENTSTREAM_PENDING: usize = 16 * 1024 * 1024;
const PRELUDE_LEN: usize = 12;
const CRC_LEN: usize = 4;

/// Incremental parser for Event Stream messages.
#[derive(Debug, Default)]
pub struct EventStreamReader {
    buffer: Vec<u8>,
}

impl EventStreamReader {
    /// Empty reader.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `bytes` and return complete messages.
    ///
    /// An exception later in the same chunk still yields the frames
    /// already parsed. The second value is that exception, after those
    /// frames have been returned and the exception bytes consumed.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(Vec<RawSse>, Option<String>), String> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_EVENTSTREAM_PENDING {
            self.buffer.clear();
            return Err(format!(
                "eventstream buffer exceeds {MAX_EVENTSTREAM_PENDING} bytes"
            ));
        }
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            match take_message(&self.buffer)? {
                None => return Ok((out, None)),
                Some(Take::Frame(msg, consumed)) => {
                    self.buffer.drain(..consumed);
                    out.push(msg.into_raw_sse());
                }
                Some(Take::Exception(err, consumed)) => {
                    self.buffer.drain(..consumed);
                    self.buffer.clear();
                    return Ok((out, Some(err)));
                }
            }
        }
    }

    /// No trailing partial message is a valid frame.
    pub fn drain(&mut self) -> Option<RawSse> {
        None
    }
}

struct EventStreamMessage {
    event_type: Option<String>,
    payload: Vec<u8>,
}

impl EventStreamMessage {
    fn into_raw_sse(self) -> RawSse {
        let data = wrap_event_payload(self.event_type.as_deref(), &self.payload);
        RawSse {
            event: self.event_type,
            data,
        }
    }
}

fn wrap_event_payload(event_type: Option<&str>, payload: &[u8]) -> String {
    let text = String::from_utf8_lossy(payload).into_owned();
    let Some(event_type) = event_type.filter(|name| !name.is_empty()) else {
        return text;
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(payload)
    else {
        return text;
    };
    if map.contains_key(event_type) {
        return text;
    }
    serde_json::json!({ event_type: serde_json::Value::Object(map) }).to_string()
}

enum Take {
    Frame(EventStreamMessage, usize),
    Exception(String, usize),
}

fn take_message(buf: &[u8]) -> Result<Option<Take>, String> {
    if buf.len() < PRELUDE_LEN {
        return Ok(None);
    }
    let total = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as usize;
    let headers_len = u32::from_be_bytes(buf[4..8].try_into().unwrap()) as usize;
    let prelude_crc = u32::from_be_bytes(buf[8..12].try_into().unwrap());
    if crc32_ieee(&buf[0..8]) != prelude_crc {
        return Err("eventstream prelude CRC mismatch".into());
    }
    if !(PRELUDE_LEN + CRC_LEN..=MAX_EVENTSTREAM_PENDING).contains(&total) {
        return Err(format!("eventstream total_length {total} is invalid"));
    }
    if buf.len() < total {
        return Ok(None);
    }
    let message_crc = u32::from_be_bytes(buf[total - CRC_LEN..total].try_into().unwrap());
    if crc32_ieee(&buf[..total - CRC_LEN]) != message_crc {
        return Err("eventstream message CRC mismatch".into());
    }
    let headers_start = PRELUDE_LEN;
    let headers_end = headers_start.saturating_add(headers_len);
    if headers_end > total - CRC_LEN {
        return Err("eventstream headers overflow payload".into());
    }
    let headers = parse_headers(&buf[headers_start..headers_end])?;
    let payload = buf[headers_end..total - CRC_LEN].to_vec();
    if headers.message_type.as_deref() == Some("exception") {
        return Ok(Some(Take::Exception(
            exception_error(
                headers.exception_type.as_deref().unwrap_or("unknown"),
                &payload,
            ),
            total,
        )));
    }
    Ok(Some(Take::Frame(
        EventStreamMessage {
            event_type: headers.event_type,
            payload,
        },
        total,
    )))
}

fn exception_error(exception_type: &str, payload: &[u8]) -> String {
    let message = serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(payload).into_owned());
    format!("eventstream exception {exception_type}: {message}")
}

struct ParsedHeaders {
    event_type: Option<String>,
    message_type: Option<String>,
    exception_type: Option<String>,
}

fn parse_headers(buf: &[u8]) -> Result<ParsedHeaders, String> {
    let mut i = 0;
    let mut event_type = None;
    let mut message_type = None;
    let mut exception_type = None;
    while i < buf.len() {
        let name_len = buf[i] as usize;
        i += 1;
        if i + name_len + 1 > buf.len() {
            return Err("eventstream header name truncated".into());
        }
        let name = std::str::from_utf8(&buf[i..i + name_len])
            .map_err(|_| "eventstream header name is not UTF-8")?;
        i += name_len;
        let ty = buf[i];
        i += 1;
        let value = read_header_value(ty, buf, &mut i)?;
        match name {
            ":event-type" => event_type = Some(value),
            ":message-type" => message_type = Some(value),
            ":exception-type" => exception_type = Some(value),
            _ => {}
        }
    }
    Ok(ParsedHeaders {
        event_type,
        message_type,
        exception_type,
    })
}

fn read_header_value(ty: u8, buf: &[u8], i: &mut usize) -> Result<String, String> {
    match ty {
        0 => Ok("true".into()),
        1 => Ok("false".into()),
        2 => take_bytes(buf, i, 1).map(|b| b[0].to_string()),
        3 => take_bytes(buf, i, 2).map(|b| i16::from_be_bytes(b.try_into().unwrap()).to_string()),
        4 => take_bytes(buf, i, 4).map(|b| i32::from_be_bytes(b.try_into().unwrap()).to_string()),
        5 => take_bytes(buf, i, 8).map(|b| i64::from_be_bytes(b.try_into().unwrap()).to_string()),
        6 => {
            let len = u16::from_be_bytes(take_bytes(buf, i, 2)?.try_into().unwrap()) as usize;
            let bytes = take_bytes(buf, i, len)?;
            Ok(format!("{bytes:?}"))
        }
        7 => {
            let len = u16::from_be_bytes(take_bytes(buf, i, 2)?.try_into().unwrap()) as usize;
            let bytes = take_bytes(buf, i, len)?;
            String::from_utf8(bytes.to_vec())
                .map_err(|_| "eventstream string header is not UTF-8".into())
        }
        8 => take_bytes(buf, i, 8).map(|b| i64::from_be_bytes(b.try_into().unwrap()).to_string()),
        9 => take_bytes(buf, i, 16).map(|b| {
            b.iter()
                .map(|x| format!("{x:02x}"))
                .collect::<Vec<_>>()
                .join("")
        }),
        other => Err(format!("eventstream unknown header type {other}")),
    }
}

fn take_bytes<'a>(buf: &'a [u8], i: &mut usize, n: usize) -> Result<&'a [u8], String> {
    if *i + n > buf.len() {
        return Err("eventstream header value truncated".into());
    }
    let slice = &buf[*i..*i + n];
    *i += n;
    Ok(slice)
}

/// Encode one Event Stream message (tests + remapping TO Bedrock).
pub fn encode_message(event_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut headers = Vec::new();
    write_string_header(&mut headers, ":message-type", "event");
    write_string_header(&mut headers, ":event-type", event_type);
    write_string_header(&mut headers, ":content-type", "application/json");
    encode_with_headers(&headers, payload)
}

/// Encode an AWS Event Stream exception frame (no `:event-type`).
pub fn encode_exception_message(exception_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut headers = Vec::new();
    write_string_header(&mut headers, ":message-type", "exception");
    write_string_header(&mut headers, ":exception-type", exception_type);
    write_string_header(&mut headers, ":content-type", "application/json");
    encode_with_headers(&headers, payload)
}

fn encode_with_headers(headers: &[u8], payload: &[u8]) -> Vec<u8> {
    let total = PRELUDE_LEN + headers.len() + payload.len() + CRC_LEN;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32_ieee(&out);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(headers);
    out.extend_from_slice(payload);
    let message_crc = crc32_ieee(&out);
    out.extend_from_slice(&message_crc.to_be_bytes());
    out
}

fn write_string_header(out: &mut Vec<u8>, name: &str, value: &str) {
    out.push(name.len() as u8);
    out.extend_from_slice(name.as_bytes());
    out.push(7);
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_content_block_delta() {
        let payload = br#"{"contentBlockDelta":{"delta":{"text":"hi"}}}"#;
        let bytes = encode_message("contentBlockDelta", payload);
        let mut reader = EventStreamReader::new();
        let (frames, err) = reader.feed(&bytes).expect("feed");
        assert!(err.is_none(), "{err:?}");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("contentBlockDelta"));
        assert!(frames[0].data.contains("\"text\":\"hi\""));
    }

    #[test]
    fn split_across_chunks() {
        let bytes = encode_message(
            "messageStop",
            br#"{"messageStop":{"stopReason":"end_turn"}}"#,
        );
        let mut reader = EventStreamReader::new();
        let mid = bytes.len() / 2;
        let (first, err) = reader.feed(&bytes[..mid]).expect("first");
        assert!(err.is_none());
        assert!(first.is_empty());
        let (second, err) = reader.feed(&bytes[mid..]).expect("second");
        assert!(err.is_none());
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].event.as_deref(), Some("messageStop"));
    }

    #[test]
    fn bad_crc_is_error() {
        let mut bytes = encode_message("metadata", br#"{}"#);
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        let mut reader = EventStreamReader::new();
        let err = reader.feed(&bytes).expect_err("crc");
        assert!(err.contains("CRC"), "{err}");
    }

    #[test]
    fn unwrapped_event_type_payload_is_wrapped() {
        let payload = br#"{"delta":{"text":"hi"},"contentBlockIndex":0}"#;
        let bytes = encode_message("contentBlockDelta", payload);
        let mut reader = EventStreamReader::new();
        let (frames, err) = reader.feed(&bytes).expect("feed");
        assert!(err.is_none());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("contentBlockDelta"));
        let value: serde_json::Value = serde_json::from_str(&frames[0].data).expect("json");
        assert_eq!(
            value
                .pointer("/contentBlockDelta/delta/text")
                .and_then(serde_json::Value::as_str),
            Some("hi"),
            "{}",
            frames[0].data
        );
    }

    #[test]
    fn exception_frame_is_feed_error() {
        let bytes = encode_exception_message(
            "validationException",
            br#"{"message":"The provided model identifier is invalid."}"#,
        );
        let mut reader = EventStreamReader::new();
        let (frames, err) = reader.feed(&bytes).expect("exception");
        assert!(frames.is_empty());
        let err = err.expect("exception");
        assert!(err.contains("validationException"), "{err}");
        assert!(
            err.contains("The provided model identifier is invalid."),
            "{err}"
        );
    }

    #[test]
    fn exception_then_delta_in_same_buffer_is_error() {
        let mut bytes =
            encode_exception_message("internalServerException", br#"{"message":"boom"}"#);
        bytes.extend_from_slice(&encode_message(
            "contentBlockDelta",
            br#"{"delta":{"text":"hi"},"contentBlockIndex":0}"#,
        ));
        let mut reader = EventStreamReader::new();
        let (frames, err) = reader.feed(&bytes).expect("exception first");
        assert!(frames.is_empty());
        let err = err.expect("exception first");
        assert!(err.contains("internalServerException"), "{err}");
        assert!(err.contains("boom"), "{err}");
        let (again, again_err) = reader.feed(&[]).expect("rest");
        assert!(
            again.is_empty() && again_err.is_none(),
            "must not emit a later contentBlockDelta as success: {again:?} {again_err:?}"
        );
    }

    #[test]
    fn good_frames_then_exception_in_same_chunk() {
        let mut bytes = encode_message(
            "contentBlockDelta",
            br#"{"contentBlockDelta":{"delta":{"text":"final"}}}"#,
        );
        bytes.extend_from_slice(&encode_exception_message(
            "modelStreamErrorException",
            br#"{"message":"cut"}"#,
        ));
        let mut reader = EventStreamReader::new();
        let (frames, err) = reader.feed(&bytes).expect("feed");
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert!(frames[0].data.contains("final"), "{}", frames[0].data);
        let err = err.expect("exception after frames");
        assert!(err.contains("modelStreamErrorException"), "{err}");
        assert!(err.contains("cut"), "{err}");
        assert!(reader.buffer.is_empty(), "exception bytes must be consumed");
    }
}

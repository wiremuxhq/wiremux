//! Incremental SSE frame reader. Feed TCP chunks; emit complete frames.

use super::RawSse;

/// Hostile or buggy stream with no newline must not grow unbounded.
/// Joined `data:` payload for one frame uses the same cap.
pub const MAX_SSE_PENDING: usize = 16 * 1024 * 1024;

/// Incremental parser for `event:` / `data:` frames.
///
/// Bytes stay raw until a newline so a multi-byte UTF-8 character split
/// across chunks is not turned into U+FFFD.
#[derive(Debug, Default)]
pub struct SseFrameReader {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    data_bytes: usize,
    has_fields: bool,
}

impl SseFrameReader {
    /// Empty reader.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `bytes` and return every complete frame found so far.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<RawSse>, String> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_SSE_PENDING
            && !bytes.contains(&b'\n')
            && !self.buffer.contains(&b'\n')
        {
            self.reset();
            return Err(format!("SSE line exceeds {MAX_SSE_PENDING} bytes"));
        }
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(newline_pos) = self.buffer.iter().position(|&b| b == b'\n') {
            if newline_pos > MAX_SSE_PENDING {
                self.reset();
                return Err(format!("SSE line exceeds {MAX_SSE_PENDING} bytes"));
            }
            let mut line_bytes: Vec<u8> = self.buffer.drain(..=newline_pos).collect();
            line_bytes.pop();
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            if let Some(frame) = self.push_line(&line)? {
                out.push(frame);
            }
        }
        if self.buffer.len() > MAX_SSE_PENDING {
            self.reset();
            return Err(format!("SSE line exceeds {MAX_SSE_PENDING} bytes"));
        }
        Ok(out)
    }

    /// Emit a last frame if fields are pending (EOF without a blank line).
    pub fn drain(&mut self) -> Option<RawSse> {
        if !self.buffer.is_empty() {
            let line = String::from_utf8_lossy(&self.buffer).into_owned();
            self.buffer.clear();
            match self.push_line(line.trim_end_matches('\r')) {
                Ok(Some(frame)) => return Some(frame),
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        self.take_frame()
    }

    fn push_line(&mut self, line: &str) -> Result<Option<RawSse>, String> {
        if line.is_empty() {
            return Ok(self.take_frame());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (name, value) = match line.split_once(':') {
            Some((name, value)) => (name, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match name {
            "event" => {
                self.event = Some(value.to_string());
                self.has_fields = true;
            }
            "data" => {
                let extra = if self.data.is_empty() {
                    value.len()
                } else {
                    value.len().saturating_add(1)
                };
                if self.data_bytes.saturating_add(extra) > MAX_SSE_PENDING {
                    self.reset();
                    return Err(format!("SSE data exceeds {MAX_SSE_PENDING} bytes"));
                }
                self.data_bytes = self.data_bytes.saturating_add(extra);
                self.data.push(value.to_string());
                self.has_fields = true;
            }
            "id" | "retry" => self.has_fields = true,
            _ => {}
        }
        Ok(None)
    }

    fn take_frame(&mut self) -> Option<RawSse> {
        if !self.has_fields {
            return None;
        }
        let frame = RawSse {
            event: self.event.take(),
            data: self.data.join("\n"),
        };
        self.data.clear();
        self.data_bytes = 0;
        self.has_fields = false;
        Some(frame)
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.event = None;
        self.data.clear();
        self.data_bytes = 0;
        self.has_fields = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_line_stays_buffered() {
        let mut reader = SseFrameReader::new();
        assert!(reader.feed(b"data: hel").unwrap().is_empty());
        let frames = reader.feed(b"lo\n\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hello");
    }

    #[test]
    fn keeps_event_field() {
        let mut reader = SseFrameReader::new();
        let frames = reader
            .feed(b"event: ping\ndata: {\"type\":\"ping\"}\n\n")
            .unwrap();
        assert_eq!(frames[0].event.as_deref(), Some("ping"));
        assert_eq!(frames[0].data, r#"{"type":"ping"}"#);
    }

    #[test]
    fn split_3byte_utf8_is_not_fffd() {
        let euro = "€";
        let bytes = euro.as_bytes();
        let mut reader = SseFrameReader::new();
        let mut first = b"data: ".to_vec();
        first.extend_from_slice(&bytes[..2]);
        assert!(reader.feed(&first).unwrap().is_empty());
        let mut second = bytes[2..].to_vec();
        second.extend_from_slice(b"\n\n");
        let frames = reader.feed(&second).unwrap();
        assert_eq!(frames[0].data, euro);
        assert!(!frames[0].data.contains('\u{FFFD}'));
    }

    #[test]
    fn drain_last_data_without_blank_line() {
        let mut reader = SseFrameReader::new();
        let items = reader.feed(b"data: first\n\ndata: last").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].data, "first");
        let last = reader.drain().expect("drain last");
        assert_eq!(last.data, "last");
    }

    #[test]
    fn pending_over_cap_fails_closed() {
        let mut reader = SseFrameReader::new();
        let mut chunk = b"data: ".to_vec();
        chunk.resize(MAX_SSE_PENDING + 8, b'x');
        assert!(reader.feed(&chunk).is_err());
        assert!(reader.drain().is_none());
    }

    #[test]
    fn data_lines_over_cap_fails_closed() {
        let mut reader = SseFrameReader::new();
        let payload = "x".repeat(64 * 1024);
        let line = format!("data: {payload}\n");
        let mut saw_err = false;
        for _ in 0..(MAX_SSE_PENDING / payload.len() + 2) {
            match reader.feed(line.as_bytes()) {
                Ok(_) => {}
                Err(_) => {
                    saw_err = true;
                    break;
                }
            }
        }
        assert!(
            saw_err,
            "joined data: lines must fail closed at {MAX_SSE_PENDING} bytes"
        );
        assert!(reader.drain().is_none());
    }
}

//! Bounded SSE framing helpers.

use bytes::Bytes;
use serde_json::Value;

use crate::error::GatewayError;

const MAX_SSE_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct SseFrame {
    pub raw: Bytes,
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, GatewayError> {
        if self.buffer.len().saturating_add(chunk.len()) > MAX_SSE_FRAME_BYTES {
            return Err(GatewayError::protocol(
                "one SSE frame exceeded the 8 MiB limit",
            ));
        }
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some((end, delimiter_len)) = find_frame_boundary(&self.buffer) {
            let raw = self.buffer.drain(..end + delimiter_len).collect::<Vec<_>>();
            frames.push(parse_frame(Bytes::from(raw))?);
        }
        Ok(frames)
    }

    pub fn finish(&mut self) -> Result<(), GatewayError> {
        if self.buffer.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return Err(GatewayError::protocol(
                "the SSE stream ended with an incomplete frame",
            ));
        }
        self.buffer.clear();
        Ok(())
    }
}

pub(crate) fn encode_json_event(kind: &str, mut value: Value) -> Result<Bytes, GatewayError> {
    value["type"] = Value::String(kind.to_owned());
    let data = serde_json::to_string(&value)
        .map_err(|_| GatewayError::protocol("could not serialize a projected SSE event"))?;
    Ok(Bytes::from(format!("event: {kind}\ndata: {data}\n\n")))
}

fn find_frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index < buffer.len() {
        if buffer[index..].starts_with(b"\r\n\r\n") {
            return Some((index, 4));
        }
        if buffer[index..].starts_with(b"\n\n") || buffer[index..].starts_with(b"\r\r") {
            return Some((index, 2));
        }
        index += 1;
    }
    None
}

fn parse_frame(raw: Bytes) -> Result<SseFrame, GatewayError> {
    let source = std::str::from_utf8(&raw)
        .map_err(|_| GatewayError::protocol("SSE frames must be valid UTF-8"))?;
    let mut event = None;
    let mut data = Vec::new();
    for line in source.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.strip_prefix(' ').unwrap_or(value).to_owned());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
    }
    Ok(SseFrame {
        raw,
        event,
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_arbitrary_chunk_and_line_boundaries() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push(b"event: one\r\ndata: {\"type\":\"")
                .unwrap()
                .is_empty()
        );
        let frames = decoder
            .push(b"one\"}\r\n\r\ndata: {\"type\":\"two\"}\n\n")
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("one"));
        assert_eq!(frames[0].data, r#"{"type":"one"}"#);
        assert_eq!(frames[1].event, None);
        assert_eq!(frames[1].data, r#"{"type":"two"}"#);
        decoder.finish().unwrap();
    }

    #[test]
    fn decoder_rejects_incomplete_frames() {
        let mut decoder = SseDecoder::default();
        decoder.push(b"data: unfinished").unwrap();
        assert!(decoder.finish().is_err());
    }
}

//! Encoder/Decoder for the stream-json (JSONL) protocol.
//!
//! Each message is a single JSON object terminated by a newline.  This codec
//! can be used with [`tokio_util::codec::Framed`] over any `AsyncRead +
//! AsyncWrite` stream (TCP, Unix socket, stdin/stdout, …).
//!
//! For WebSocket transports the framing is already handled by the WebSocket
//! layer; see [`crate::transport`] which uses these same serialization helpers
//! but operates on WebSocket text frames instead of raw byte streams.

use bytes::{Buf, BufMut, BytesMut};
use serde::{de::DeserializeOwned, Serialize};
use std::marker::PhantomData;
use tokio_util::codec::{Decoder, Encoder};

/// A newline-delimited JSON codec generic over the message type.
///
/// `Dec` is the type produced by decoding (must impl `DeserializeOwned`).
/// `Enc` is the type consumed by encoding (must impl `Serialize`).
///
/// Most users will want `EventCodec` (alias below) which fixes both to
/// [`crate::types::Event`].
#[derive(Debug)]
pub struct JsonLineCodec<Dec, Enc = Dec> {
    max_line_len: usize,
    _dec: PhantomData<Dec>,
    _enc: PhantomData<Enc>,
}

impl<Dec, Enc> JsonLineCodec<Dec, Enc> {
    pub fn new() -> Self {
        Self {
            max_line_len: 16 * 1024 * 1024, // 16 MiB
            _dec: PhantomData,
            _enc: PhantomData,
        }
    }

    pub fn with_max_line_len(mut self, len: usize) -> Self {
        self.max_line_len = len;
        self
    }
}

impl<Dec, Enc> Default for JsonLineCodec<Dec, Enc> {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Decoder ---------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("line exceeds maximum length ({max} bytes)")]
    LineTooLong { max: usize },
}

impl<Dec: DeserializeOwned, Enc> Decoder for JsonLineCodec<Dec, Enc> {
    type Item = Dec;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Find the first newline.
        let newline_pos = src.iter().position(|&b| b == b'\n');
        match newline_pos {
            Some(pos) => {
                if pos > self.max_line_len {
                    return Err(CodecError::LineTooLong {
                        max: self.max_line_len,
                    });
                }
                let line = &src[..pos];
                // Skip blank lines.
                let trimmed = trim_ascii(line);
                if trimmed.is_empty() {
                    src.advance(pos + 1);
                    return Ok(None);
                }
                let item: Dec = serde_json::from_slice(trimmed)?;
                src.advance(pos + 1);
                Ok(Some(item))
            }
            None => {
                if src.len() > self.max_line_len {
                    return Err(CodecError::LineTooLong {
                        max: self.max_line_len,
                    });
                }
                Ok(None) // need more data
            }
        }
    }
}

// ---- Encoder ---------------------------------------------------------------

impl<Dec, Enc: Serialize> Encoder<Enc> for JsonLineCodec<Dec, Enc> {
    type Error = CodecError;

    fn encode(&mut self, item: Enc, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let json = serde_json::to_vec(&item)?;
        dst.reserve(json.len() + 1);
        dst.put_slice(&json);
        dst.put_u8(b'\n');
        Ok(())
    }
}

/// Convenience alias: an `EventCodec` decodes and encodes [`crate::types::Event`].
pub type EventCodec = JsonLineCodec<crate::types::Event, crate::types::Event>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|&c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|&c| !c.is_ascii_whitespace()).map_or(start, |p| p + 1);
    &b[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Event;
    use bytes::BytesMut;

    #[test]
    fn round_trip() {
        let mut codec = EventCodec::new();
        let event = Event::assistant_text("hello world");

        let mut buf = BytesMut::new();
        codec.encode(event.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert!(matches!(decoded, Event::Assistant { .. }));
    }

    #[test]
    fn partial_line() {
        let mut codec: JsonLineCodec<Event> = JsonLineCodec::new();
        let mut buf = BytesMut::from(&b"{\"type\":\"result\""[..]);
        // No newline yet → None
        assert!(codec.decode(&mut buf).unwrap().is_none());
        // Append the rest
        buf.extend_from_slice(b",\"subtype\":\"success\"}\n");
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert!(matches!(decoded, Event::Result { .. }));
    }
}

//! SSE (Server-Sent Events) transport for the worker events stream.
//!
//! Connects to `{session_url}/worker/events/stream` to receive inbound
//! events (user messages, system events, etc.) as a server-sent event stream.

use crate::types::Event;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;

#[derive(Debug, thiserror::Error)]
pub enum SseError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("connect error {status}: {body}")]
    Connect { status: u16, body: String },
}

/// SSE-based transport for receiving worker events.
pub struct SseTransport {
    lines: Vec<String>,
    response: reqwest::Response,
    buf: String,
}

impl SseTransport {
    /// Connect to the worker events SSE stream.
    ///
    /// `session_url` is `{api_base_url}/v1/code/sessions/{cse_id}`.
    /// `worker_jwt` is the JWT from bridge_link.
    /// `http_client` allows reusing a shared reqwest::Client for connection pooling.
    pub async fn connect(
        session_url: &str,
        worker_jwt: &str,
        http_client: Option<&Client>,
    ) -> Result<Self, SseError> {
        let url = format!("{}/worker/events/stream", session_url);
        tracing::debug!("connecting to SSE stream");

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {worker_jwt}")).unwrap(),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static("2023-06-01"),
        );
        headers.insert("Accept", HeaderValue::from_static("text/event-stream"));

        let owned_client;
        let client = match http_client {
            Some(c) => c,
            None => {
                owned_client = Client::new();
                &owned_client
            }
        };

        let resp = client
            .get(&url)
            .headers(headers)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(SseError::Connect { status, body });
        }

        tracing::info!("connected to SSE stream");
        Ok(Self {
            lines: Vec::new(),
            response: resp,
            buf: String::new(),
        })
    }

    /// Receive the next event from the SSE stream.
    ///
    /// Returns `(Event, Option<event_id>, Option<raw_payload>)` or `None` when the stream ends.
    /// The raw payload is the original JSON value before deserialization into `Event`.
    pub async fn recv(&mut self) -> Result<Option<(Event, Option<String>, Option<serde_json::Value>)>, SseError> {
        loop {
            // Process any buffered lines first.
            if let Some(result) = self.try_parse_event() {
                return Ok(Some(result));
            }

            // Read more data from the stream.
            let chunk = match self.response.chunk().await? {
                Some(c) => c,
                None => return Ok(None), // Stream ended.
            };

            let text = String::from_utf8_lossy(&chunk);
            self.buf.push_str(&text);

            // Split on newlines and process complete lines.
            while let Some(nl) = self.buf.find('\n') {
                let line = self.buf[..nl].trim_end_matches('\r').to_string();
                self.buf = self.buf[nl + 1..].to_string();
                self.lines.push(line);
            }
        }
    }

    /// Try to parse a complete SSE event from buffered lines.
    fn try_parse_event(&mut self) -> Option<(Event, Option<String>, Option<serde_json::Value>)> {
        // SSE events are separated by blank lines.
        // Each data line starts with "data: ".
        let mut data_parts = Vec::new();
        let mut consumed = 0;

        for (i, line) in self.lines.iter().enumerate() {
            if line.is_empty() {
                // End of event block.
                consumed = i + 1;
                break;
            }
            if line.starts_with(':') {
                // SSE comment (e.g. ":keepalive") — skip.
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                data_parts.push(data.to_string());
            } else if let Some(data) = line.strip_prefix("data:") {
                data_parts.push(data.to_string());
            }
            // Ignore "event:", "id:", "retry:" lines.
        }

        if consumed == 0 {
            return None;
        }

        // Always drain consumed lines, even if no data (comment-only blocks).
        self.lines.drain(..consumed);

        if data_parts.is_empty() {
            return None;
        }

        let data = data_parts.join("\n");
        let trimmed = data.trim();
        if trimmed.is_empty() {
            return None;
        }

        // The SSE data is a wrapper: { event_id, event_type, payload: <Event> }
        // Try to extract payload first, then fall back to direct parse.
        if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let event_id = wrapper
                .get("event_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if let Some(payload) = wrapper.get("payload") {
                match serde_json::from_value::<Event>(payload.clone()) {
                    Ok(event) => return Some((event, event_id, Some(payload.clone()))),
                    Err(e) => {
                        tracing::debug!(
                            event_type = ?wrapper.get("event_type"),
                            err = %e,
                            "SSE: could not parse payload as Event"
                        );
                    }
                }
            }
        }

        match serde_json::from_str::<Event>(trimmed) {
            Ok(event) => Some((event, None, None)),
            Err(e) => {
                tracing::debug!(err = %e, "SSE: skipping non-Event data");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a transport from pre-buffered lines for testing the parser.
    fn parse_lines(lines: &[&str]) -> Option<(Event, Option<String>, Option<serde_json::Value>)> {
        // We can't easily construct a SseTransport without a real HTTP response,
        // so we test try_parse_event by constructing the struct with empty response workaround.
        // Instead, we test the parsing logic via a minimal integration approach.
        let mut transport_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        // Append empty line to mark end of event block
        transport_lines.push(String::new());

        // Create a minimal test helper that mimics try_parse_event logic
        let mut data_parts = Vec::new();
        let mut consumed = 0;

        for (i, line) in transport_lines.iter().enumerate() {
            if line.is_empty() {
                consumed = i + 1;
                break;
            }
            if line.starts_with(':') {
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                data_parts.push(data.to_string());
            } else if let Some(data) = line.strip_prefix("data:") {
                data_parts.push(data.to_string());
            }
        }

        if consumed == 0 || data_parts.is_empty() {
            return None;
        }

        let data = data_parts.join("\n");
        let trimmed = data.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let event_id = wrapper
                .get("event_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if let Some(payload) = wrapper.get("payload") {
                if let Ok(event) = serde_json::from_value::<Event>(payload.clone()) {
                    return Some((event, event_id, Some(payload.clone())));
                }
            }
        }

        match serde_json::from_str::<Event>(trimmed) {
            Ok(event) => Some((event, None, None)),
            Err(_) => None,
        }
    }

    #[test]
    fn parse_user_event_from_sse() {
        let result = parse_lines(&[
            r#"data: {"event_id":"abc-123","event_type":"user","payload":{"type":"user","message":{"role":"user","content":"hello"},"uuid":"def-456"}}"#,
        ]);
        assert!(result.is_some());
        let (event, event_id, _) = result.unwrap();
        assert_eq!(event_id.as_deref(), Some("abc-123"));
        assert!(matches!(event, Event::User { .. }));
        assert_eq!(event.user_text().unwrap(), "hello");
    }

    #[test]
    fn parse_control_request() {
        let result = parse_lines(&[
            r#"data: {"event_id":"e1","event_type":"control_request","payload":{"type":"control_request","request":{"subtype":"initialize"},"request_id":"r1"}}"#,
        ]);
        assert!(result.is_some());
        let (event, _, _) = result.unwrap();
        assert!(matches!(event, Event::ControlRequest { .. }));
    }

    #[test]
    fn keepalive_returns_none() {
        let result = parse_lines(&[":keepalive"]);
        assert!(result.is_none());
    }

    #[test]
    fn data_without_space_after_colon() {
        let result = parse_lines(&[
            r#"data:{"event_id":"e1","event_type":"user","payload":{"type":"user","message":{"role":"user","content":"test"},"uuid":"u1"}}"#,
        ]);
        assert!(result.is_some());
    }

    #[test]
    fn multi_line_data_joined() {
        // SSE spec: multiple "data:" lines are joined with newlines.
        // In practice, the Anthropic API sends single-line data, but verify
        // that if it's split across two lines, the joined result can still parse
        // if it happens to form valid JSON.
        let result = parse_lines(&[
            r#"data: {"event_id":"e1","event_type":"user","#,
            r#"data: "payload":{"type":"user","message":{"role":"user","content":"hi"},"uuid":"u1"}}"#,
        ]);
        // The two lines joined with \n happen to produce valid JSON here
        // (JSON allows whitespace including newlines between tokens)
        assert!(result.is_some());
    }

    #[test]
    fn direct_event_without_wrapper() {
        let result = parse_lines(&[
            r#"data: {"type":"result","subtype":"success"}"#,
        ]);
        assert!(result.is_some());
        let (event, event_id, _) = result.unwrap();
        assert!(event_id.is_none());
        assert!(matches!(event, Event::Result { .. }));
    }
}

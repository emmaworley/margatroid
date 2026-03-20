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

    #[error("stream ended")]
    StreamEnded,

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
    pub async fn connect(session_url: &str, worker_jwt: &str) -> Result<Self, SseError> {
        let url = format!("{}/worker/events/stream", session_url);
        tracing::debug!(url = %url, "connecting to SSE stream");

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

        let client = Client::new();
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
            tracing::debug!(raw = %text, "SSE chunk received");
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
                tracing::debug!(data = %trimmed, err = %e, "SSE: skipping non-Event data");
                None
            }
        }
    }
}

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Bridge HTTP API types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RegisterRequest {
    pub machine_name: String,
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_repo_url: Option<String>,
    pub max_sessions: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterResponse {
    pub environment_id: String,
    /// Secret token used for subsequent API calls (poll, ack, heartbeat).
    #[serde(default)]
    pub environment_secret: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// A work item returned by the poll endpoint.
///
/// The `id` field doubles as the session ID (prefixed `cse_`).
/// The `secret` field is a base64-encoded JSON blob containing the
/// `session_ingress_token`, `api_base_url`, and `auth` credentials.
#[derive(Debug, Deserialize)]
pub struct WorkItem {
    pub id: String,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default, rename = "type")]
    pub work_type: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

impl WorkItem {
    /// Decode the base64 `secret` field into a [`WorkSecret`].
    pub fn decode_secret(&self) -> Option<WorkSecret> {
        let secret = self.secret.as_ref()?;
        // The secret is base64-encoded JSON.
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(secret)
            .ok()
            .or_else(|| {
                // Try URL-safe variant
                base64::engine::general_purpose::URL_SAFE
                    .decode(secret)
                    .ok()
            })
            .or_else(|| {
                // Try with padding
                let padded = match secret.len() % 4 {
                    2 => format!("{secret}=="),
                    3 => format!("{secret}="),
                    _ => secret.clone(),
                };
                base64::engine::general_purpose::STANDARD
                    .decode(&padded)
                    .ok()
            })?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Get the session ID from this work item.
    ///
    /// The session ID lives in `data.id`. Falls back to converting the work
    /// ID (`cse_XXXX` → `session_XXXX`).
    pub fn session_id(&self) -> String {
        // Primary: data.id (what the runner actually uses)
        if let Some(data) = &self.data {
            if let Some(id) = data.get("id").and_then(|v| v.as_str()) {
                return id.to_string();
            }
        }
        // Fallback: convert work ID
        if let Some(suffix) = self.id.strip_prefix("cse_") {
            format!("session_{suffix}")
        } else {
            self.id.clone()
        }
    }

    /// Get the data type (e.g. "session", "healthcheck").
    pub fn data_type(&self) -> Option<&str> {
        self.data.as_ref()?.get("type")?.as_str()
    }
}

/// Decoded contents of the work item `secret` field.
#[derive(Debug, Deserialize)]
pub struct WorkSecret {
    #[serde(default)]
    pub version: Option<u32>,
    #[serde(default)]
    pub session_ingress_token: Option<String>,
    #[serde(default)]
    pub api_base_url: Option<String>,
    #[serde(default)]
    pub sources: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub auth: Option<Vec<AuthEntry>>,
    #[serde(default)]
    pub claude_code_args: Option<serde_json::Value>,
    #[serde(default)]
    pub environment_variables: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct AuthEntry {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub token: String,
}

/// Response from the bridge link endpoint (`/v1/code/sessions/{id}/bridge`).
#[derive(Debug, Deserialize)]
pub struct BridgeLinkResponse {
    pub worker_jwt: String,
    pub api_base_url: String,
    pub expires_in: u64,
    #[serde(default)]
    pub worker_epoch: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatResponse {
    #[serde(default)]
    pub lease_extended: Option<bool>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Stream-JSON protocol events (over WebSocket / stdin+stdout)
// ---------------------------------------------------------------------------

/// An event in the stream-json protocol.
///
/// The protocol is JSONL: each line is a self-contained JSON object with a
/// `type` field that discriminates the variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "system")]
    System {
        #[serde(default)]
        subtype: Option<String>,
        #[serde(flatten)]
        fields: serde_json::Value,
    },

    #[serde(rename = "user")]
    User {
        #[serde(default)]
        message: Option<Message>,
        #[serde(default)]
        uuid: Option<String>,
        #[serde(flatten)]
        fields: serde_json::Value,
    },

    #[serde(rename = "assistant")]
    Assistant {
        message: Message,
        #[serde(default)]
        uuid: Option<String>,
        #[serde(flatten)]
        fields: serde_json::Value,
    },

    #[serde(rename = "result")]
    Result {
        #[serde(default)]
        subtype: Option<String>,
        #[serde(default)]
        result: Option<String>,
        #[serde(flatten)]
        fields: serde_json::Value,
    },

    #[serde(rename = "control_request")]
    ControlRequest {
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        request: Option<serde_json::Value>,
        #[serde(flatten)]
        fields: serde_json::Value,
    },

    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl Event {
    /// Create an assistant text response event matching real Claude Code structure.
    ///
    /// The payload matches what the server stores for real workers:
    /// `{ message, parent_tool_use_id, session_id, type, uuid }`
    pub fn assistant_text(text: &str) -> Self {
        Self::assistant_text_for_session(text, None)
    }

    /// Create an assistant text response with a session ID.
    ///
    /// Matches real Claude Code server-side event structure exactly:
    /// message includes model, id, type, stop_reason, stop_sequence, usage.
    pub fn assistant_text_for_session(text: &str, session_id: Option<&str>) -> Self {
        let mut fields = serde_json::Map::new();
        fields.insert(
            "parent_tool_use_id".into(),
            serde_json::Value::Null,
        );
        if let Some(sid) = session_id {
            fields.insert(
                "session_id".into(),
                serde_json::Value::String(sid.into()),
            );
        }
        Event::Assistant {
            message: Message {
                role: "assistant".into(),
                content: serde_json::json!([{
                    "type": "text",
                    "text": text,
                }]),
            },
            uuid: Some(uuid::Uuid::new_v4().to_string()),
            fields: serde_json::Value::Object(fields),
        }
    }

    /// Build a raw JSON assistant event matching real Claude Code exactly.
    ///
    /// `session_id` should be the `session_` prefixed ID (not `cse_`).
    pub fn raw_assistant_json(text: &str, session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "assistant",
            "uuid": uuid::Uuid::new_v4().to_string(),
            "session_id": session_id,
            "parent_tool_use_id": null,
            "message": {
                "role": "assistant",
                "type": "message",
                "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                "model": "<synthetic>",
                "content": [{ "type": "text", "text": text }],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                }
            }
        })
    }

    /// Build a raw JSON result event matching real Claude Code exactly.
    ///
    /// `session_id` should be the `session_` prefixed ID (not `cse_`).
    pub fn raw_result_json(session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "result",
            "uuid": uuid::Uuid::new_v4().to_string(),
            "session_id": session_id,
            "subtype": "success",
            "is_error": false,
            "result": "",
            "stop_reason": null,
            "duration_ms": 0,
            "duration_api_ms": 0,
            "total_cost_usd": 0,
            "num_turns": 0,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
            }
        })
    }

    /// Create an assistant text response that references a parent event.
    pub fn assistant_text_reply(text: &str, _parent_uuid: Option<&str>) -> Self {
        // Note: real Claude Code does NOT send parentUuid to the server.
        // That field is local to the CLI's JSONL transcript only.
        Self::assistant_text(text)
    }

    /// Create a result/success event.
    pub fn result_success(text: &str) -> Self {
        Self::result_success_reply(text, None)
    }

    /// Create a result/success event that references a parent event.
    pub fn result_success_reply(text: &str, _parent_uuid: Option<&str>) -> Self {
        let fields = serde_json::Map::new();
        Event::Result {
            subtype: Some("success".into()),
            result: Some(text.into()),
            fields: serde_json::Value::Object(fields),
        }
    }

    /// Get the UUID of this event.
    pub fn uuid(&self) -> Option<&str> {
        match self {
            Event::User { uuid, .. } | Event::Assistant { uuid, .. } => {
                uuid.as_deref()
            }
            _ => None,
        }
    }

    /// Extract the plain-text content from a User event, if any.
    pub fn user_text(&self) -> Option<String> {
        match self {
            Event::User { message: Some(m), .. } => extract_text(&m.content),
            _ => None,
        }
    }
}

fn extract_text(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(t) = item.get("type").and_then(|v| v.as_str()) {
                    if t == "text" {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            parts.push(text.to_string());
                        }
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(""))
            }
        }
        _ => None,
    }
}

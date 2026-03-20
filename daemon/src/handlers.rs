//! Handle control requests from the bridge protocol.

use bridge::BridgeClient;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Handle a control_request event (e.g. initialize, set_model).
///
/// `session_id` is the `session_` prefixed ID for the response payload.
pub async fn handle_control_request(
    client: &Arc<Mutex<BridgeClient>>,
    session_url: &str,
    worker_jwt: &str,
    worker_epoch: &serde_json::Value,
    request_id: &str,
    request: Option<&serde_json::Value>,
    session_id: &str,
) {
    // Try request.type, then request.subtype, then parse from request_id prefix
    let request_type = request
        .and_then(|r| r.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let effective_type = if !request_type.is_empty() {
        request_type
    } else {
        let subtype = request
            .and_then(|r| r.get("subtype"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if !subtype.is_empty() {
            subtype
        } else if request_id.starts_with("set-model") || request_id.starts_with("set_model") {
            "set_model"
        } else {
            debug!(request_id, ?request, "control request with no type");
            ""
        }
    };

    info!(request_type = effective_type, request_id, "handling control request");

    match effective_type {
        "initialize" => {
            // Real CC format: type=control_response, nested response with request_id
            let response = serde_json::json!({
                "type": "control_response",
                "uuid": uuid::Uuid::new_v4().to_string(),
                "session_id": session_id,
                "response": {
                    "request_id": request_id,
                    "subtype": "success",
                    "response": {
                        "account": {},
                        "available_output_styles": ["normal"],
                        "commands": [],
                        "models": [],
                        "output_style": "normal",
                        "pid": 2,
                    }
                }
            });

            let c = client.lock().await;
            let _ = c
                .send_worker_events_raw(session_url, worker_jwt, worker_epoch, &[response])
                .await;
        }
        "set-model" | "set_model" => {
            let response = serde_json::json!({
                "type": "control_response",
                "uuid": uuid::Uuid::new_v4().to_string(),
                "session_id": session_id,
                "response": {
                    "request_id": request_id,
                    "subtype": "success",
                    "response": {
                        "model": request
                            .and_then(|r| r.get("model"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("claude-opus-4-6"),
                    }
                }
            });

            let c = client.lock().await;
            let _ = c
                .send_worker_events_raw(session_url, worker_jwt, worker_epoch, &[response])
                .await;
        }
        _ => {
            info!(effective_type, request_id, ?request, "unhandled control request");
        }
    }
}

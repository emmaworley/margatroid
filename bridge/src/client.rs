//! HTTP client for the bridge environment API.
//!
//! Handles registration, work polling, acknowledgement, heartbeat, and
//! session event sending.
//!
//! Auth per endpoint (reverse-engineered from the runner binary):
//!
//! | Endpoint          | Token                                        |
//! |-------------------|----------------------------------------------|
//! | register          | OAuth access token                           |
//! | poll              | environment_secret (from registration)        |
//! | ack               | session_ingress_token (from work secret)      |
//! | heartbeat         | session_ingress_token (from work secret)      |
//! | stop              | OAuth access token (with refresh on 401)      |
//! | create_session    | OAuth access token                           |
//! | deregister        | OAuth access token                           |

use crate::config::BridgeConfig;
use crate::types::*;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;

const BETA_HEADER: &str = "ccr-byoc-2025-07-29,environments-2025-11-01";
const API_VERSION: &str = "2023-06-01";
const RUNNER_VERSION: &str = "2.1.79";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },

    #[error("missing field in work item: {0}")]
    MissingField(&'static str),
}

/// Client for the bridge environment HTTP API.
pub struct BridgeClient {
    http: Client,
    config: BridgeConfig,
    /// The registered environment ID.
    environment_id: Option<String>,
    /// Secret returned by registration — used for poll auth.
    environment_secret: Option<String>,
}

impl BridgeClient {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            http: Client::new(),
            config,
            environment_id: None,
            environment_secret: None,
        }
    }

    pub fn environment_id(&self) -> Option<&str> {
        self.environment_id.as_deref()
    }

    pub fn org_uuid(&self) -> &str {
        &self.config.org_uuid
    }

    /// Re-read the OAuth access token from disk, updating the config in place.
    pub fn reload_access_token(&mut self) -> Result<(), crate::config::ConfigError> {
        self.config.reload_access_token()
    }

    /// Milliseconds until the OAuth access token expires.
    pub fn token_ttl_ms(&self) -> u64 {
        self.config.token_ttl_ms()
    }

    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// Return a reference to the shared HTTP client for reuse (e.g. SSE transport).
    pub fn http_client(&self) -> &Client {
        &self.http
    }

    // -----------------------------------------------------------------------
    // Registration (OAuth token)
    // -----------------------------------------------------------------------

    /// Register this process as a bridge environment.
    pub async fn register(&mut self, req: RegisterRequest) -> Result<RegisterResponse, ClientError> {
        let url = format!("{}/v1/environments/bridge", self.config.base_url);
        let resp = self
            .http
            .post(&url)
            .headers(self.full_headers(&self.config.access_token))
            .json(&req)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api { status, body });
        }

        let reg: RegisterResponse = resp.json().await?;
        self.environment_id = Some(reg.environment_id.clone());
        self.environment_secret = reg.environment_secret.clone();
        tracing::info!(
            env_id = %reg.environment_id,
            has_secret = self.environment_secret.is_some(),
            "registered"
        );
        Ok(reg)
    }

    // -----------------------------------------------------------------------
    // Work polling (environment_secret)
    // -----------------------------------------------------------------------

    /// Long-poll for the next work item. Returns `None` if the poll timed out
    /// with no work available.
    pub async fn poll_for_work(&self) -> Result<Option<WorkItem>, ClientError> {
        let env_id = self.env_id()?;
        let env_secret = self
            .environment_secret
            .as_deref()
            .ok_or(ClientError::MissingField("environment_secret (not registered)"))?;

        let url = format!(
            "{}/v1/environments/{}/work/poll",
            self.config.base_url, env_id
        );

        let resp = self
            .http
            .get(&url)
            .headers(self.full_headers(env_secret))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api { status, body });
        }

        // An empty body, 204, or JSON null means no work.
        let text = resp.text().await?;
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed == "null" {
            return Ok(None);
        }

        match serde_json::from_str::<WorkItem>(&text) {
            Ok(item) => Ok(Some(item)),
            Err(e) => {
                tracing::warn!(
                    err = %e,
                    body_len = text.len(),
                    "poll response was non-empty JSON but failed to parse as WorkItem"
                );
                Ok(None)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Acknowledge (session_ingress_token)
    // -----------------------------------------------------------------------

    /// Acknowledge receipt of a work item.
    ///
    /// `session_token` is the `session_ingress_token` from the decoded work secret.
    pub async fn acknowledge_work(
        &self,
        work_id: &str,
        session_token: &str,
    ) -> Result<(), ClientError> {
        let env_id = self.env_id()?;
        let url = format!(
            "{}/v1/environments/{}/work/{}/ack",
            self.config.base_url, env_id, work_id
        );

        let resp = self
            .http
            .post(&url)
            .headers(self.full_headers(session_token))
            .json(&serde_json::json!({}))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        check_status(resp).await
    }

    // -----------------------------------------------------------------------
    // Heartbeat (session_ingress_token)
    // -----------------------------------------------------------------------

    /// Send a heartbeat for an active work item.
    ///
    /// `session_token` is the `session_ingress_token` from the decoded work secret.
    pub async fn heartbeat_work(
        &self,
        work_id: &str,
        session_token: &str,
    ) -> Result<HeartbeatResponse, ClientError> {
        let env_id = self.env_id()?;
        let url = format!(
            "{}/v1/environments/{}/work/{}/heartbeat",
            self.config.base_url, env_id, work_id
        );

        let resp = self
            .http
            .post(&url)
            .headers(self.full_headers(session_token))
            .json(&serde_json::json!({}))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api { status, body });
        }

        Ok(resp.json().await?)
    }

    // -----------------------------------------------------------------------
    // Stop work (OAuth token)
    // -----------------------------------------------------------------------

    /// Signal that work processing is complete.
    pub async fn stop_work(&self, work_id: &str, force: bool) -> Result<(), ClientError> {
        let env_id = self.env_id()?;
        let url = format!(
            "{}/v1/environments/{}/work/{}/stop",
            self.config.base_url, env_id, work_id
        );

        let resp = self
            .http
            .post(&url)
            .headers(self.full_headers(&self.config.access_token))
            .json(&serde_json::json!({ "force": force }))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        check_status(resp).await
    }

    // -----------------------------------------------------------------------
    // Session creation (OAuth token)
    // -----------------------------------------------------------------------

    /// Create a new code session tied to this bridge environment.
    ///
    /// Uses `POST /v1/sessions` which generates a work item for the poll loop.
    pub async fn create_session(&self, title: &str) -> Result<String, ClientError> {
        let env_id = self.env_id()?;

        let url = format!("{}/v1/sessions", self.config.base_url);
        let body = serde_json::json!({
            "title": title,
            "events": [],
            "session_context": {
                "sources": [],
                "outcomes": [],
            },
            "environment_id": env_id,
            "source": "remote-control",
        });

        let mut h = self.full_headers(&self.config.access_token);
        h.insert("anthropic-beta", HeaderValue::from_static("ccr-byoc-2025-07-29"));

        let resp = self
            .http
            .post(&url)
            .headers(h)
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api { status, body });
        }

        let text = resp.text().await?;
        let data: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ClientError::Api { status: 0, body: format!("parse: {e}") })?;

        let session_id = data
            .get("session")
            .and_then(|s| s.get("id"))
            .and_then(|id| id.as_str())
            .or_else(|| data.get("id").and_then(|id| id.as_str()))
            .ok_or(ClientError::MissingField("id in create session response"))?
            .to_string();

        tracing::info!(session_id = %session_id, "created session");
        Ok(session_id)
    }

    // -----------------------------------------------------------------------
    // Bridge link (OAuth token) — returns worker_jwt
    // -----------------------------------------------------------------------

    /// Fetch worker credentials by linking to a code session.
    ///
    /// Uses the OAuth access token (same as `RtA` in the runner).
    /// Returns `worker_jwt`, `api_base_url`, `expires_in`, `worker_epoch`.
    pub async fn bridge_link(
        &self,
        session_id: &str,
    ) -> Result<BridgeLinkResponse, ClientError> {
        let url = format!(
            "{}/v1/code/sessions/{}/bridge",
            self.config.base_url, session_id
        );

        let resp = self
            .http
            .post(&url)
            .headers(self.simple_headers(&self.config.access_token))
            .json(&serde_json::json!({}))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api { status, body });
        }

        let link: BridgeLinkResponse = resp.json().await?;
        tracing::info!(
            api_base_url = %link.api_base_url,
            expires_in = link.expires_in,
            "bridge link successful"
        );
        Ok(link)
    }

    // -----------------------------------------------------------------------
    // Worker registration + events (worker_jwt)
    // -----------------------------------------------------------------------

    /// Register the worker with the session server.
    ///
    /// This is required before the SSE stream will deliver events.
    /// Corresponds to `PUT /worker { worker_status: "idle", worker_epoch }`.
    pub async fn register_worker(
        &self,
        session_url: &str,
        worker_jwt: &str,
        worker_epoch: &serde_json::Value,
    ) -> Result<(), ClientError> {
        self.set_worker_status(session_url, worker_jwt, worker_epoch, "idle")
            .await?;
        tracing::info!("worker registered (idle)");
        Ok(())
    }

    /// Update worker status to "processing".
    pub async fn worker_processing(
        &self,
        session_url: &str,
        worker_jwt: &str,
        worker_epoch: &serde_json::Value,
    ) -> Result<(), ClientError> {
        self.set_worker_status(session_url, worker_jwt, worker_epoch, "processing")
            .await
    }

    /// Set worker status (idle or processing).
    async fn set_worker_status(
        &self,
        session_url: &str,
        worker_jwt: &str,
        worker_epoch: &serde_json::Value,
        status: &str,
    ) -> Result<(), ClientError> {
        let url = format!("{}/worker", session_url);
        let body = serde_json::json!({
            "worker_status": status,
            "worker_epoch": worker_epoch,
        });

        let resp = self
            .http
            .put(&url)
            .headers(self.simple_headers(worker_jwt))
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        check_status(resp).await
    }

    /// Send raw JSON events to a session via the worker events API.
    ///
    /// Each event should have a `type` field. This method wraps each event
    /// in the `{ event_type, payload }` envelope format required by the API,
    /// and auto-generates a UUID if one isn't present.
    pub async fn send_worker_events_raw(
        &self,
        session_url: &str,
        worker_jwt: &str,
        worker_epoch: &serde_json::Value,
        events: &[serde_json::Value],
    ) -> Result<(), ClientError> {
        let url = format!("{}/worker/events", session_url);

        let wrapped: Vec<serde_json::Value> = events
            .iter()
            .map(|e| {
                let event_type = e
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let mut payload = e.clone();
                if payload.get("uuid").is_none() {
                    if let Some(o) = payload.as_object_mut() {
                        o.insert(
                            "uuid".into(),
                            serde_json::Value::String(uuid::Uuid::new_v4().to_string()),
                        );
                    }
                }
                serde_json::json!({
                    "event_type": event_type,
                    "payload": payload,
                })
            })
            .collect();

        let body = serde_json::json!({
            "worker_epoch": worker_epoch,
            "events": wrapped,
        });

        let resp = self
            .http
            .post(&url)
            .headers(self.simple_headers(worker_jwt))
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;

        check_status(resp).await
    }

    /// Acknowledge delivery of events to the server.
    pub async fn report_delivery(
        &self,
        session_url: &str,
        worker_jwt: &str,
        worker_epoch: &serde_json::Value,
        updates: &[(String, String)], // (event_id, status)
    ) -> Result<(), ClientError> {
        let url = format!("{}/worker/events/delivery", session_url);

        let updates_json: Vec<serde_json::Value> = updates
            .iter()
            .map(|(id, status)| {
                serde_json::json!({ "event_id": id, "status": status })
            })
            .collect();

        let body = serde_json::json!({
            "worker_epoch": worker_epoch,
            "updates": updates_json,
        });

        let resp = self
            .http
            .post(&url)
            .headers(self.simple_headers(worker_jwt))
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        check_status(resp).await
    }

    // -----------------------------------------------------------------------
    // Session archive + Deregister (OAuth token)
    // -----------------------------------------------------------------------

    /// Archive a session, removing it from the web UI.
    pub async fn archive_session(&self, session_id: &str) -> Result<(), ClientError> {
        let url = format!(
            "{}/v1/sessions/{}/archive",
            self.config.base_url, session_id
        );

        let resp = self
            .http
            .post(&url)
            .headers(self.full_headers(&self.config.access_token))
            .json(&serde_json::json!({}))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        check_status(resp).await
    }

    /// Deregister this bridge environment.
    pub async fn deregister(&self) -> Result<(), ClientError> {
        let env_id = self.env_id()?;
        let url = format!(
            "{}/v1/environments/bridge/{}",
            self.config.base_url, env_id
        );

        let resp = self
            .http
            .delete(&url)
            .headers(self.full_headers(&self.config.access_token))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        check_status(resp).await
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn env_id(&self) -> Result<&str, ClientError> {
        self.environment_id
            .as_deref()
            .ok_or(ClientError::MissingField("environment_id (not registered)"))
    }

    /// Build full headers with beta, runner version, and org UUID.
    /// Used for environment API endpoints (register, poll, ack, heartbeat, stop, archive, deregister).
    fn full_headers(&self, token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(
            "anthropic-version",
            HeaderValue::from_static(API_VERSION),
        );
        h.insert("anthropic-beta", HeaderValue::from_static(BETA_HEADER));
        h.insert(
            "x-environment-runner-version",
            HeaderValue::from_static(RUNNER_VERSION),
        );
        h.insert(
            "x-organization-uuid",
            HeaderValue::from_str(&self.config.org_uuid).unwrap(),
        );
        h
    }

    /// Build simple headers with just Bearer + anthropic-version.
    /// Used for worker endpoints (bridge_link, worker registration, events, delivery).
    fn simple_headers(&self, token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        h
    }
}

async fn check_status(resp: reqwest::Response) -> Result<(), ClientError> {
    let status = resp.status().as_u16();
    if status >= 400 {
        let body = resp.text().await.unwrap_or_default();
        return Err(ClientError::Api { status, body });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_headers_contains_required_fields() {
        let config = BridgeConfig {
            base_url: "https://api.anthropic.com".to_string(),
            access_token: "test-token".to_string(),
            org_uuid: "org-123".to_string(),
            expires_at_ms: None,
        };
        let client = BridgeClient::new(config);
        let h = client.full_headers("my-secret");

        assert_eq!(
            h.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            "Bearer my-secret"
        );
        assert_eq!(
            h.get("anthropic-version").unwrap().to_str().unwrap(),
            API_VERSION
        );
        assert_eq!(
            h.get("anthropic-beta").unwrap().to_str().unwrap(),
            BETA_HEADER
        );
        assert_eq!(
            h.get("x-environment-runner-version").unwrap().to_str().unwrap(),
            RUNNER_VERSION
        );
        assert_eq!(
            h.get("x-organization-uuid").unwrap().to_str().unwrap(),
            "org-123"
        );
    }

    #[test]
    fn simple_headers_has_no_beta() {
        let config = BridgeConfig {
            base_url: "https://api.anthropic.com".to_string(),
            access_token: "test-token".to_string(),
            org_uuid: "org-123".to_string(),
            expires_at_ms: None,
        };
        let client = BridgeClient::new(config);
        let h = client.simple_headers("worker-jwt");

        assert_eq!(
            h.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            "Bearer worker-jwt"
        );
        assert_eq!(
            h.get("anthropic-version").unwrap().to_str().unwrap(),
            API_VERSION
        );
        assert!(h.get("anthropic-beta").is_none());
        assert!(h.get("x-environment-runner-version").is_none());
        assert!(h.get("x-organization-uuid").is_none());
    }

    #[test]
    fn token_ttl_ms_delegates_to_config() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let config = BridgeConfig {
            base_url: "https://api.anthropic.com".to_string(),
            access_token: "test-token".to_string(),
            org_uuid: "org-123".to_string(),
            expires_at_ms: Some(now_ms + 60_000),
        };
        let client = BridgeClient::new(config);
        let ttl = client.token_ttl_ms();
        assert!(ttl > 55_000 && ttl <= 60_000, "ttl={ttl}");
    }

    #[test]
    fn token_ttl_ms_zero_when_no_expiry() {
        let config = BridgeConfig {
            base_url: "https://api.anthropic.com".to_string(),
            access_token: "test-token".to_string(),
            org_uuid: "org-123".to_string(),
            expires_at_ms: None,
        };
        let client = BridgeClient::new(config);
        assert_eq!(client.token_ttl_ms(), 0);
    }
}

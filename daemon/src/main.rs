#![deny(warnings)]

mod commands;
mod handlers;

use bridge::types::RegisterRequest;
use bridge::{BridgeClient, BridgeConfig, Event};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// How far in advance (in seconds) of JWT expiry to refresh the bridge link.
const JWT_REFRESH_MARGIN_SECS: u64 = 300; // 5 minutes

/// Refresh the OAuth access token when less than this many ms remain.
const OAUTH_REFRESH_MARGIN_MS: u64 = 30 * 60 * 1000; // 30 minutes

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("loading bridge config");
    let config = BridgeConfig::from_default_files()?;
    let client = Arc::new(Mutex::new(BridgeClient::new(config)));

    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    // Before the first API call, make sure the OAuth access token on disk is
    // fresh. The daemon previously crash-looped overnight because the token
    // would expire, register() would fail with 401, main() would return, and
    // systemd would restart it — but the in-loop refresh logic never got to
    // run. Refresh proactively here so startup survives an expired token.
    ensure_fresh_oauth_token(&client).await;

    // Register as bridge environment. If the first attempt returns 401 (e.g.
    // the proactive refresh above failed or raced), force a refresh and retry
    // once before giving up.
    info!(machine = %hostname, "registering bridge environment");
    register_environment(&client, &hostname).await?;

    // Create session with a durable, recognizable name
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let session_name = format!("{user}@{hostname} Session Manager");
    info!(name = %session_name, "creating session");
    let mut manager_session_id = {
        let c = client.lock().await;
        match c.create_session(&session_name).await {
            Ok(id) => {
                info!(session_id = %id, "session created");
                Some(id)
            }
            Err(e) => {
                warn!(err = %e, "failed to create session");
                None
            }
        }
    };

    // Set up shutdown signal (handle both SIGINT and SIGTERM)
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let shutdown_tx = shutdown.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("received SIGINT");
            shutdown_tx.store(true, Ordering::SeqCst);
        });
    }
    {
        let shutdown_tx = shutdown.clone();
        tokio::spawn(async move {
            let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");
            sig.recv().await;
            info!("received SIGTERM");
            shutdown_tx.store(true, Ordering::SeqCst);
        });
    }

    let mut last_oauth_check = std::time::Instant::now();

    // Main poll loop
    loop {
        // Check shutdown before polling
        if shutdown.load(Ordering::SeqCst) {
            do_shutdown(&client, &manager_session_id).await;
            return Ok(());
        }

        // Periodically check if the OAuth access token needs refreshing.
        if last_oauth_check.elapsed() >= std::time::Duration::from_secs(60) {
            last_oauth_check = std::time::Instant::now();
            ensure_fresh_oauth_token(&client).await;
        }

        info!("polling for work...");

        let item = {
            let c = client.lock().await;
            match c.poll_for_work().await {
                Ok(Some(item)) => item,
                Ok(None) => {
                    drop(c);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
                Err(e) => {
                    error!(err = %e, "poll error");
                    drop(c);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            }
        };

        // Skip healthcheck work items
        if item.data_type() == Some("healthcheck") {
            info!("healthcheck — skipping");
            continue;
        }

        // Decode work secret
        let secret = match item.decode_secret() {
            Some(s) => s,
            None => {
                warn!("could not decode work item secret");
                continue;
            }
        };

        let cse_id = item.session_id();
        // session_ prefixed ID for event payloads (what the web UI expects)
        let session_id = if let Some(suffix) = cse_id.strip_prefix("cse_") {
            format!("session_{suffix}")
        } else {
            cse_id.clone()
        };
        let ingress_token = match &secret.session_ingress_token {
            Some(t) => t.clone(),
            None => {
                warn!("no session_ingress_token in work secret");
                continue;
            }
        };

        // Acknowledge work
        info!(session_id = %cse_id, "acknowledging work");
        {
            let c = client.lock().await;
            if let Err(e) = c.acknowledge_work(&item.id, &ingress_token).await {
                warn!(err = %e, "failed to ack work");
                continue;
            }
        }

        // Get bridge credentials
        let link = {
            let c = client.lock().await;
            match c.bridge_link(&cse_id).await {
                Ok(link) => link,
                Err(e) => {
                    error!(err = %e, "bridge_link failed");
                    continue;
                }
            }
        };

        let session_url = format!(
            "{}/v1/code/sessions/{}",
            link.api_base_url.trim_end_matches('/'),
            cse_id
        );
        let worker_jwt = Arc::new(Mutex::new(link.worker_jwt.clone()));
        let worker_epoch = link.worker_epoch.clone().unwrap_or(serde_json::json!(0));
        let mut jwt_obtained_at = std::time::Instant::now();
        let mut jwt_expires_in = link.expires_in;

        // Start heartbeat task with its own HTTP client to avoid lock contention.
        let hb_http = reqwest::Client::new();
        let (hb_base_url, hb_env_id, hb_org_uuid) = {
            let c = client.lock().await;
            (
                c.base_url().to_string(),
                c.environment_id().unwrap_or("").to_string(),
                c.org_uuid().to_string(),
            )
        };
        let hb_work_id = item.id.clone();
        let hb_token = ingress_token.clone();
        let hb_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                interval.tick().await;
                if let Err(e) = send_heartbeat(
                    &hb_http,
                    &hb_base_url,
                    &hb_env_id,
                    &hb_org_uuid,
                    &hb_work_id,
                    &hb_token,
                )
                .await
                {
                    warn!(err = %e, "heartbeat failed");
                    break;
                }
            }
        });

        // Register worker as idle
        {
            let c = client.lock().await;
            let jwt = worker_jwt.lock().await;
            if let Err(e) = c
                .register_worker(&session_url, &jwt, &worker_epoch)
                .await
            {
                error!(err = %e, "register_worker failed");
                hb_handle.abort();
                continue;
            }
        }

        // Connect SSE stream (share the HTTP client for connection pooling)
        info!(session_url = %session_url, "connecting to SSE stream");
        let sse_jwt = worker_jwt.lock().await.clone();
        let http_client = {
            let c = client.lock().await;
            c.http_client().clone()
        };
        let mut sse = match bridge::SseTransport::connect(&session_url, &sse_jwt, Some(&http_client)).await {
            Ok(s) => s,
            Err(e) => {
                error!(err = %e, "SSE connect failed");
                hb_handle.abort();
                continue;
            }
        };

        // Send welcome message on first connect
        {
            let jwt = worker_jwt.lock().await.clone();
            send_welcome(&client, &session_url, &jwt, &worker_epoch, &hostname, &session_id).await;
        }

        // Event loop — race SSE recv against periodic shutdown check
        loop {
            if shutdown.load(Ordering::SeqCst) {
                info!("shutting down SSE loop");
                break;
            }

            // Check if JWT needs refresh (before expiry minus margin)
            let elapsed = jwt_obtained_at.elapsed().as_secs();
            if jwt_expires_in > JWT_REFRESH_MARGIN_SECS
                && elapsed >= jwt_expires_in - JWT_REFRESH_MARGIN_SECS
            {
                info!("worker_jwt approaching expiry, refreshing bridge link");
                let c = client.lock().await;
                match c.bridge_link(&cse_id).await {
                    Ok(new_link) => {
                        let mut jwt = worker_jwt.lock().await;
                        *jwt = new_link.worker_jwt;
                        jwt_obtained_at = std::time::Instant::now();
                        jwt_expires_in = new_link.expires_in;
                        info!(expires_in = jwt_expires_in, "worker_jwt refreshed");
                    }
                    Err(e) => {
                        warn!(err = %e, "failed to refresh bridge link");
                    }
                }
            }

            // Use a timeout so we can check shutdown periodically
            let recv = tokio::select! {
                result = sse.recv() => {
                    match result {
                        Ok(Some((event, event_id, _raw_payload))) => (event, event_id),
                        Ok(None) => {
                            info!("SSE stream ended");
                            break;
                        }
                        Err(e) => {
                            error!(err = %e, "SSE recv error");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                    continue; // loop back to check shutdown flag
                }
            };

            let (event, event_id) = recv;
            let current_jwt = worker_jwt.lock().await.clone();

            // Report received + processed immediately so the server can
            // deliver the event to WebSocket subscribers right away.
            if let Some(ref eid) = event_id {
                let c = client.lock().await;
                let _ = c
                    .report_delivery(
                        &session_url,
                        &current_jwt,
                        &worker_epoch,
                        &[(eid.clone(), "received".to_string())],
                    )
                    .await;
                let _ = c
                    .report_delivery(
                        &session_url,
                        &current_jwt,
                        &worker_epoch,
                        &[(eid.clone(), "processed".to_string())],
                    )
                    .await;
            }

            match &event {
                Event::ControlRequest {
                    request_id,
                    request,
                    ..
                } => {
                    handlers::handle_control_request(
                        &client,
                        &session_url,
                        &current_jwt,
                        &worker_epoch,
                        request_id.as_deref().unwrap_or(""),
                        request.as_ref(),
                        &session_id,
                    )
                    .await;
                }
                Event::User { message, .. } => {
                    let text = match message {
                        Some(_) => event.user_text().unwrap_or_default(),
                        None => String::new(),
                    };

                    info!(text = %text, "received user message");

                    // Set worker to processing
                    {
                        let c = client.lock().await;
                        let _ = c
                            .worker_processing(&session_url, &current_jwt, &worker_epoch)
                            .await;
                    }

                    let response = commands::handle_command(&text);

                    // Send assistant + result via worker API
                    let response_json = Event::raw_assistant_json(
                        &response,
                        &session_id,
                    );
                    let result_json = Event::raw_result_json(&session_id);

                    {
                        let c = client.lock().await;
                        if let Err(e) = c
                            .send_worker_events_raw(
                                &session_url,
                                &current_jwt,
                                &worker_epoch,
                                &[response_json, result_json],
                            )
                            .await
                        {
                            error!(err = %e, "failed to send response events");
                        }
                    }

                    // Set worker back to idle
                    {
                        let c = client.lock().await;
                        let _ = c
                            .register_worker(&session_url, &current_jwt, &worker_epoch)
                            .await;
                    }
                }
                _ => {}
            }
        }

        hb_handle.abort();

        // Stop work
        {
            let c = client.lock().await;
            let _ = c.stop_work(&item.id, false).await;
        }

        // If shutdown was signaled, archive session and deregister
        if shutdown.load(Ordering::SeqCst) {
            do_shutdown(&client, &manager_session_id).await;
            return Ok(());
        }

        // SSE stream dropped — archive the old session and create a fresh one
        // so the poll loop picks up a new work item.
        {
            let c = client.lock().await;
            if let Some(ref sid) = manager_session_id {
                if let Err(e) = c.archive_session(sid).await {
                    warn!(err = %e, "failed to archive old session");
                } else {
                    info!(session_id = %sid, "archived old session after SSE drop");
                }
            }
            match c.create_session(&session_name).await {
                Ok(id) => {
                    info!(session_id = %id, "created replacement session");
                    manager_session_id = Some(id);
                }
                Err(e) => {
                    error!(err = %e, "failed to create replacement session — exiting for restart");
                    do_shutdown(&client, &None).await;
                    return Ok(());
                }
            }
        }
    }
}

/// Send a heartbeat using a dedicated HTTP client (no lock contention).
async fn send_heartbeat(
    http: &reqwest::Client,
    base_url: &str,
    env_id: &str,
    org_uuid: &str,
    work_id: &str,
    session_token: &str,
) -> Result<(), String> {
    let url = format!("{}/v1/environments/{}/work/{}/heartbeat", base_url, env_id, work_id);

    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {session_token}")).unwrap(),
    );
    h.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    h.insert(
        "anthropic-version",
        reqwest::header::HeaderValue::from_static("2023-06-01"),
    );
    h.insert(
        "anthropic-beta",
        reqwest::header::HeaderValue::from_static("ccr-byoc-2025-07-29,environments-2025-11-01"),
    );
    h.insert(
        "x-environment-runner-version",
        reqwest::header::HeaderValue::from_static("2.1.79"),
    );
    h.insert(
        "x-organization-uuid",
        reqwest::header::HeaderValue::from_str(org_uuid).unwrap(),
    );

    let resp = http
        .post(&url)
        .headers(h)
        .json(&serde_json::json!({}))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("heartbeat HTTP error: {e}"))?;

    let status = resp.status().as_u16();
    if status >= 400 {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("heartbeat {status}: {body}"));
    }
    Ok(())
}

/// Graceful shutdown: archive session and deregister environment.
async fn do_shutdown(
    client: &Arc<Mutex<BridgeClient>>,
    manager_session_id: &Option<String>,
) {
    let c = client.lock().await;
    if let Some(ref sid) = manager_session_id {
        match c.archive_session(sid).await {
            Ok(()) => info!(session_id = %sid, "session archived"),
            Err(e) => warn!(err = %e, "failed to archive session"),
        }
    }
    match c.deregister().await {
        Ok(()) => info!("environment deregistered"),
        Err(e) => warn!(err = %e, "failed to deregister"),
    }
}

/// Send a welcome message when the daemon first connects to a session.
async fn send_welcome(
    client: &Arc<Mutex<BridgeClient>>,
    session_url: &str,
    worker_jwt: &str,
    worker_epoch: &serde_json::Value,
    hostname: &str,
    session_id: &str,
) {
    let sessions = margatroid::session::list_all().unwrap_or_default();
    let running = sessions
        .iter()
        .filter(|s| s.status == margatroid::session::SessionStatus::Running)
        .count();
    let stopped = sessions
        .iter()
        .filter(|s| s.status == margatroid::session::SessionStatus::Stopped)
        .count();

    let welcome = if sessions.is_empty() {
        format!(
            "## {hostname} Session Manager\n\n\
             No sessions found. Get started:\n\n\
             - `/start <name> <--image=IMAGE|--host>` — create a new session\n\
             - `/help` — see all commands"
        )
    } else {
        format!(
            "## {hostname} Session Manager\n\n\
             **{running}** running, **{stopped}** stopped ({} total)\n\n\
             - `/list` — show all sessions\n\
             - `/start <name> <--image=IMAGE|--host>` — create a new session\n\
             - `/help` — see all commands",
            sessions.len()
        )
    };

    // Set to processing, send welcome + result, set back to idle
    {
        let c = client.lock().await;
        let _ = c
            .worker_processing(session_url, worker_jwt, worker_epoch)
            .await;
    }

    let response_json = Event::raw_assistant_json(&welcome, session_id);
    let result_json = Event::raw_result_json(session_id);
    {
        let c = client.lock().await;
        if let Err(e) = c
            .send_worker_events_raw(
                session_url,
                worker_jwt,
                worker_epoch,
                &[response_json, result_json],
            )
            .await
        {
            warn!(err = %e, "failed to send welcome message");
        }
    }

    {
        let c = client.lock().await;
        let _ = c
            .register_worker(session_url, worker_jwt, worker_epoch)
            .await;
    }
}

/// Reload the OAuth access token from disk and, if it is within the refresh
/// margin of expiry (or already expired), shell out to `claude` to refresh it
/// and reload again.
///
/// Safe to call repeatedly — this is a no-op when the on-disk token is fresh.
async fn ensure_fresh_oauth_token(client: &Arc<Mutex<BridgeClient>>) {
    let mut c = client.lock().await;
    // Re-read from disk first (another process may have refreshed it).
    if let Err(e) = c.reload_access_token() {
        warn!(err = %e, "failed to reload access token from disk");
    }
    let ttl = c.token_ttl_ms();
    if ttl >= OAUTH_REFRESH_MARGIN_MS {
        return;
    }
    info!(ttl_ms = ttl, "OAuth token near/past expiry, triggering refresh");
    drop(c);
    refresh_oauth_token().await;
    let mut c = client.lock().await;
    if let Err(e) = c.reload_access_token() {
        warn!(err = %e, "failed to reload access token after refresh");
    } else {
        info!(ttl_ms = c.token_ttl_ms(), "OAuth token reloaded after refresh");
    }
}

/// Register as a bridge environment. Retries once with a forced token refresh
/// if the first attempt returns 401 — otherwise returns the error.
async fn register_environment(
    client: &Arc<Mutex<BridgeClient>>,
    hostname: &str,
) -> anyhow::Result<()> {
    let make_req = || RegisterRequest {
        machine_name: hostname.to_string(),
        directory: std::env::var("HOME").unwrap_or_else(|_| "/home/claude".into()),
        branch: None,
        git_repo_url: None,
        max_sessions: 1,
        metadata: Some(serde_json::json!({ "worker_type": "margatroid-daemon" })),
        environment_id: None,
    };

    let first = {
        let mut c = client.lock().await;
        c.register(make_req()).await
    };
    match first {
        Ok(_) => Ok(()),
        Err(bridge::client::ClientError::Api { status: 401, body }) => {
            warn!(body = %body, "register returned 401, forcing token refresh and retrying");
            refresh_oauth_token().await;
            {
                let mut c = client.lock().await;
                if let Err(e) = c.reload_access_token() {
                    warn!(err = %e, "failed to reload access token after forced refresh");
                }
            }
            let mut c = client.lock().await;
            c.register(make_req()).await?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Shell out to `claude` to trigger its internal OAuth token refresh.
///
/// Claude Code refreshes `~/.claude/.credentials.json` when the token is
/// near-expiry. We make a trivial API call so it goes through its auth flow,
/// then the daemon can re-read the refreshed token from disk.
async fn refresh_oauth_token() {
    info!("invoking claude to refresh OAuth token");
    match tokio::process::Command::new("claude")
        .args(["-p", "hi", "--max-turns", "1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
    {
        Ok(status) => {
            if status.success() {
                info!("claude token refresh invocation succeeded");
            } else {
                warn!(code = ?status.code(), "claude token refresh exited non-zero");
            }
        }
        Err(e) => {
            warn!(err = %e, "failed to invoke claude for token refresh");
        }
    }
}

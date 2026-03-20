#![deny(warnings)]

mod commands;
mod handlers;

use bridge::types::RegisterRequest;
use bridge::{BridgeClient, BridgeConfig, Event};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

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

    // Register as bridge environment
    info!(machine = %hostname, "registering bridge environment");
    {
        let mut c = client.lock().await;
        c.register(RegisterRequest {
            machine_name: hostname.clone(),
            directory: std::env::var("HOME").unwrap_or_else(|_| "/home/claude".into()),
            branch: None,
            git_repo_url: None,
            max_sessions: 1,
            metadata: Some(serde_json::json!({ "worker_type": "orchestrator-daemon" })),
            environment_id: None,
        })
        .await?;
    }

    // Create session with a durable, recognizable name
    let session_name = format!("{hostname} Session Manager");
    info!(name = %session_name, "creating session");
    let manager_session_id = {
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

    // Main poll loop
    loop {
        info!("polling for work...");
        // Check shutdown before polling
        if shutdown.load(Ordering::SeqCst) {
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
            return Ok(());
        }

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
        let worker_jwt = link.worker_jwt.clone();
        let worker_epoch = link.worker_epoch.clone().unwrap_or(serde_json::json!(0));

        // Start heartbeat task
        let hb_client = client.clone();
        let hb_work_id = item.id.clone();
        let hb_token = ingress_token.clone();
        let hb_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                interval.tick().await;
                let c = hb_client.lock().await;
                if let Err(e) = c.heartbeat_work(&hb_work_id, &hb_token).await {
                    warn!(err = %e, "heartbeat failed");
                    break;
                }
            }
        });

        // Register worker as idle
        {
            let c = client.lock().await;
            if let Err(e) = c
                .register_worker(&session_url, &worker_jwt, &worker_epoch)
                .await
            {
                error!(err = %e, "register_worker failed");
                hb_handle.abort();
                continue;
            }
        }

        // Connect SSE stream
        info!(session_url = %session_url, "connecting to SSE stream");
        let mut sse = match bridge::SseTransport::connect(&session_url, &worker_jwt).await {
            Ok(s) => s,
            Err(e) => {
                error!(err = %e, "SSE connect failed");
                hb_handle.abort();
                continue;
            }
        };

        // Send welcome message on first connect
        send_welcome(&client, &session_url, &worker_jwt, &worker_epoch, &hostname, &session_id).await;

        // Event loop — race SSE recv against periodic shutdown check
        loop {
            if shutdown.load(Ordering::SeqCst) {
                info!("shutting down SSE loop");
                break;
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

            // Report received + processed immediately so the server can
            // deliver the event to WebSocket subscribers right away.
            // Previously, "processed" was reported AFTER handling, which
            // may have blocked WebSocket delivery of the user event until
            // after we'd already sent our response.
            if let Some(ref eid) = event_id {
                let c = client.lock().await;
                let _ = c
                    .report_delivery(
                        &session_url,
                        &worker_jwt,
                        &worker_epoch,
                        &[(eid.clone(), "received".to_string())],
                    )
                    .await;
                let _ = c
                    .report_delivery(
                        &session_url,
                        &worker_jwt,
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
                        &worker_jwt,
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
                            .worker_processing(&session_url, &worker_jwt, &worker_epoch)
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
                                &worker_jwt,
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
                            .register_worker(&session_url, &worker_jwt, &worker_epoch)
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
            return Ok(());
        }
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
    let sessions = orchestrator::session::list_all().unwrap_or_default();
    let running = sessions
        .iter()
        .filter(|s| s.status == orchestrator::session::SessionStatus::Running)
        .count();
    let stopped = sessions
        .iter()
        .filter(|s| s.status == orchestrator::session::SessionStatus::Stopped)
        .count();

    let welcome = if sessions.is_empty() {
        format!(
            "## {hostname} Session Manager\n\n\
             No sessions found. Get started:\n\n\
             - `/start <name> [image]` — create a new session (default image: ubuntu)\n\
             - `/help` — see all commands"
        )
    } else {
        format!(
            "## {hostname} Session Manager\n\n\
             **{running}** running, **{stopped}** stopped ({} total)\n\n\
             - `/list` — show all sessions\n\
             - `/start <name> [image]` — create a new session\n\
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

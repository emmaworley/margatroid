mod pty;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query,
    },
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower_http::services::ServeDir;

use pty::PtyProcess;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let static_dir = find_static_dir();
    tracing::info!("serving static files from {}", static_dir.display());

    // Compute a version hash from index.html (which references content-hashed
    // JS/CSS bundles, so any asset change means a different hash).
    let version = compute_version(&static_dir);
    tracing::info!("frontend version: {version}");

    let app = Router::new()
        .route("/api/sessions", get(list_sessions))
        .route("/api/version", get(move || async move {
            Json(serde_json::json!({ "version": version }))
        }))
        .route("/ws/:session", get(ws_handler))
        .fallback_service(ServeDir::new(&static_dir));

    let addr = "0.0.0.0:8080";
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    // Disable Nagle's algorithm on accepted connections so single-keystroke
    // WebSocket frames are sent immediately without buffering.
    axum::serve(listener, app)
        .tcp_nodelay(true)
        .await
        .unwrap();
}

fn compute_version(static_dir: &std::path::Path) -> String {
    // Read the .version file written by the frontend build (SHA256 of index.html,
    // which references all content-hashed asset filenames).
    let version_file = static_dir.join(".version");
    std::fs::read_to_string(&version_file)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn find_static_dir() -> std::path::PathBuf {
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/dist");
    if dev.exists() {
        return dev;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let installed = dir.join("static");
            if installed.exists() {
                return installed;
            }
        }
    }
    dev
}

#[derive(Serialize)]
struct SessionInfo {
    name: String,
    image: String,
    status: String,
    connectable: bool,
}

async fn list_sessions() -> Json<Vec<SessionInfo>> {
    let sessions = tokio::task::spawn_blocking(|| {
        margatroid::session::list_all().unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    let mut result = Vec::with_capacity(sessions.len() + 1);

    result.push(SessionInfo {
        name: "_manager".into(),
        image: "tui".into(),
        status: "running".into(),
        connectable: true,
    });

    for s in sessions {
        let running = s.status == margatroid::session::SessionStatus::Running;
        // Sessions are connectable if running and the relay socket exists.
        let sock = margatroid::margatroid_dir()
            .join("sessions")
            .join(&s.name)
            .join("relay.sock");
        let connectable = running && sock.exists();
        result.push(SessionInfo {
            name: s.name,
            image: s.image,
            status: if running { "running" } else { "stopped" }.into(),
            connectable,
        });
    }

    Json(result)
}

#[derive(Deserialize)]
struct WsParams {
    cols: Option<u16>,
    rows: Option<u16>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(session): Path<String>,
    Query(params): Query<WsParams>,
) -> Result<impl IntoResponse, StatusCode> {
    let cols = params.cols.unwrap_or(80);
    let rows = params.rows.unwrap_or(24);

    if session == "_manager" {
        let tui = find_tui_binary().ok_or(StatusCode::NOT_FOUND)?;
        return Ok(ws.on_upgrade(move |socket| handle_manager(socket, tui, cols, rows)));
    }

    // Verify relay socket exists for this session.
    let sock_path = margatroid::margatroid_dir()
        .join("sessions")
        .join(&session)
        .join("relay.sock");
    if !sock_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(ws.on_upgrade(move |socket| handle_relay(socket, session, sock_path, cols, rows)))
}

fn find_tui_binary() -> Option<String> {
    let installed = margatroid::margatroid_dir().join("bin/margatroid-tui");
    if installed.exists() {
        return Some(installed.to_string_lossy().into_owned());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let dev = dir.join("margatroid-tui");
            if dev.exists() {
                return Some(dev.to_string_lossy().into_owned());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Manager: spawns a fresh TUI in a PTY (no relay)
// ---------------------------------------------------------------------------

async fn handle_manager(socket: WebSocket, tui: String, cols: u16, rows: u16) {
    let pty = match PtyProcess::spawn(&tui, &[], cols, rows) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("failed to spawn tui: {e}");
            return;
        }
    };

    tracing::info!("web session connected: _manager");
    let master = pty.master();
    let (mut ws_sender, mut ws_receiver) = socket.split();

    let read_master = master.clone();
    let mut pty_to_ws = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match read_master.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if ws_sender.send(Message::Binary(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let write_master = master;
    let mut ws_to_pty = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Binary(data) => {
                    if write_master.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Message::Text(text) => {
                    if let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&text) {
                        if cmd["type"] == "resize" {
                            if let (Some(c), Some(r)) =
                                (cmd["cols"].as_u64(), cmd["rows"].as_u64())
                            {
                                write_master.resize(c as u16, r as u16);
                            }
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut pty_to_ws => {}
        _ = &mut ws_to_pty => {}
    }
    pty_to_ws.abort();
    ws_to_pty.abort();
    tracing::info!("web session disconnected: _manager");
}

// ---------------------------------------------------------------------------
// Relay: connect to the session's Unix socket
// ---------------------------------------------------------------------------

async fn handle_relay(
    socket: WebSocket,
    session: String,
    sock_path: std::path::PathBuf,
    cols: u16,
    rows: u16,
) {
    let stream = match tokio::net::UnixStream::connect(&sock_path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to connect to relay for {session}: {e}");
            return;
        }
    };

    tracing::info!("web session connected: {session}");
    let (mut sock_reader, mut sock_writer) = stream.into_split();

    // Send initial resize so the relay knows our dimensions.
    let mut resize_msg = vec![0u8];
    resize_msg.extend_from_slice(&cols.to_le_bytes());
    resize_msg.extend_from_slice(&rows.to_le_bytes());
    let _ = sock_writer.write_all(&resize_msg).await;

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Relay socket → WebSocket
    let mut relay_to_ws = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match sock_reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if ws_sender.send(Message::Binary(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // WebSocket → relay socket
    let mut ws_to_relay = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Binary(data) => {
                    if sock_writer.write_all(&data).await.is_err()
                        || sock_writer.flush().await.is_err()
                    {
                        break;
                    }
                }
                Message::Text(text) => {
                    if let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&text) {
                        if cmd["type"] == "resize" {
                            if let (Some(c), Some(r)) =
                                (cmd["cols"].as_u64(), cmd["rows"].as_u64())
                            {
                                let mut msg = vec![0u8];
                                msg.extend_from_slice(&(c as u16).to_le_bytes());
                                msg.extend_from_slice(&(r as u16).to_le_bytes());
                                if sock_writer.write_all(&msg).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut relay_to_ws => {}
        _ = &mut ws_to_relay => {}
    }
    relay_to_ws.abort();
    ws_to_relay.abort();
    tracing::info!("web session disconnected: {session}");
}

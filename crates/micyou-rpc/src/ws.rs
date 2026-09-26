/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 *
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! WebSocket transport: JSON-RPC over `ws://<addr>/rpc` (text frames, one
//! message per frame). Serves browser frontends, remote tools and any
//! language with a WebSocket client. A `/health` GET endpoint answers
//! liveness probes.
//!
//! Bind to loopback by default; exposing the daemon on a LAN interface is a
//! deliberate operator choice (there is no authentication yet — see
//! docs/rpc-api.md "Security").

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::router::RpcService;

/// Shared router state: the RPC service plus an optional static web-UI
/// directory (e.g. a Flutter-web bundle) served under `/ui/`.
type Shared = (Arc<RpcService>, Option<PathBuf>);

/// Serve the WebSocket transport until the process ends (or the returned
/// server task is aborted). Returns the bound address error, if any.
pub async fn serve_ws(service: Arc<RpcService>, addr: SocketAddr) -> Result<(), String> {
    serve_ws_with_ui(service, addr, None).await
}

/// Like [`serve_ws`], additionally hosting a static frontend bundle at
/// `/ui/` (and redirecting `/` there) when `web_ui` is set.
pub async fn serve_ws_with_ui(
    service: Arc<RpcService>,
    addr: SocketAddr,
    web_ui: Option<PathBuf>,
) -> Result<(), String> {
    let app = Router::new()
        .route("/rpc", get(ws_upgrade))
        .route("/health", get(health))
        .route("/", get(root))
        .route("/ui/{*path}", get(static_file))
        .with_state((service, web_ui));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?;
    log::info!("[rpc] WebSocket transport listening on ws://{bound}/rpc");

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("ws server: {e}"))
}

async fn root(State((_, ui)): State<Shared>) -> impl IntoResponse {
    if ui.is_some() {
        Redirect::to("/ui/").into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

fn mime_of(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("wasm") => "application/wasm",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Minimal path-traversal-safe static file server for the bundled web UI.
async fn static_file(State((_, dir)): State<Shared>, Path(path): Path<String>) -> Response {
    let Some(dir) = dir else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let rel = path.trim_start_matches('/');
    if rel.split(['/', '\\']).any(|seg| seg == "..") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut file = dir.join(if rel.is_empty() { "index.html" } else { rel });
    if file.is_dir() {
        file = file.join("index.html");
    }
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let mime = mime_of(&file);
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn health(State((service, _)): State<Shared>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "backend": "libmicyou",
        "version": env!("CARGO_PKG_VERSION"),
        "apiVersion": micyou_api::API_VERSION,
        "sessions": service.sessions.count(),
    }))
}

async fn ws_upgrade(ws: WebSocketUpgrade, State((service, _)): State<Shared>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, service))
}

async fn handle_socket(socket: WebSocket, service: Arc<RpcService>) {
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Outbound pump: session lines → text frames.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let send_task = tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if ws_sink.send(Message::Text(line.into())).await.is_err() {
                break;
            }
        }
    });

    let session = service.sessions.create(out_tx);
    log::info!("[rpc] websocket session {} attached", session.id);

    while let Some(Ok(message)) = ws_stream.next().await {
        match message {
            Message::Text(text) => {
                if let Some(response) = service.handle_line(&session, &text).await {
                    if !session.send_line(response) {
                        break;
                    }
                }
            }
            Message::Close(_) => break,
            // Binary/ping/pong: axum answers pings itself; ignore the rest.
            _ => {}
        }
    }

    log::info!("[rpc] websocket session {} detached", session.id);
    // Drop the session to close the outbound channel; the send task drains
    // queued frames before finishing.
    service.sessions.remove(session.id);
    drop(session);
    let _ = send_task.await;
}

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
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::router::RpcService;

/// Serve the WebSocket transport until the process ends (or the returned
/// server task is aborted). Returns the bound address error, if any.
pub async fn serve_ws(service: Arc<RpcService>, addr: SocketAddr) -> Result<(), String> {
    let app = Router::new()
        .route("/rpc", get(ws_upgrade))
        .route("/health", get(health))
        .with_state(service);

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

async fn health(State(service): State<Arc<RpcService>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "backend": "libmicyou",
        "version": env!("CARGO_PKG_VERSION"),
        "apiVersion": micyou_api::API_VERSION,
        "sessions": service.sessions.count(),
    }))
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(service): State<Arc<RpcService>>,
) -> impl IntoResponse {
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

/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! Stdio transport: newline-delimited JSON-RPC over stdin/stdout.
//!
//! This is the primary transport for sidecar deployments — a Tauri (or any
//! GUI) frontend spawns `micyou-daemon --stdio` as a child process and talks
//! to it over pipes. Logs must never go to stdout on this transport (use the
//! file logger or stderr mirroring).

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::router::RpcService;

/// Serve one stdio session until EOF on stdin. Returns after cleanup.
pub async fn serve_stdio(service: Arc<RpcService>) -> std::io::Result<()> {
    // Outbound writer task owns stdout (single writer, ordered lines).
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdout.write_all(b"\n").await.is_err() {
                break;
            }
            if stdout.flush().await.is_err() {
                break;
            }
        }
    });

    let session = service.sessions.create(out_tx);
    log::info!("[rpc] stdio session {} attached", session.id);

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = stdin.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = service.handle_line(&session, &line).await {
            if !session.send_line(response) {
                break; // writer gone (stdout closed)
            }
        }
    }

    log::info!("[rpc] stdio session {} detached (eof)", session.id);
    // Detach and DROP the session so the outbound channel closes; the writer
    // task then drains any queued responses (e.g. the reply to the final
    // request that arrived together with EOF) and exits on its own.
    service.sessions.remove(session.id);
    drop(session);
    let _ = writer.await;
    Ok(())
}

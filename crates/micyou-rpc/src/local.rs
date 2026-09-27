/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! In-process transport: a duplex pair of unbounded channels speaking the
//! same newline-delimited JSON-RPC as the other transports. Embedded Rust
//! hosts (CLI, TUI, a Tauri app keeping the backend in-process during
//! migration) get the full API — including events — without sockets or
//! child processes, and with identical serialization, so switching a
//! frontend from in-process to `--stdio`/`--ws` later is a non-event.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::router::RpcService;

/// Client end of an in-process RPC connection.
pub struct LocalConnection {
    /// Send JSON-RPC request/notification lines to the backend.
    pub inbound: mpsc::UnboundedSender<String>,
    /// Receive responses and event notifications from the backend.
    pub outbound: mpsc::UnboundedReceiver<String>,
}

/// Attach an in-process session to `service`.
///
/// Spawns the request pump task; the returned connection stays usable until
/// `inbound` is dropped (which detaches the session).
pub fn attach(service: Arc<RpcService>) -> LocalConnection {
    let (inbound, mut inbound_rx) = mpsc::unbounded_channel::<String>();
    let (outbound_tx, outbound) = mpsc::unbounded_channel::<String>();

    let session = service.sessions.create(outbound_tx);
    log::info!("[rpc] in-process session {} attached", session.id);

    tokio::spawn(async move {
        while let Some(line) = inbound_rx.recv().await {
            if let Some(response) = service.handle_line(&session, &line).await {
                if !session.send_line(response) {
                    break;
                }
            }
        }
        log::info!("[rpc] in-process session {} detached", session.id);
        service.sessions.remove(session.id);
    });

    LocalConnection { inbound, outbound }
}

#[cfg(test)]
mod tests {
    use super::*;
    use micyou_core::Backend;

    #[tokio::test]
    async fn request_response_roundtrip_over_channels() {
        let service = RpcService::new(Arc::new(Backend::new()));
        service.spawn_event_pump();
        let mut conn = attach(service.clone());

        conn.inbound
            .send(r#"{"jsonrpc":"2.0","id":1,"method":"system/version"}"#.to_string())
            .unwrap();
        let line = conn.outbound.recv().await.expect("response");
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], 1);
        assert!(value["result"]["version"].is_string());
    }

    #[tokio::test]
    async fn events_reach_subscribed_in_process_session() {
        let service = RpcService::new(Arc::new(Backend::new()));
        service.spawn_event_pump();
        let mut conn = attach(service.clone());

        conn.inbound
            .send(
                r#"{"jsonrpc":"2.0","id":1,"method":"session/subscribe","params":{"events":["*"]}}"#
                    .to_string(),
            )
            .unwrap();
        let _ = conn.outbound.recv().await.expect("subscribe ack");

        // Publish directly on the core bus; the pump must fan it out.
        service
            .backend
            .core
            .bus
            .publish(micyou_core::ServerEvent::AudioLevel { level: 55 });

        let line = conn.outbound.recv().await.expect("event");
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["method"], "event");
        assert_eq!(value["params"]["type"], "audioLevel");
        assert_eq!(value["params"]["data"]["level"], 55);
    }
}

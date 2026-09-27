/* libmicyou — micyou-gui frontend (original MicYou Vue UI). */

//! Backend session management: spawn `micyou-daemon --stdio` as a sidecar,
//! perform the JSON-RPC handshake, keep a shared client in Tauri state and
//! pump backend events into the webview. If the daemon dies (crash, upgrade),
//! the session manager respawns it on a short backoff so the GUI survives.

use std::sync::Arc;

use micyou_client::{Client, ClientError};
use serde_json::Value;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

/// Shared backend handle. `None` while (re)connecting — commands then fail
/// with [`NO_SESSION`] instead of blocking.
#[derive(Default)]
pub struct BackendState {
    pub client: Mutex<Option<Arc<Client>>>,
}

/// Error surfaced to the webview while no daemon session is attached.
pub const NO_SESSION: &str = "backend session not connected (daemon restarting?)";

/// Resolve the daemon binary: `$MICYOU_DAEMON` → sibling of this executable →
/// `micyou-daemon` on PATH. Same convention as the other reference frontends.
pub fn daemon_command() -> tokio::process::Command {
    let mut command = if let Ok(path) = std::env::var("MICYOU_DAEMON") {
        tokio::process::Command::new(path)
    } else if let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
        .map(|dir| {
            dir.join(if cfg!(windows) {
                "micyou-daemon.exe"
            } else {
                "micyou-daemon"
            })
        })
        .filter(|path| path.exists())
    {
        tokio::process::Command::new(sibling)
    } else {
        tokio::process::Command::new(if cfg!(windows) {
            "micyou-daemon.exe"
        } else {
            "micyou-daemon"
        })
    };
    // The GUI owns the whole backend surface; skip the shared mode lock so
    // dev builds coexist with a stock MicYou install.
    command.arg("--stdio").arg("--no-mode-lock");
    command
}

/// Spawn the connect → serve → reconnect supervisor. Call once from setup.
pub fn spawn_session_manager(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            match connect().await {
                Ok(client) => {
                    let client = Arc::new(client);
                    {
                        let state = app.state::<BackendState>();
                        *state.client.lock().await = Some(client.clone());
                    }
                    pump_events(&app, &client).await;
                    log::error!("backend event stream closed; restarting daemon");
                }
                Err(e) => {
                    log::error!("backend connection failed: {e}; retrying in 3s");
                }
            }
            {
                let state = app.state::<BackendState>();
                *state.client.lock().await = None;
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    });
}

/// Spawn the sidecar and perform hello + subscribe("*") so the GUI receives
/// every event, including the high-frequency audio streams.
async fn connect() -> Result<Client, ClientError> {
    let mut command = daemon_command();
    log::info!("spawning daemon sidecar: {command:?}");
    let client = Client::connect_stdio(&mut command).await?;
    let info = client.hello("micyou-gui", true).await?;
    client.subscribe(&["*"]).await?;
    log::info!(
        "backend connected: {} v{} (api v{}, {} {})",
        info.backend,
        info.version,
        info.api_version,
        info.os,
        info.arch
    );
    Ok(client)
}

/// Forward daemon events to the webview until the stream closes (daemon gone).
async fn pump_events(app: &AppHandle, client: &Arc<Client>) {
    let mut events = client.events();
    loop {
        match events.recv().await {
            Ok(event) => crate::events::forward(app, client, &event).await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Map a client error onto the plain-string shape the Vue UI expects
/// (upstream Tauri commands returned `Result<_, String>`).
pub fn rpc_err(error: ClientError) -> String {
    match error {
        ClientError::Rpc { message, .. } => message,
        other => other.to_string(),
    }
}

/// One RPC round-trip against the connected daemon.
pub async fn rpc(client: &Client, method: &str, params: Value) -> Result<Value, String> {
    client
        .call::<Value, Value>(method, params)
        .await
        .map_err(rpc_err)
}

/// Convenience wrapper for command handlers: resolve the shared client and
/// call [`rpc`].
pub async fn rpc_state(
    state: &BackendState,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let client = { state.client.lock().await.clone() }.ok_or(NO_SESSION)?;
    rpc(&client, method, params).await
}

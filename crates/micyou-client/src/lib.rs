/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 *
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! MicYou backend client SDK.
//!
//! Typed JSON-RPC client for frontends written in Rust (CLI, TUI, a Tauri
//! app driving the daemon as a sidecar). Every method mirrors the contract
//! in [`micyou_api::methods`]; events arrive on a broadcast stream with the
//! same filtering semantics as remote sessions.
//!
//! ```no_run
//! use micyou_client::Client;
//!
//! # async fn run() -> Result<(), micyou_client::ClientError> {
//! // Sidecar: spawn the daemon and talk over its pipes.
//! let mut client = Client::connect_stdio(
//!     tokio::process::Command::new("micyou-daemon").arg("--stdio"),
//! )
//! .await?;
//! client.hello("my-frontend", false).await?;
//! client.subscribe(&["*"]).await?;
//!
//! let status = client.server_status().await?;
//! println!("server phase: {}", status.phase);
//!
//! while let Ok(event) = client.events.recv().await {
//!     println!("event: {}", event.tag());
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

pub use micyou_api;
use micyou_api::config::{ServerPrefs, ThemeColors, UiPrefs};
use micyou_api::events::ServerEvent;
use micyou_api::jsonrpc::{Request, Response, RpcError, EVENT_METHOD, VERSION};
use micyou_api::methods as m;
use micyou_audio::dsp::AudioDspSettings;
use micyou_transport::adb::{AdbDevice, UsbModeResult};
use micyou_transport::host_info::{NetworkInfo, NetworkInterfaceInfo};
pub use micyou_transport::stats::AudioMetrics;
pub use micyou_transport::tcp::DeviceInfo;

/// Outbound line sink implemented by each transport.
enum Transport {
    Stdio {
        stdin: Arc<Mutex<tokio::process::ChildStdin>>,
        child: tokio::process::Child,
    },
    WebSocket {
        sink: Arc<
            Mutex<
                futures_util::stream::SplitSink<
                    tokio_tungstenite::WebSocketStream<
                        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                    >,
                    tokio_tungstenite::Message,
                >,
            >,
        >,
    },
}

/// Client-side failures.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Transport I/O failure.
    #[error("transport error: {0}")]
    Transport(String),
    /// The backend returned a JSON-RPC error.
    #[error("rpc error {code}: {message}")]
    Rpc {
        /// JSON-RPC/application error code.
        code: i64,
        /// Backend-provided message.
        message: String,
        /// Structured error data, when present.
        data: Option<Value>,
    },
    /// Response payload did not match the expected type.
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl From<RpcError> for ClientError {
    fn from(error: RpcError) -> Self {
        ClientError::Rpc {
            code: error.code,
            message: error.message,
            data: error.data,
        }
    }
}

/// A connected libmicyou backend client.
pub struct Client {
    transport: Arc<Transport>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>,
    /// Backend events (all of them; filter locally as needed).
    pub events: broadcast::Receiver<Arc<ServerEvent>>,
    event_bus: broadcast::Sender<Arc<ServerEvent>>,
    /// Raw notification lines that are not events (future extensions).
    _raw_notifications: mpsc::UnboundedReceiver<Value>,
}

impl Client {
    /// Connect to a daemon spawned as a child process (`micyou-daemon --stdio`).
    ///
    /// The child is killed when the client is dropped.
    pub async fn connect_stdio(command: &mut tokio::process::Command) -> Result<Self, ClientError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| ClientError::Transport(format!("spawn daemon: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::Transport("daemon stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::Transport("daemon stdout unavailable".into()))?;

        let transport = Arc::new(Transport::Stdio {
            stdin: Arc::new(Mutex::new(stdin)),
            child,
        });

        let (event_bus, events) = broadcast::channel(256);
        let (raw_tx, raw_notifications) = mpsc::unbounded_channel();
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        {
            let pending = pending.clone();
            let event_bus = event_bus.clone();
            let raw_tx = raw_tx.clone();
            let mut lines = BufReader::new(stdout).lines();
            tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    route_inbound_line(&line, &pending, &event_bus, &raw_tx).await;
                }
            });
        }

        Ok(Self {
            transport,
            next_id: AtomicI64::new(1),
            pending,
            events,
            event_bus,
            _raw_notifications: raw_notifications,
        })
    }

    /// Connect to `ws://addr/rpc`.
    pub async fn connect_ws(url: &str) -> Result<Self, ClientError> {
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let (stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| ClientError::Transport(format!("ws connect {url}: {e}")))?;
        let (sink, mut incoming) = stream.split();
        let transport = Arc::new(Transport::WebSocket {
            sink: Arc::new(Mutex::new(sink)),
        });

        let (event_bus, events) = broadcast::channel(256);
        let (raw_tx, raw_notifications) = mpsc::unbounded_channel();
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Reader task: frames → responses / events.
        {
            let pending = pending.clone();
            let event_bus = event_bus.clone();
            let raw_tx = raw_tx.clone();
            tokio::spawn(async move {
                while let Some(Ok(frame)) = incoming.next().await {
                    let text = match frame {
                        WsMessage::Text(text) => text.to_string(),
                        WsMessage::Close(_) => break,
                        _ => continue,
                    };
                    route_inbound_line(&text, &pending, &event_bus, &raw_tx).await;
                }
            });
        }

        Ok(Self {
            transport,
            next_id: AtomicI64::new(1),
            pending,
            events,
            event_bus,
            _raw_notifications: raw_notifications,
        })
    }

    // ── raw plumbing ───────────────────────────────────────────────────────

    /// Call a method with typed params/result (escape hatch for methods this
    /// SDK does not wrap yet — the wire contract is the api crate).
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let params_value =
            serde_json::to_value(params).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let request = Request {
            jsonrpc: VERSION.to_string(),
            id: Some(micyou_api::jsonrpc::Id::Num(id)),
            method: method.to_string(),
            params: Some(params_value),
        };
        let line =
            serde_json::to_string(&request).map_err(|e| ClientError::Protocol(e.to_string()))?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.send_line(line).await?;

        let result = rx.await.map_err(|_| {
            ClientError::Transport("connection closed while awaiting response".into())
        })??;
        serde_json::from_value(result).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// Call a method without params.
    pub async fn call0<R: DeserializeOwned>(&self, method: &str) -> Result<R, ClientError> {
        self.call(method, serde_json::json!({})).await
    }

    async fn send_line(&self, line: String) -> Result<(), ClientError> {
        match &*self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut guard = stdin.lock().await;
                guard
                    .write_all(line.as_bytes())
                    .await
                    .map_err(|e| ClientError::Transport(e.to_string()))?;
                guard
                    .write_all(b"\n")
                    .await
                    .map_err(|e| ClientError::Transport(e.to_string()))?;
                guard
                    .flush()
                    .await
                    .map_err(|e| ClientError::Transport(e.to_string()))?;
                Ok(())
            }
            Transport::WebSocket { sink } => {
                use tokio_tungstenite::tungstenite::Message as WsMessage;
                let mut guard = sink.lock().await;
                guard
                    .send(WsMessage::Text(line.into()))
                    .await
                    .map_err(|e| ClientError::Transport(e.to_string()))?;
                Ok(())
            }
        }
    }

    // ── session ────────────────────────────────────────────────────────────

    /// Identify this client and declare capabilities (`ui` = can render
    /// plugin panels / handle uiRequest events).
    pub async fn hello(&self, name: &str, ui: bool) -> Result<m::SessionInfo, ClientError> {
        self.call(
            m::SESSION_HELLO,
            m::HelloParams {
                name: Some(name.to_string()),
                ui: Some(ui),
            },
        )
        .await
    }

    /// Subscribe to high-frequency events by tag prefix (`"audio"`, `"*"`).
    pub async fn subscribe(&self, filters: &[&str]) -> Result<(), ClientError> {
        let _: m::Ack = self
            .call(
                m::SESSION_SUBSCRIBE,
                m::SubscriptionParams {
                    events: filters.iter().map(|s| s.to_string()).collect(),
                },
            )
            .await?;
        Ok(())
    }

    /// Unsubscribe from previously added filters.
    pub async fn unsubscribe(&self, filters: &[&str]) -> Result<(), ClientError> {
        let _: m::Ack = self
            .call(
                m::SESSION_UNSUBSCRIBE,
                m::SubscriptionParams {
                    events: filters.iter().map(|s| s.to_string()).collect(),
                },
            )
            .await?;
        Ok(())
    }

    // ── server ─────────────────────────────────────────────────────────────

    /// Start the audio server (unset fields fall back to server.json).
    pub async fn start_server(
        &self,
        params: m::StartServerParams,
    ) -> Result<m::ServerActionResult, ClientError> {
        self.call(m::SERVER_START, params).await
    }

    /// Stop the audio server.
    pub async fn stop_server(&self) -> Result<m::ServerActionResult, ClientError> {
        self.call0(m::SERVER_STOP).await
    }

    /// Lifecycle/connection/mute status.
    pub async fn server_status(&self) -> Result<m::ServerStatus, ClientError> {
        self.call0(m::SERVER_STATUS).await
    }

    /// `server.json` preferences.
    pub async fn server_prefs(&self) -> Result<ServerPrefs, ClientError> {
        self.call0(m::SERVER_PREFS_GET).await
    }

    /// Persist `server.json`.
    pub async fn save_server_prefs(&self, prefs: ServerPrefs) -> Result<(), ClientError> {
        let _: m::MessageResult = self.call(m::SERVER_PREFS_SAVE, prefs).await?;
        Ok(())
    }

    // ── audio ──────────────────────────────────────────────────────────────

    /// System output device names.
    pub async fn audio_devices(&self) -> Result<Vec<String>, ClientError> {
        self.call0(m::AUDIO_DEVICES).await
    }

    /// Current DSP settings.
    pub async fn audio_settings(&self) -> Result<AudioDspSettings, ClientError> {
        self.call0(m::AUDIO_SETTINGS_GET).await
    }

    /// Apply + persist DSP settings.
    pub async fn update_audio_settings(
        &self,
        settings: AudioDspSettings,
    ) -> Result<(), ClientError> {
        let _: m::MessageResult = self
            .call(
                m::AUDIO_SETTINGS_UPDATE,
                m::UpdateSettingsParams { settings },
            )
            .await?;
        Ok(())
    }

    /// Set hard-mute.
    pub async fn set_muted(&self, muted: bool) -> Result<(), ClientError> {
        let _: m::Ack = self
            .call(m::AUDIO_MUTE_SET, m::MuteParams { muted })
            .await?;
        Ok(())
    }

    /// Toggle ear-return monitoring.
    pub async fn set_monitoring(&self, enabled: bool) -> Result<(), ClientError> {
        let _: m::Ack = self
            .call(m::AUDIO_MONITORING_SET, m::MonitoringParams { enabled })
            .await?;
        Ok(())
    }

    /// Toggle spectrum streaming.
    pub async fn set_spectrum_streaming(&self, enabled: bool) -> Result<(), ClientError> {
        let _: m::Ack = self
            .call(m::AUDIO_SPECTRUM_SET, m::SpectrumParams { enabled })
            .await?;
        Ok(())
    }

    // ── network / usb / devices ────────────────────────────────────────────

    /// LAN IPs for QR display.
    pub async fn network_info(&self) -> Result<NetworkInfo, ClientError> {
        self.call0(m::NETWORK_INFO).await
    }

    /// Candidate LAN interfaces.
    pub async fn network_interfaces(&self) -> Result<Vec<NetworkInterfaceInfo>, ClientError> {
        self.call0(m::NETWORK_INTERFACES).await
    }

    /// Set up adb reverse for USB mode.
    pub async fn usb_enable(
        &self,
        port: Option<u16>,
        device_serial: Option<String>,
    ) -> Result<UsbModeResult, ClientError> {
        self.call(
            m::USB_ENABLE,
            m::UsbEnableParams {
                port,
                device_serial,
            },
        )
        .await
    }

    /// Attached adb devices.
    pub async fn usb_devices(&self) -> Result<Vec<AdbDevice>, ClientError> {
        self.call0(m::USB_DEVICES).await
    }

    /// PipeWire virtual-device status.
    pub async fn pipewire_check(&self) -> Result<m::PipeWireStatus, ClientError> {
        self.call0(m::PIPEWIRE_CHECK).await
    }

    /// Web-mode status.
    pub async fn web_status(&self) -> Result<m::WebStatus, ClientError> {
        self.call0(m::WEB_STATUS).await
    }

    // ── config ─────────────────────────────────────────────────────────────

    /// `ui.json` preferences.
    pub async fn ui_prefs(&self) -> Result<UiPrefs, ClientError> {
        self.call0(m::CONFIG_UI_GET).await
    }

    /// Persist `ui.json`.
    pub async fn save_ui_prefs(&self, prefs: UiPrefs) -> Result<(), ClientError> {
        let _: m::Ack = self.call(m::CONFIG_UI_SAVE, prefs).await?;
        Ok(())
    }

    /// `theme.json` colors.
    pub async fn theme_colors(&self) -> Result<ThemeColors, ClientError> {
        self.call0(m::CONFIG_THEME_GET).await
    }

    /// Persist `theme.json`.
    pub async fn save_theme_colors(&self, colors: ThemeColors) -> Result<(), ClientError> {
        let _: m::Ack = self.call(m::CONFIG_THEME_SAVE, colors).await?;
        Ok(())
    }

    // ── plugins ────────────────────────────────────────────────────────────

    /// Installed plugins.
    pub async fn plugins_list(&self) -> Result<Vec<m::PluginView>, ClientError> {
        self.call0(m::PLUGINS_LIST).await
    }

    /// Enable/disable a plugin.
    pub async fn plugins_set_enabled(&self, id: &str, enabled: bool) -> Result<(), ClientError> {
        let _: m::Ack = self
            .call(
                m::PLUGINS_SET_ENABLED,
                m::PluginIdEnabledParams {
                    id: id.to_string(),
                    enabled,
                },
            )
            .await?;
        Ok(())
    }

    /// Trigger a plugin UI action.
    pub async fn plugins_trigger(
        &self,
        plugin_id: &str,
        action: &str,
        payload: Option<String>,
    ) -> Result<(), ClientError> {
        let _: m::Ack = self
            .call(
                m::PLUGINS_TRIGGER,
                m::PluginTriggerParams {
                    plugin_id: plugin_id.to_string(),
                    action: action.to_string(),
                    payload,
                },
            )
            .await?;
        Ok(())
    }

    /// Fetch a plugin panel document.
    pub async fn plugins_panel(
        &self,
        plugin_id: &str,
        panel_id: &str,
    ) -> Result<m::PluginPanel, ClientError> {
        self.call(
            m::PLUGINS_PANEL,
            m::PluginPanelParams {
                plugin_id: plugin_id.to_string(),
                panel_id: panel_id.to_string(),
            },
        )
        .await
    }

    // ── system ─────────────────────────────────────────────────────────────

    /// Backend + contract version.
    pub async fn version(&self) -> Result<m::VersionInfo, ClientError> {
        self.call0(m::SYSTEM_VERSION).await
    }

    /// Tail of the daemon log.
    pub async fn log_content(&self, max_bytes: Option<u64>) -> Result<String, ClientError> {
        let result: m::LogContentResult = self
            .call(m::SYSTEM_LOG_CONTENT, m::LogContentParams { max_bytes })
            .await?;
        Ok(result.content)
    }

    /// Mode-lock status.
    pub async fn mode_status(&self) -> Result<m::ModeStatus, ClientError> {
        self.call0(m::MODE_STATUS).await
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Kill the sidecar child, if this client exclusively owns it.
        if let Some(transport) = Arc::get_mut(&mut self.transport) {
            if let Transport::Stdio { child, .. } = transport {
                let _ = child.start_kill();
            }
        }
    }
}

/// Shared inbound-line handling for all transports: complete pending calls,
/// fan out events, surface unknown notifications.
async fn route_inbound_line(
    line: &str,
    pending: &Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>,
    event_bus: &broadcast::Sender<Arc<ServerEvent>>,
    raw_tx: &mpsc::UnboundedSender<Value>,
) {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return,
    };

    // Notification (no id): event or raw passthrough.
    if value.get("id").is_none() {
        if value.get("method").and_then(|v| v.as_str()) == Some(EVENT_METHOD) {
            if let Some(params) = value.get("params") {
                if let Ok(event) = serde_json::from_value::<ServerEvent>(params.clone()) {
                    let _ = event_bus.send(Arc::new(event));
                }
            }
        } else {
            let _ = raw_tx.send(value);
        }
        return;
    }

    // Response: complete the matching pending call.
    let response: Response = match serde_json::from_value(value) {
        Ok(response) => response,
        Err(_) => return,
    };
    let micyou_api::jsonrpc::Id::Num(id) = response.id else {
        return;
    };
    let sender = pending.lock().await.remove(&id);
    if let Some(sender) = sender {
        let result = match (response.result, response.error) {
            (Some(result), _) => Ok(result),
            (None, Some(error)) => Err(error),
            (None, None) => Err(RpcError::internal("response without result or error")),
        };
        let _ = sender.send(result);
    }
}

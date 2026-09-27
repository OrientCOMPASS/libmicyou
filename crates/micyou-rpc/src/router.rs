/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! The RPC router: maps JSON-RPC method names onto [`Backend`] calls and
//! manages per-session subscriptions. One `RpcService` instance serves every
//! transport (stdio, WebSocket, in-process) concurrently.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

use micyou_api::error as codes;
use micyou_api::jsonrpc::{Id, Request, Response, RpcError, VERSION};
use micyou_api::methods as m;
use micyou_core::Backend;

use crate::session::{SessionHandle, SessionRegistry};
use crate::ui::RpcUiBridge;

/// The RPC facade shared by all transports.
pub struct RpcService {
    /// Backend service implementing every method.
    pub backend: Arc<Backend>,
    /// Live sessions (event pump + UI bridge read this).
    pub sessions: Arc<SessionRegistry>,
}

impl RpcService {
    /// Create the service and wire the plugin-facing bridges:
    /// - the UI bridge (plugin `open_window` → `uiRequest` events), and
    /// - the host-RPC bridge (plugin `call_host` → `dispatch`, capability-gated).
    pub fn new(backend: Arc<Backend>) -> Arc<Self> {
        let sessions = SessionRegistry::new();
        backend.core.plugins.ui.set(Some(Arc::new(RpcUiBridge {
            sessions: sessions.clone(),
        })));
        let service = Arc::new(Self { backend, sessions });
        service.install_host_bridge();
        service
    }

    /// Install the plugin `call_host` bridge (synthetic admin-less session;
    /// access is decided per call by capabilities + method classification).
    fn install_host_bridge(self: &Arc<Self>) {
        let (out_tx, _out_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = self.sessions.create(out_tx);
        let bridge = Arc::new(crate::host_bridge::RpcHostBridge::new(
            self.clone(),
            session,
        ));
        if let Ok(mut slot) = self.backend.core.plugins.host_rpc.write() {
            *slot = Some(bridge);
        }
        // Plugin event subscriptions are served by the core pump; make sure
        // it runs whenever a runtime is available.
        self.backend.core.ensure_plugin_event_pump();
    }

    /// Spawn the event pump: forwards [`micyou_core::EventBus`] events to
    /// every session whose filters accept them. Runs until the bus closes
    /// (process lifetime in practice); call once per service.
    pub fn spawn_event_pump(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            let mut rx = service.backend.core.bus.subscribe();
            loop {
                match rx.recv().await {
                    Ok(event) => service.sessions.broadcast_event(&event),
                    // Lagged subscriber: resynchronize on the next event.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    /// Handle one inbound JSON line/frame for a session. Returns the response
    /// line, or `None` for notifications and unparsable input without an id.
    pub async fn handle_line(&self, session: &SessionHandle, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let request: Request = match serde_json::from_str(trimmed) {
            Ok(request) => request,
            Err(e) => {
                let response = Response::err(
                    Id::Null,
                    RpcError::parse_error().with_data_fallback(format_args!("{e}")),
                );
                return serialize_response(&response);
            }
        };
        if request.jsonrpc != VERSION {
            if request.is_notification() {
                return None;
            }
            let response = Response::err(
                request.id.unwrap_or(Id::Null),
                RpcError::invalid_request(format!(
                    "unsupported jsonrpc version {:?}",
                    request.jsonrpc
                )),
            );
            return serialize_response(&response);
        }

        let id = match request.id {
            Some(id) => id,
            None => {
                // Notification: fire-and-forget (still validated/dispatched).
                let _ = self
                    .dispatch(session, &request.method, request.params)
                    .await;
                return None;
            }
        };

        let result = self
            .dispatch(session, &request.method, request.params)
            .await;
        let response = match result {
            Ok(value) => Response::ok(id, value),
            Err(error) => Response::err(id, error),
        };
        serialize_response(&response)
    }

    /// Dispatch one method call. Public so in-process hosts and the plugin
    /// `call_host` bridge can reuse the exact same code path.
    pub async fn dispatch(
        &self,
        session: &SessionHandle,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcError> {
        let b = &self.backend;
        match method {
            // ── session ────────────────────────────────────────────────
            m::SESSION_HELLO => {
                let hello: m::HelloParams = parse_params(params)?;
                if let Some(name) = hello.name {
                    if let Ok(mut slot) = session.name.write() {
                        *slot = name;
                    }
                }
                session.ui_capable.store(
                    hello.ui.unwrap_or(false),
                    std::sync::atomic::Ordering::Relaxed,
                );
                to_value(b.session_hello())
            }
            m::SESSION_SUBSCRIBE => {
                let p: m::SubscriptionParams = parse_params(params)?;
                session.add_subscriptions(&p.events);
                to_value(m::Ack::ok())
            }
            m::SESSION_UNSUBSCRIBE => {
                let p: m::SubscriptionParams = parse_params(params)?;
                session.remove_subscriptions(&p.events);
                to_value(m::Ack::ok())
            }

            // ── server ─────────────────────────────────────────────────
            m::SERVER_START => {
                let p: m::StartServerParams = parse_params(params)?;
                ok_or_rpc(b.start_server(p).await)
            }
            m::SERVER_STOP => ok_or_rpc(b.stop_server().await),
            m::SERVER_STATUS => to_value(b.server_status().await),
            m::SERVER_PREFS_GET => to_value(b.server_prefs()),
            m::SERVER_PREFS_SAVE => {
                let p = parse_params(params)?;
                ok_or_rpc(b.save_server_prefs(p))
            }
            m::SERVER_PREFS_EXISTS => to_value(b.server_prefs_exists()),

            // ── audio ──────────────────────────────────────────────────
            m::AUDIO_DEVICES => to_value(b.audio_devices()),
            m::AUDIO_SETTINGS_GET => to_value(b.audio_settings()),
            m::AUDIO_SETTINGS_UPDATE => {
                let p: m::UpdateSettingsParams = parse_params(params)?;
                ok_or_rpc(b.update_audio_settings(p.settings))
            }
            m::AUDIO_MUTE_SET => {
                let p: m::MuteParams = parse_params(params)?;
                to_value(b.set_muted(p.muted))
            }
            m::AUDIO_MUTE_GET => to_value(b.muted()),
            m::AUDIO_MONITORING_SET => {
                let p: m::MonitoringParams = parse_params(params)?;
                to_value(b.set_monitoring(p.enabled))
            }
            m::AUDIO_SPECTRUM_SET => {
                let p: m::SpectrumParams = parse_params(params)?;
                to_value(b.set_spectrum_streaming(p.enabled))
            }

            // ── network / usb ──────────────────────────────────────────
            m::NETWORK_INFO => to_value(b.network_info()),
            m::NETWORK_INTERFACES => to_value(b.network_interfaces()),
            m::NETWORK_FIREWALL_ALLOW => ok_or_rpc(b.firewall_allow().await),
            m::USB_ENABLE => {
                let p: m::UsbEnableParams = parse_params_or_default(params)?;
                ok_or_rpc(b.usb_enable(p))
            }
            m::USB_DEVICES => ok_or_rpc(b.usb_devices()),

            // ── virtual devices ────────────────────────────────────────
            m::VBCABLE_CHECK => ok_or_rpc(b.vbcable_check().await),
            m::VBCABLE_INSTALL => ok_or_rpc(b.vbcable_install().await),
            m::BLACKHOLE_CHECK => ok_or_rpc(b.blackhole_check().await),
            m::BLACKHOLE_SET_INPUT => ok_or_rpc(b.blackhole_set_input().await),
            m::BLACKHOLE_RESTORE => ok_or_rpc(b.blackhole_restore().await),
            m::PIPEWIRE_CHECK => to_value(b.pipewire_check()),

            // ── web mode ───────────────────────────────────────────────
            m::WEB_STATUS => to_value(b.web_status().await),

            // ── config ─────────────────────────────────────────────────
            m::CONFIG_UI_GET => to_value(b.ui_prefs()),
            m::CONFIG_UI_SAVE => {
                let p = parse_params(params)?;
                ok_or_rpc(b.save_ui_prefs(p))
            }
            m::CONFIG_THEME_GET => to_value(b.theme_colors()),
            m::CONFIG_THEME_SAVE => {
                let p = parse_params(params)?;
                ok_or_rpc(b.save_theme_colors(p))
            }

            // ── plugins ────────────────────────────────────────────────
            m::PLUGINS_LIST => ok_or_rpc(b.plugins_list()),
            m::PLUGINS_SET_ENABLED => {
                let p: m::PluginIdEnabledParams = parse_params(params)?;
                ok_or_rpc(b.plugins_set_enabled(p))
            }
            m::PLUGINS_UNINSTALL => {
                let p: m::PluginIdParams = parse_params(params)?;
                ok_or_rpc(b.plugins_uninstall(p))
            }
            m::PLUGINS_CONFIG_GET => {
                let p: m::PluginIdParams = parse_params(params)?;
                b.plugins_config_get(p).map_err(string_err)
            }
            m::PLUGINS_CONFIG_SET => {
                let p: m::PluginConfigSetParams = parse_params(params)?;
                ok_or_rpc(b.plugins_config_set(p))
            }
            m::PLUGINS_LOGS => {
                let p: m::PluginIdParams = parse_params(params)?;
                ok_or_rpc(b.plugins_logs(p))
            }
            m::PLUGINS_SYNC_STATUS => to_value(b.plugins_sync_status()),
            m::PLUGINS_DIR => ok_or_rpc(b.plugins_dir()),
            m::PLUGINS_PREVIEW_ZIP => {
                let p: m::PluginPreviewZipParams = parse_params(params)?;
                ok_or_rpc(b.plugins_preview_zip(p))
            }
            m::PLUGINS_PREVIEW_URL => {
                let p: m::PluginPreviewUrlParams = parse_params(params)?;
                ok_or_rpc(b.plugins_preview_url(p).await)
            }
            m::PLUGINS_INSTALL_URL => {
                let p: m::PluginInstallParams = parse_params(params)?;
                ok_or_rpc(b.plugins_install_url(p).await)
            }
            m::PLUGINS_INSTALL_CANCEL => {
                let p: m::PluginIdParams = parse_params(params)?;
                ok_or_rpc(b.plugins_install_cancel(p))
            }
            m::PLUGINS_UPDATE_CHECK => ok_or_rpc(b.plugins_update_check().await),
            m::PLUGINS_UPDATE_APPLY => {
                let p: m::PluginIdParams = parse_params(params)?;
                ok_or_rpc(b.plugins_update_apply(p).await)
            }
            m::PLUGINS_IMPORT => {
                let p: m::PluginImportParams = parse_params(params)?;
                ok_or_rpc(b.plugins_import(p))
            }
            m::PLUGINS_TRIGGER => {
                let p: m::PluginTriggerParams = parse_params(params)?;
                ok_or_rpc(b.plugins_trigger(p))
            }
            m::PLUGINS_PANEL => {
                let p: m::PluginPanelParams = parse_params(params)?;
                ok_or_rpc(b.plugins_panel(p))
            }
            m::PLUGINS_PANEL_ICONS => {
                let p: m::PluginIdParams = parse_params(params)?;
                to_value(b.plugins_panel_icons(p))
            }
            m::PLUGINS_WINDOW_OPEN => {
                let p: m::PluginPanelParams = parse_params(params)?;
                match b.plugins_window_open(p) {
                    Ok(ack) => to_value(ack),
                    Err(message) => Err(RpcError::new(codes::UI_UNAVAILABLE, message)),
                }
            }

            // ── mode ───────────────────────────────────────────────────
            m::MODE_STATUS => to_value(b.mode_status()),
            m::MODE_RELEASE_LOCK => ok_or_rpc(b.release_mode_lock()),

            // ── system ─────────────────────────────────────────────────
            m::SYSTEM_VERSION => to_value(b.version()),
            m::SYSTEM_LOG_PATH => to_value(b.log_path()),
            m::SYSTEM_LOG_CONTENT => {
                let p: m::LogContentParams = parse_params_or_default(params)?;
                to_value(b.log_content(p))
            }
            m::SYSTEM_LOG_EXPORT => ok_or_rpc(b.log_export()),
            m::SYSTEM_UPDATE_CHECK => {
                let p: m::UpdateCheckParams = parse_params_or_default(params)?;
                ok_or_rpc(b.update_check(p).await)
            }
            m::SYSTEM_SPONSORS => {
                let p: m::SponsorsParams = parse_params_or_default(params)?;
                ok_or_rpc(b.sponsors(p).await)
            }
            m::SYSTEM_LOCALE => to_value(b.locale()),

            other => Err(RpcError::method_not_found(other)),
        }
    }
}

/// Parse by-name params; missing params deserialize from `{}`.
fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcError> {
    let value = params.unwrap_or_else(|| Value::Object(Default::default()));
    serde_json::from_value(value).map_err(|e| RpcError::invalid_params(e.to_string()))
}

/// Like [`parse_params`] but tolerates explicit `null`.
fn parse_params_or_default<T: DeserializeOwned + Default>(
    params: Option<Value>,
) -> Result<T, RpcError> {
    match params {
        None | Some(Value::Null) => Ok(T::default()),
        Some(value) => {
            serde_json::from_value(value).map_err(|e| RpcError::invalid_params(e.to_string()))
        }
    }
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::internal(e.to_string()))
}

/// Map the backend's `Result<T, String>` onto the generic application error
/// code (messages stay human-readable for frontends).
fn ok_or_rpc<T: serde::Serialize>(result: Result<T, String>) -> Result<Value, RpcError> {
    match result {
        Ok(value) => to_value(value),
        Err(message) => Err(string_err(message)),
    }
}

fn string_err(message: String) -> RpcError {
    RpcError::new(codes::SERVER_ERROR, message)
}

fn serialize_response(response: &Response) -> Option<String> {
    serde_json::to_string(response).ok()
}

/// Extension used by the parse-error path (keeps RpcError construction
/// expressive without a second constructor in the API crate).
trait RpcErrorExt {
    fn with_data_fallback(self, detail: std::fmt::Arguments<'_>) -> Self;
}

impl RpcErrorExt for RpcError {
    fn with_data_fallback(mut self, detail: std::fmt::Arguments<'_>) -> Self {
        if self.data.is_none() {
            self.data = Some(Value::String(detail.to_string()));
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionRegistry;
    use tokio::sync::mpsc;

    fn test_service() -> (Arc<RpcService>, SessionHandle) {
        let backend = Arc::new(Backend::new());
        let service = RpcService::new(backend);
        let (tx, _rx) = mpsc::unbounded_channel();
        let session = service.sessions.create(tx);
        (service, session)
    }

    #[tokio::test]
    async fn unknown_method_yields_method_not_found() {
        let (service, session) = test_service();
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"nope/nope"}"#;
        let response = service.handle_line(&session, line).await.unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn hello_reports_backend_identity() {
        let (service, session) = test_service();
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"session/hello","params":{"name":"test","ui":true}}"#;
        let response = service.handle_line(&session, line).await.unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["backend"], "libmicyou");
        assert_eq!(value["result"]["apiVersion"], micyou_api::API_VERSION);
        assert_eq!(service.sessions.ui_capable_count(), 1);
        assert_eq!(*session.name.read().unwrap(), "test");
    }

    #[tokio::test]
    async fn version_and_status_answer_without_server() {
        let (service, session) = test_service();
        let response = service
            .handle_line(
                &session,
                r#"{"jsonrpc":"2.0","id":3,"method":"system/version"}"#,
            )
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert!(value["result"]["version"].is_string());

        let response = service
            .handle_line(
                &session,
                r#"{"jsonrpc":"2.0","id":4,"method":"server/status"}"#,
            )
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["phase"], "stopped");
        assert_eq!(value["result"]["isServerRunning"], false);
    }

    #[tokio::test]
    async fn subscribe_updates_session_filters() {
        let (service, session) = test_service();
        let line = r#"{"jsonrpc":"2.0","id":5,"method":"session/subscribe","params":{"events":["audio","*"]}}"#;
        service.handle_line(&session, line).await.unwrap();
        assert_eq!(
            session.subscriptions(),
            vec!["audio".to_string(), "*".to_string()]
        );

        let line = r#"{"jsonrpc":"2.0","id":6,"method":"session/unsubscribe","params":{"events":["audio"]}}"#;
        service.handle_line(&session, line).await.unwrap();
        assert_eq!(session.subscriptions(), vec!["*".to_string()]);
    }

    #[tokio::test]
    async fn malformed_json_yields_parse_error() {
        let (service, session) = test_service();
        let response = service.handle_line(&session, "{oops").await.unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], codes::PARSE_ERROR);
        assert_eq!(value["id"], Value::Null);
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        let (service, session) = test_service();
        let line = r#"{"jsonrpc":"2.0","method":"session/subscribe","params":{"events":["*"]}}"#;
        assert!(service.handle_line(&session, line).await.is_none());
        assert_eq!(session.subscriptions(), vec!["*".to_string()]);
    }

    #[test]
    fn registry_broadcast_reaches_subscribers() {
        let registry = SessionRegistry::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let session = registry.create(tx);
        session.set_subscriptions(vec!["*".to_string()]);
        registry.broadcast_event(&micyou_api::events::ServerEvent::AudioLevel { level: 9 });
        let line = rx.try_recv().expect("event delivered");
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["method"], micyou_api::jsonrpc::EVENT_METHOD);
        assert_eq!(value["params"]["type"], "audioLevel");
    }
}

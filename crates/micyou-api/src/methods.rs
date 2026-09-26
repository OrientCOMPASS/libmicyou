/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou command semantics)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! RPC method catalogue: names, parameter and result DTOs.
//!
//! Method names are namespaced (`domain/action`) and map 1:1 onto the
//! upstream Tauri command surface minus window/tray management (which stays
//! in graphical frontends). All params are by-name JSON objects; camelCase
//! throughout.
//!
//! | Upstream Tauri command        | RPC method                    |
//! |-------------------------------|-------------------------------|
//! | `start_server`                | `server/start`                |
//! | `stop_server`                 | `server/stop`                 |
//! | `get_streaming_status`        | `server/status`               |
//! | `get_server_prefs`            | `server/prefs/get`            |
//! | `save_server_prefs`           | `server/prefs/save`           |
//! | `server_prefs_exists`         | `server/prefs/exists`         |
//! | `get_audio_devices`           | `audio/devices`               |
//! | `get_audio_settings`          | `audio/settings/get`          |
//! | `update_audio_settings`       | `audio/settings/update`       |
//! | `set_mute_state`              | `audio/mute/set`              |
//! | `set_monitoring`              | `audio/monitoring/set`        |
//! | `set_spectrum_streaming`      | `audio/spectrum/set`          |
//! | `get_network_info`            | `network/info`                |
//! | `get_network_interfaces`      | `network/interfaces`          |
//! | `allow_firewall`              | `network/firewall/allow`      |
//! | `enable_usb_mode`             | `usb/enable`                  |
//! | `list_adb_devices`            | `usb/devices`                 |
//! | `check_vbcable`               | `devices/vbcable/check`       |
//! | `install_vbcable`             | `devices/vbcable/install`     |
//! | `check_blackhole`             | `devices/blackhole/check`     |
//! | `set_blackhole_as_input`      | `devices/blackhole/setInput`  |
//! | `restore_input_device`        | `devices/blackhole/restore`   |
//! | `check_pipewire`              | `devices/pipewire/check`      |
//! | `save_ui_prefs`               | `config/ui/save`              |
//! | `get_theme_colors`            | `config/theme/get`            |
//! | `save_theme_colors`           | `config/theme/save`           |
//! | `list_plugins` …              | `plugins/*`                   |
//! | `get_mode_status`             | `mode/status`                 |
//! | `release_gui_lock`            | `mode/releaseLock`            |
//! | `get_web_status`              | `web/status`                  |
//! | `get_app_version`             | `system/version`              |
//! | `get_log_path`                | `system/log/path`             |
//! | `get_log_content`             | `system/log/content`          |
//! | `export_log`                  | `system/log/export`           |
//! | `check_app_update`            | `system/update/check`         |
//! | `get_sponsors`                | `system/sponsors`             |
//! | `get_app_locale`              | `system/locale`               |

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// --- session ----------------------------------------------------------------

pub const SESSION_HELLO: &str = "session/hello";
pub const SESSION_SUBSCRIBE: &str = "session/subscribe";
pub const SESSION_UNSUBSCRIBE: &str = "session/unsubscribe";

/// Result of `session/hello`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// Backend implementation name (`"libmicyou"`).
    pub backend: String,
    /// Backend crate version.
    pub version: String,
    /// [`crate::API_VERSION`] of the contract.
    pub api_version: u32,
    /// Host OS (`"linux" | "windows" | "macos"`).
    pub os: String,
    /// CPU architecture (`"x86_64" | "aarch64" | …`).
    pub arch: String,
}

/// Params of `session/subscribe` / `session/unsubscribe`.
///
/// Filters are event tags or tag prefixes (`"audio*"` matches audioLevel,
/// audioMetrics, audioSpectrum; `"*"` matches everything). High-frequency
/// events are delivered only to subscribed sessions.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SubscriptionParams {
    pub events: Vec<String>,
}

// --- server -----------------------------------------------------------------

pub const SERVER_START: &str = "server/start";
pub const SERVER_STOP: &str = "server/stop";
pub const SERVER_STATUS: &str = "server/status";
pub const SERVER_PREFS_GET: &str = "server/prefs/get";
pub const SERVER_PREFS_SAVE: &str = "server/prefs/save";
pub const SERVER_PREFS_EXISTS: &str = "server/prefs/exists";

/// Params of `server/start`. Missing fields fall back to `server.json`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct StartServerParams {
    /// TCP control port (UDP audio = port + 1); web-mode TLS port in web mode.
    pub port: Option<u16>,
    /// `"wifi" | "usb" | "web"`.
    pub mode: Option<String>,
    /// Bind address override.
    pub bind_address: Option<String>,
    /// Output device override (`""`/`"auto"` = default/virtual device).
    pub output_device: Option<String>,
    /// USB mode: adb serial when multiple devices are attached.
    pub usb_device_serial: Option<String>,
}

/// Result of `server/start` and `server/stop`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerActionResult {
    pub message: String,
}

/// Result of `server/status` — superset of the upstream `StreamingStatus`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    /// Lifecycle phase: `stopped|starting|running|stopping|stoppingResidual`.
    pub phase: String,
    /// Convenience flag (`phase == running`).
    pub is_server_running: bool,
    /// A device (phone or browser) is connected.
    pub is_connected: bool,
    /// Local hard-mute flag.
    pub is_muted: bool,
    /// Ear-return monitoring flag.
    pub is_monitoring: bool,
    /// Mode of the running/last server, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Port of the running/last server, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

// --- audio ------------------------------------------------------------------

pub const AUDIO_DEVICES: &str = "audio/devices";
pub const AUDIO_SETTINGS_GET: &str = "audio/settings/get";
pub const AUDIO_SETTINGS_UPDATE: &str = "audio/settings/update";
pub const AUDIO_MUTE_SET: &str = "audio/mute/set";
pub const AUDIO_MUTE_GET: &str = "audio/mute/get";
pub const AUDIO_MONITORING_SET: &str = "audio/monitoring/set";
pub const AUDIO_SPECTRUM_SET: &str = "audio/spectrum/set";

/// Params of `audio/settings/update`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettingsParams {
    pub settings: micyou_audio::dsp::AudioDspSettings,
}

/// Params of `audio/mute/set`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MuteParams {
    pub muted: bool,
}

/// Params of `audio/monitoring/set`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MonitoringParams {
    pub enabled: bool,
}

/// Params of `audio/spectrum/set`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SpectrumParams {
    pub enabled: bool,
}

/// Result of `audio/mute/get`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MuteState {
    pub muted: bool,
}

// --- network / usb ----------------------------------------------------------

pub const NETWORK_INFO: &str = "network/info";
pub const NETWORK_INTERFACES: &str = "network/interfaces";
pub const NETWORK_FIREWALL_ALLOW: &str = "network/firewall/allow";
pub const USB_ENABLE: &str = "usb/enable";
pub const USB_DEVICES: &str = "usb/devices";

/// Params of `usb/enable` (port defaults to `server.json`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct UsbEnableParams {
    pub port: Option<u16>,
    pub device_serial: Option<String>,
}

// --- virtual devices --------------------------------------------------------

pub const VBCABLE_CHECK: &str = "devices/vbcable/check";
pub const VBCABLE_INSTALL: &str = "devices/vbcable/install";
pub const BLACKHOLE_CHECK: &str = "devices/blackhole/check";
pub const BLACKHOLE_SET_INPUT: &str = "devices/blackhole/setInput";
pub const BLACKHOLE_RESTORE: &str = "devices/blackhole/restore";
pub const PIPEWIRE_CHECK: &str = "devices/pipewire/check";

/// Result of `devices/vbcable/check`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VbcableStatus {
    pub installed: bool,
}

/// Result of `devices/pipewire/check`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PipeWireStatus {
    pub available: bool,
    pub setup: bool,
    pub device_exists: bool,
    pub install_command: String,
    pub distro: String,
}

// --- web mode ----------------------------------------------------------------

pub const WEB_STATUS: &str = "web/status";

/// Result of `web/status`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WebStatus {
    pub running: bool,
    pub client_count: u32,
}

// --- config ------------------------------------------------------------------

pub const CONFIG_UI_GET: &str = "config/ui/get";
pub const CONFIG_UI_SAVE: &str = "config/ui/save";
pub const CONFIG_THEME_GET: &str = "config/theme/get";
pub const CONFIG_THEME_SAVE: &str = "config/theme/save";

// --- plugins -----------------------------------------------------------------

pub const PLUGINS_LIST: &str = "plugins/list";
pub const PLUGINS_SET_ENABLED: &str = "plugins/setEnabled";
pub const PLUGINS_UNINSTALL: &str = "plugins/uninstall";
pub const PLUGINS_CONFIG_GET: &str = "plugins/config/get";
pub const PLUGINS_CONFIG_SET: &str = "plugins/config/set";
pub const PLUGINS_LOGS: &str = "plugins/logs";
pub const PLUGINS_SYNC_STATUS: &str = "plugins/syncStatus";
pub const PLUGINS_DIR: &str = "plugins/dir";
pub const PLUGINS_PREVIEW_ZIP: &str = "plugins/preview/zip";
pub const PLUGINS_PREVIEW_URL: &str = "plugins/preview/url";
pub const PLUGINS_INSTALL_URL: &str = "plugins/install/url";
pub const PLUGINS_INSTALL_CANCEL: &str = "plugins/install/cancel";
pub const PLUGINS_UPDATE_CHECK: &str = "plugins/update/check";
pub const PLUGINS_UPDATE_APPLY: &str = "plugins/update/apply";
pub const PLUGINS_IMPORT: &str = "plugins/import";
pub const PLUGINS_TRIGGER: &str = "plugins/trigger";
pub const PLUGINS_PANEL: &str = "plugins/panel";
pub const PLUGINS_PANEL_ICONS: &str = "plugins/panelIcons";
pub const PLUGINS_WINDOW_OPEN: &str = "plugins/window/open";

/// Frontend view of one installed plugin (`plugins/list`).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `"native" | "wasm"`.
    pub runtime: String,
    /// `"dsp" | "utility"`.
    pub kind: String,
    pub platforms: Vec<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui: Option<micyou_plugin::manifest::UiDescriptor>,
    pub enabled: bool,
    pub loaded: bool,
    pub dsp_node: bool,
    /// Load/enable error surfaced to the user (e.g. artifact missing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Localized names keyed by BCP-47 tag (`nameI18n`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub name_i18n: HashMap<String, String>,
    /// Localized descriptions keyed by BCP-47 tag (`descriptionI18n`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub description_i18n: HashMap<String, String>,
    /// Declared dependencies on other plugins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<micyou_plugin::manifest::PluginDependency>,
    /// Declarative settings schema for automatic form generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<micyou_plugin::manifest::ConfigSchema>,
}

/// Cross-device sync status (`plugins/syncStatus`).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginSyncStatus {
    /// Whether a phone device session is connected.
    pub device_connected: bool,
    /// Plugins can currently reach the remote device.
    pub transport_ready: bool,
}

/// Params of `plugins/setEnabled`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginIdEnabledParams {
    pub id: String,
    pub enabled: bool,
}

/// Params carrying a single plugin id.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginIdParams {
    pub id: String,
}

/// Params of `plugins/config/set`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginConfigSetParams {
    pub id: String,
    pub key: String,
    pub value: serde_json::Value,
}

/// Manifest preview before installation (`plugins/preview/*`).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginPreview {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub runtime: String,
    pub kind: String,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

/// Params of `plugins/preview/url`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginPreviewUrlParams {
    pub manifest_url: String,
}

/// Params of `plugins/install/url`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallParams {
    pub id: String,
    pub zip_url: String,
}

/// Available update for one plugin (`plugins/update/check`).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdate {
    pub id: String,
    pub current_version: String,
    pub latest_version: String,
    pub update_url: String,
}

/// Result of `plugins/update/apply`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateApplied {
    pub version: String,
}

/// Params of `plugins/import` (directory or .zip path).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginImportParams {
    pub source: String,
}

/// Params of `plugins/trigger`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginTriggerParams {
    pub plugin_id: String,
    pub action: String,
    /// Opaque payload handed to the plugin (`ui:<action>` message body).
    #[serde(default)]
    pub payload: Option<String>,
}

/// Params of `plugins/panel` and `plugins/window/open`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginPanelParams {
    pub plugin_id: String,
    pub panel_id: String,
}

/// Result of `plugins/panel` — the raw panel document for frontend rendering.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginPanel {
    /// Panel HTML source (from the plugin's declared `ui.panels[].entry`).
    pub html: String,
    /// Suggested window title (`<plugin name> · <panel label>`).
    pub title: String,
}

/// Result of `plugins/dir`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginDirResult {
    pub path: String,
}

// --- mode --------------------------------------------------------------------

pub const MODE_STATUS: &str = "mode/status";
pub const MODE_RELEASE_LOCK: &str = "mode/releaseLock";

/// Result of `mode/status`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModeStatus {
    /// Lock state on disk: `"gui" | "cli" | "tui" | "daemon" | "none"`.
    pub mode: String,
    pub pid: Option<u32>,
    /// Whether a live process owns the lock.
    pub running: bool,
}

// --- system ------------------------------------------------------------------

pub const SYSTEM_VERSION: &str = "system/version";
pub const SYSTEM_LOG_PATH: &str = "system/log/path";
pub const SYSTEM_LOG_CONTENT: &str = "system/log/content";
pub const SYSTEM_LOG_EXPORT: &str = "system/log/export";
pub const SYSTEM_UPDATE_CHECK: &str = "system/update/check";
pub const SYSTEM_SPONSORS: &str = "system/sponsors";
pub const SYSTEM_LOCALE: &str = "system/locale";

/// Result of `system/version`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    /// libmicyou crate version.
    pub version: String,
    /// Contract version.
    pub api_version: u32,
}

/// Params of `system/log/content`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct LogContentParams {
    /// Return at most this many trailing bytes (default 256 KiB).
    pub max_bytes: Option<u64>,
}

/// Result of `system/log/path` / `system/log/export`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LogPathResult {
    pub path: String,
}

/// Result of `system/log/content`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LogContentResult {
    pub content: String,
}

/// Params of `system/update/check`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateCheckParams {
    /// Optional MirrorChyan CDK for the mirror download channel.
    pub cdk: Option<String>,
}

/// Result of `system/update/check`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub has_update: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
    pub release_notes: Option<String>,
    pub is_mirror: bool,
    pub cdk_expired_time: Option<i64>,
}

/// Params of `system/sponsors` (proxied Afdian query).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SponsorsParams {
    /// Afdian API token (defaults to the `AIFADIAN_API_TOKEN` env var).
    pub api_token: Option<String>,
    /// Afdian user id (defaults to the `AIFADIAN_USER_ID` env var).
    pub user_id: Option<String>,
}

/// Result of `system/sponsors` — the raw Afdian JSON response body.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SponsorsResult {
    pub raw: String,
}

/// Result of `system/locale`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LocaleResult {
    pub language: String,
}

/// Generic acknowledgement result for methods without a payload.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Ack {
    pub ok: bool,
}

impl Ack {
    /// The single success value.
    pub fn ok() -> Self {
        Self { ok: true }
    }
}

/// Result carrying a human-readable message (`server/*`, settings updates).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MessageResult {
    pub message: String,
}

impl MessageResult {
    /// Wrap a message string.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_names_are_namespaced() {
        for name in [
            SESSION_HELLO,
            SERVER_START,
            AUDIO_SETTINGS_UPDATE,
            PLUGINS_INSTALL_URL,
            SYSTEM_UPDATE_CHECK,
        ] {
            assert!(name.contains('/'), "{name} must be namespaced");
            assert!(!name.starts_with('/'));
            assert!(!name.ends_with('/'));
        }
    }

    #[test]
    fn start_params_default_to_all_none() {
        let params: StartServerParams = serde_json::from_str("{}").unwrap();
        assert!(params.port.is_none());
        assert!(params.mode.is_none());
    }

    #[test]
    fn server_status_serializes_camel_case() {
        let status = ServerStatus {
            phase: "running".into(),
            is_server_running: true,
            is_connected: false,
            is_muted: false,
            is_monitoring: false,
            mode: Some("wifi".into()),
            port: Some(8554),
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["isServerRunning"], true);
        assert_eq!(value["mode"], "wifi");
    }
}

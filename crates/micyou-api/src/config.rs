/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! Shared preference file schemas.
//!
//! These structs are persisted under the platform config dir (`settings.json`,
//! `server.json`, `ui.json`, `theme.json`) with camelCase fields — byte
//! compatible with the stock MicYou GUI/CLI/TUI so all surfaces can share one
//! directory during and after migration. They are part of the RPC contract
//! (`server/prefs/*`, `config/*` methods).

use serde::{Deserialize, Serialize};

/// Connection-level settings (`server.json`), shared by every frontend.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerPrefs {
    /// Streaming port for wifi/usb modes (TCP control; UDP audio = port + 1).
    pub port: u16,
    /// Port for the web (https) mode.
    pub web_port: u16,
    /// Connection mode: `wifi` | `usb` | `web`.
    pub mode: String,
    /// Bind address (`"0.0.0.0"` when auto-bind).
    pub bind_address: String,
    /// Whether to listen on all interfaces.
    pub auto_bind: bool,
    /// Selected output audio device name (`""`/`"auto"`/`"default"` = system).
    pub output_device: String,
    /// Whether mute state is synchronized with the mobile client in both
    /// directions. Defaults to true (including for files written before this
    /// field existed).
    #[serde(default = "default_mute_sync")]
    pub mute_sync: bool,
}

fn default_mute_sync() -> bool {
    true
}

impl Default for ServerPrefs {
    fn default() -> Self {
        Self {
            port: 8554,
            web_port: 8443,
            mode: "wifi".to_string(),
            bind_address: "0.0.0.0".to_string(),
            auto_bind: true,
            output_device: String::new(),
            mute_sync: true,
        }
    }
}

impl ServerPrefs {
    /// Typed view of the `mode` string.
    pub fn transport_mode(&self) -> Option<micyou_transport::TransportMode> {
        micyou_transport::TransportMode::parse(&self.mode)
    }
}

/// GUI preferences (`ui.json`) shared with the TUI.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct UiPrefs {
    /// BCP-47-ish language key used by the frontends (`en`, `zh`, …).
    pub language: String,
    /// Material-3 seed color as `#rrggbb`.
    pub theme_color: String,
}

/// Theme colors (`theme.json`) exported from the GUI for other frontends.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ThemeColors {
    pub primary: String,
    pub secondary: String,
    pub tertiary: String,
    pub surface: String,
    pub surface_variant: String,
    pub on_surface: String,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_prefs_defaults_match_upstream() {
        let prefs = ServerPrefs::default();
        assert_eq!(prefs.port, 8554);
        assert_eq!(prefs.web_port, 8443);
        assert_eq!(prefs.mode, "wifi");
        assert!(prefs.mute_sync);
        assert_eq!(
            prefs.transport_mode(),
            Some(micyou_transport::TransportMode::Wifi)
        );
    }

    #[test]
    fn legacy_json_without_mute_sync_defaults_to_true() {
        let prefs: ServerPrefs = serde_json::from_str(r#"{"port": 9000, "mode": "usb"}"#).unwrap();
        assert!(prefs.mute_sync);
        assert_eq!(prefs.port, 9000);
        assert_eq!(prefs.web_port, 8443);
        assert!(prefs.auto_bind);
    }

    #[test]
    fn camel_case_field_names_are_stable() {
        let value = serde_json::to_value(ServerPrefs::default()).unwrap();
        assert!(value.get("webPort").is_some());
        assert!(value.get("bindAddress").is_some());
        assert!(value.get("muteSync").is_some());
        assert!(value.get("outputDevice").is_some());
    }
}

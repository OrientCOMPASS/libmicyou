/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! Backend → frontend event catalogue.
//!
//! Events are delivered as JSON-RPC notifications on method [`crate::jsonrpc::EVENT_METHOD`]
//! with params `{"type": <tag>, "data": <payload>}`. High-frequency events
//! (audio level/metrics/spectrum) are only sent to sessions that subscribed
//! (see `session/subscribe` in [`crate::methods`]).

use micyou_audio::AecFailure;
use micyou_transport::stats::AudioMetrics;
use micyou_transport::tcp::DeviceInfo;
use serde::{Deserialize, Serialize};

/// AEC availability/state snapshot.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AecStatus {
    /// Whether the platform provides a usable echo-reference capture path.
    pub available: bool,
    /// Whether AEC processing is currently enabled in the DSP settings.
    pub enabled: bool,
    /// Why AEC became unavailable, if it did.
    pub reason: Option<AecFailure>,
}

/// UI-side actions the backend cannot perform headlessly. A graphical
/// frontend is expected to handle these; the backend errors on the
/// corresponding request methods when no frontend is attached.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiRequest {
    /// Open a plugin's panel in a frontend-owned window.
    OpenPluginPanel { plugin_id: String, panel_id: String },
}

/// Events published by the backend core.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerEvent {
    /// A device (phone/web client) became the active audio source.
    DeviceConnected { device: DeviceInfo },
    /// The active device disconnected.
    DeviceDisconnected,
    /// Periodic network/audio metrics while streaming (1 Hz). *Subscribable.*
    AudioMetrics { metrics: AudioMetrics },
    /// Processed output level, 0..=100 (≈8 Hz while streaming). *Subscribable.*
    AudioLevel { level: u32 },
    /// Raw + processed spectrum bands. *Subscribable.*
    AudioSpectrum { raw: Vec<f32>, processed: Vec<f32> },
    /// Hard-mute state changed (any source: RPC, plugin, phone).
    MuteStateChanged { muted: bool },
    /// Ear-return monitoring toggled.
    MonitoringChanged { enabled: bool },
    /// Spectrum streaming toggled.
    SpectrumStreamingChanged { enabled: bool },
    /// The audio server stopped.
    ServerStopped,
    /// Web-mode browser client count changed.
    WebClientCount { count: u32 },
    /// TCP up but no UDP audio for >10 s (firewall hint, Windows wifi).
    UdpAudioWarning,
    /// Acoustic echo cancellation availability changed.
    AecStatusChanged { status: AecStatus },
    /// VB-CABLE installer progress line.
    InstallProgress { message: String },
    /// A UI-only action a graphical frontend should perform.
    UiRequest { request: UiRequest },
    /// Plugin set changed (install/uninstall/enable/disable/update);
    /// frontends should refresh `plugins/list`. Payload is the plugin id.
    PluginListChanged { plugin_id: String },
    /// A plugin wrote a log line.
    PluginLog {
        plugin_id: String,
        level: String,
        message: String,
    },
    /// Plugin marketplace download progress.
    PluginDownloadProgress {
        id: String,
        downloaded: u64,
        total: u64,
        done: bool,
    },
}

impl ServerEvent {
    /// Stable camelCase tag of the event (matches the JSON `type` field).
    /// Used for subscription prefix filtering.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::DeviceConnected { .. } => "deviceConnected",
            Self::DeviceDisconnected => "deviceDisconnected",
            Self::AudioMetrics { .. } => "audioMetrics",
            Self::AudioLevel { .. } => "audioLevel",
            Self::AudioSpectrum { .. } => "audioSpectrum",
            Self::MuteStateChanged { .. } => "muteStateChanged",
            Self::MonitoringChanged { .. } => "monitoringChanged",
            Self::SpectrumStreamingChanged { .. } => "spectrumStreamingChanged",
            Self::ServerStopped => "serverStopped",
            Self::WebClientCount { .. } => "webClientCount",
            Self::UdpAudioWarning => "udpAudioWarning",
            Self::AecStatusChanged { .. } => "aecStatusChanged",
            Self::InstallProgress { .. } => "installProgress",
            Self::UiRequest { .. } => "uiRequest",
            Self::PluginListChanged { .. } => "pluginListChanged",
            Self::PluginLog { .. } => "pluginLog",
            Self::PluginDownloadProgress { .. } => "pluginDownloadProgress",
        }
    }

    /// High-frequency events that sessions must explicitly subscribe to.
    pub fn is_high_frequency(&self) -> bool {
        matches!(
            self,
            Self::AudioLevel { .. } | Self::AudioMetrics { .. } | Self::AudioSpectrum { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialization_shape_is_tagged_camel_case() {
        let value = serde_json::to_value(ServerEvent::MuteStateChanged { muted: true }).unwrap();
        assert_eq!(
            value,
            json!({"type": "muteStateChanged", "data": {"muted": true}})
        );

        let value = serde_json::to_value(ServerEvent::UiRequest {
            request: UiRequest::OpenPluginPanel {
                plugin_id: "p1".into(),
                panel_id: "panel".into(),
            },
        })
        .unwrap();
        assert_eq!(value["type"], "uiRequest");
        assert_eq!(value["data"]["request"]["kind"], "openPluginPanel");
        assert_eq!(value["data"]["request"]["pluginId"], "p1");
    }

    #[test]
    fn tags_match_serializer() {
        let event = ServerEvent::PluginDownloadProgress {
            id: "x".into(),
            downloaded: 1,
            total: 2,
            done: false,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], event.tag());
        assert!(event.tag() == "pluginDownloadProgress");
    }

    #[test]
    fn high_frequency_set_covers_stream_events() {
        assert!(ServerEvent::AudioLevel { level: 1 }.is_high_frequency());
        assert!(!ServerEvent::ServerStopped.is_high_frequency());
    }
}

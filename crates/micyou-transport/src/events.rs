/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! Transport-layer observer contract.
//!
//! The transport crate knows nothing about the plugin framework, the RPC
//! layer or any frontend: everything it wants to report crosses this trait,
//! implemented by `micyou-core` (which fans the notifications out to the
//! event bus, the plugin host and the statistics store).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use micyou_protocol::micyou::MessageWrapper;
use serde::{Deserialize, Serialize};

use crate::stats::AudioMetrics;
use crate::tcp::DeviceInfo;

/// Connection mode of the running server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportMode {
    /// LAN streaming from the mobile app (mDNS discovery).
    #[default]
    Wifi,
    /// USB streaming via `adb reverse` port forwarding.
    Usb,
    /// Browser streaming over a TLS WebSocket (no mobile app needed).
    Web,
}

impl TransportMode {
    /// Parse the wire/config spelling (`"wifi" | "usb" | "web"`).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "wifi" => Some(Self::Wifi),
            "usb" => Some(Self::Usb),
            "web" => Some(Self::Web),
            _ => None,
        }
    }

    /// Config/wire spelling of the mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Usb => "usb",
            Self::Web => "web",
        }
    }
}

impl std::fmt::Display for TransportMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Runtime-tunable flags the transport reads instead of touching config
/// files. The core updates these whenever the persisted server preferences
/// change, keeping the hot path free of file I/O.
#[derive(Debug)]
pub struct TransportConfig {
    /// Whether mute state is synchronized with the mobile client in both
    /// directions (mirrors `server.json: muteSync`).
    pub mute_sync: AtomicBool,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            mute_sync: AtomicBool::new(true),
        }
    }
}

impl TransportConfig {
    /// Snapshot from persisted preferences.
    pub fn new(mute_sync: bool) -> Self {
        Self {
            mute_sync: AtomicBool::new(mute_sync),
        }
    }

    /// Update the mute-sync flag (called when server preferences are saved).
    pub fn set_mute_sync(&self, enabled: bool) {
        self.mute_sync.store(enabled, Ordering::Relaxed);
    }

    /// Current mute-sync flag.
    pub fn mute_sync_enabled(&self) -> bool {
        self.mute_sync.load(Ordering::Relaxed)
    }
}

/// A send handle for the TCP control channel of the currently connected
/// device. Handed to the core (via [`TransportEvents::control_channel_opened`])
/// so plugin messages and mute-sync updates can be pushed to the phone.
#[derive(Clone)]
pub struct ControlChannel {
    /// Transport connection id; identifies the owning session.
    pub id: u64,
    sender: tokio::sync::mpsc::Sender<MessageWrapper>,
}

impl ControlChannel {
    /// Wrap a session's control sender.
    pub fn new(id: u64, sender: tokio::sync::mpsc::Sender<MessageWrapper>) -> Self {
        Self { id, sender }
    }

    /// Non-blocking send; `false` when the session queue is full or closed.
    pub fn try_send(&self, message: MessageWrapper) -> bool {
        self.sender.try_send(message).is_ok()
    }

    /// Whether two handles target the same underlying channel.
    pub fn same_channel(&self, other: &ControlChannel) -> bool {
        self.sender.same_channel(&other.sender)
    }
}

impl std::fmt::Debug for ControlChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlChannel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Events emitted by the transport servers (TCP/UDP/web), decoupled from the
/// plugin framework, the RPC layer and any frontend. Implemented by
/// `micyou-core`; the original `ServerEvents` sink chain (GUI/CLI/TUI) lives
/// above the core event bus.
pub trait TransportEvents: Send + Sync + 'static {
    /// A device (phone or browser) became the active audio source.
    fn device_connected(&self, info: DeviceInfo);
    /// The active device disconnected.
    fn device_disconnected(&self);
    /// The mobile client changed its mute state (already filtered through
    /// [`TransportConfig::mute_sync`] by the transport).
    fn remote_mute_changed(&self, muted: bool);
    /// Periodic (1 Hz) network/audio metrics while a session is active.
    fn audio_metrics(&self, metrics: AudioMetrics);
    /// TCP is up but no UDP audio arrived for >10 s (Windows firewall hint).
    fn udp_audio_warning(&self);
    /// Number of connected web-mode browser clients changed.
    fn web_client_count(&self, count: u32);
    /// A cross-device plugin message arrived from the mobile client. The
    /// payload is still in wire form; the core converts and dispatches it on
    /// the plugin bus.
    fn plugin_message_received(&self, message: micyou_protocol::micyou::PluginMessage);
    /// The TCP control channel of the active session became available. The
    /// core points the plugin sync transport at it.
    fn control_channel_opened(&self, channel: ControlChannel);
    /// The session owning `connection_id` closed its control channel. The
    /// core clears the plugin sync transport if it still points at it.
    fn control_channel_closed(&self, connection_id: u64);
}

/// Shared handle to the active [`TransportEvents`] implementation.
pub type SharedTransportEvents = Arc<dyn TransportEvents>;

/// No-op sink for tests and standalone transport usage.
pub struct NullTransportEvents;

impl TransportEvents for NullTransportEvents {
    fn device_connected(&self, _info: DeviceInfo) {}
    fn device_disconnected(&self) {}
    fn remote_mute_changed(&self, _muted: bool) {}
    fn audio_metrics(&self, _metrics: AudioMetrics) {}
    fn udp_audio_warning(&self) {}
    fn web_client_count(&self, _count: u32) {}
    fn plugin_message_received(&self, _message: micyou_protocol::micyou::PluginMessage) {}
    fn control_channel_opened(&self, _channel: ControlChannel) {}
    fn control_channel_closed(&self, _connection_id: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parses_config_spellings() {
        assert_eq!(TransportMode::parse("wifi"), Some(TransportMode::Wifi));
        assert_eq!(TransportMode::parse("USB"), Some(TransportMode::Usb));
        assert_eq!(TransportMode::parse(" web "), Some(TransportMode::Web));
        assert_eq!(TransportMode::parse("bluetooth"), None);
        assert_eq!(TransportMode::Wifi.to_string(), "wifi");
    }

    #[test]
    fn config_mute_sync_roundtrips() {
        let config = TransportConfig::new(true);
        assert!(config.mute_sync_enabled());
        config.set_mute_sync(false);
        assert!(!config.mute_sync_enabled());
        // Defaults match server.json's default (mute sync enabled).
        assert!(TransportConfig::default().mute_sync_enabled());
    }

    #[test]
    fn control_channel_reports_closed_receiver() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let channel = ControlChannel::new(7, tx);
        drop(rx);
        assert!(!channel.try_send(MessageWrapper::default()));
    }
}

/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 * See LICENSE for details.
 */

//! Core event bus.
//!
//! Everything the backend wants to tell the outside world (frontends over
//! RPC, embedded hosts, plugins) is published here as a [`ServerEvent`].
//! The bus replaces the upstream `ServerEvents` trait-object sink chain:
//! instead of every frontend implementing a trait against the server core,
//! consumers subscribe to one broadcast channel and filter by event type.
//! The RPC layer maps these events onto JSON-RPC notifications.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use micyou_audio::AecFailure;
use micyou_transport::events::ControlChannel;
use micyou_transport::stats::AudioMetrics;
use micyou_transport::tcp::DeviceInfo;
use micyou_transport::TransportMode;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Capacity of the broadcast channel. High-frequency events (audio level at
/// ~8 Hz, metrics at 1 Hz) mean a subscriber stalled for seconds will lag;
/// lagged receivers resynchronize on the next event rather than blocking
/// publishers.
const EVENT_BUS_CAPACITY: usize = 512;

/// Events published by the backend core.
///
/// Serialized as `{"type": "<camelCase tag>", "data": <payload>}`; this shape
/// is part of the frontend contract (see `micyou-api`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum ServerEvent {
    /// A device (phone/web client) became the active audio source.
    DeviceConnected { device: DeviceInfo },
    /// The active device disconnected.
    DeviceDisconnected,
    /// Periodic network/audio metrics while streaming.
    AudioMetrics { metrics: AudioMetrics },
    /// Processed output level, 0..=100 (≈8 Hz while streaming).
    AudioLevel { level: u32 },
    /// Raw + processed spectrum bands (only while spectrum streaming is on).
    AudioSpectrum { raw: Vec<f32>, processed: Vec<f32> },
    /// Hard-mute state changed (any source: RPC, plugin, phone).
    MuteStateChanged { muted: bool },
    /// Ear-return monitoring toggled.
    MonitoringChanged { enabled: bool },
    /// Spectrum streaming toggled.
    SpectrumStreamingChanged { enabled: bool },
    /// The audio server stopped (spontaneously or via RPC).
    ServerStopped,
    /// Web-mode browser client count changed.
    WebClientCount { count: u32 },
    /// TCP up but no UDP audio for >10 s (firewall hint, Windows wifi).
    UdpAudioWarning,
    /// Acoustic echo cancellation availability changed.
    AecStatusChanged { status: AecStatus },
    /// VB-CABLE installer progress line.
    InstallProgress { message: String },
    /// A UI-only action requested by the backend (or a plugin) that only a
    /// graphical frontend can fulfil.
    UiRequest { request: UiRequest },
    /// A plugin was enabled/disabled/installed/uninstalled; frontends should
    /// refresh their plugin list. Payload is the plugin id.
    PluginListChanged { plugin_id: String },
    /// A plugin wrote a log line (mirrors the in-memory plugin log ring).
    PluginLog {
        plugin_id: String,
        level: String,
        message: String,
    },
}

/// AEC availability/state snapshot.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AecStatus {
    pub available: bool,
    pub enabled: bool,
    pub reason: Option<AecFailure>,
}

/// UI-side actions the backend cannot perform headlessly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UiRequest {
    /// Open a plugin's panel in a frontend-owned window.
    OpenPluginPanel { plugin_id: String, panel_id: String },
}

/// Fan-out broadcast bus for [`ServerEvent`]s.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Arc<ServerEvent>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(EVENT_BUS_CAPACITY)
    }
}

impl EventBus {
    /// Create a bus with the given per-receiver capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event. Cheap no-op when nobody is subscribed.
    pub fn publish(&self, event: ServerEvent) {
        // A send error only means "no receivers"; that is normal.
        let _ = self.tx.send(Arc::new(event));
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<ServerEvent>> {
        self.tx.subscribe()
    }

    /// Current number of live subscribers.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// Bridges transport notifications onto the core event bus and the plugin
/// framework. Installed as the `TransportEvents` implementation when the
/// server starts; holds the current [`TransportMode`] so plugin device
/// events carry the real mode (upstream hardcoded "wifi").
pub struct CoreTransportBridge {
    bus: EventBus,
    plugins: Arc<crate::plugins::PluginHost>,
    mode: AtomicU8,
}

fn mode_from_u8(value: u8) -> TransportMode {
    match value {
        1 => TransportMode::Usb,
        2 => TransportMode::Web,
        _ => TransportMode::Wifi,
    }
}

fn mode_to_u8(mode: TransportMode) -> u8 {
    match mode {
        TransportMode::Wifi => 0,
        TransportMode::Usb => 1,
        TransportMode::Web => 2,
    }
}

impl CoreTransportBridge {
    /// Create the bridge; `mode` is updated on every server start.
    pub fn new(bus: EventBus, plugins: Arc<crate::plugins::PluginHost>) -> Self {
        Self {
            bus,
            plugins,
            mode: AtomicU8::new(0),
        }
    }

    /// Record the mode of the (re)starting server.
    pub fn set_mode(&self, mode: TransportMode) {
        self.mode.store(mode_to_u8(mode), Ordering::Relaxed);
    }
}

impl micyou_transport::TransportEvents for CoreTransportBridge {
    fn device_connected(&self, info: DeviceInfo) {
        self.plugins
            .broadcast_event(&micyou_plugin::PluginEvent::DeviceConnected {
                mode: mode_from_u8(self.mode.load(Ordering::Relaxed)).to_string(),
                label: info.name.clone(),
            });
        self.bus
            .publish(ServerEvent::DeviceConnected { device: info });
    }

    fn device_disconnected(&self) {
        self.plugins
            .broadcast_event(&micyou_plugin::PluginEvent::DeviceDisconnected);
        self.bus.publish(ServerEvent::DeviceDisconnected);
    }

    fn remote_mute_changed(&self, muted: bool) {
        // The transport already applied the flag to the shared stats.
        self.plugins
            .broadcast_event(&micyou_plugin::PluginEvent::MuteChanged { muted });
        self.bus.publish(ServerEvent::MuteStateChanged { muted });
    }

    fn audio_metrics(&self, metrics: AudioMetrics) {
        self.bus.publish(ServerEvent::AudioMetrics { metrics });
    }

    fn udp_audio_warning(&self) {
        self.bus.publish(ServerEvent::UdpAudioWarning);
    }

    fn web_client_count(&self, count: u32) {
        self.bus.publish(ServerEvent::WebClientCount { count });
    }

    fn plugin_message_received(&self, message: micyou_protocol::micyou::PluginMessage) {
        let logical = micyou_plugin::sync::from_wire(&message);
        self.plugins.bus.handle_incoming(&logical);
    }

    fn control_channel_opened(&self, channel: ControlChannel) {
        self.plugins.sync.set_channel(Some(channel));
    }

    fn control_channel_closed(&self, connection_id: u64) {
        self.plugins.sync.clear_if(connection_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bus_delivers_published_events_to_subscribers() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        bus.publish(ServerEvent::AudioLevel { level: 42 });
        let received = rx.recv().await.expect("event");
        match &*received {
            ServerEvent::AudioLevel { level } => assert_eq!(*level, 42),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn publish_without_subscribers_is_a_noop() {
        let bus = EventBus::default();
        bus.publish(ServerEvent::ServerStopped);
        assert_eq!(bus.receiver_count(), 0);
    }

    #[test]
    fn events_serialize_with_camel_case_tags() {
        let json = serde_json::to_value(ServerEvent::MuteStateChanged { muted: true }).unwrap();
        assert_eq!(json["type"], "muteStateChanged");
        assert_eq!(json["data"]["muted"], true);

        let json = serde_json::to_value(ServerEvent::UiRequest {
            request: UiRequest::OpenPluginPanel {
                plugin_id: "p".into(),
                panel_id: "panel".into(),
            },
        })
        .unwrap();
        assert_eq!(json["type"], "uiRequest");
        assert_eq!(json["data"]["request"]["kind"], "openPluginPanel");
    }

    #[test]
    fn transport_mode_roundtrips_through_u8() {
        for mode in [TransportMode::Wifi, TransportMode::Usb, TransportMode::Web] {
            assert_eq!(mode_from_u8(mode_to_u8(mode)), mode);
        }
    }
}

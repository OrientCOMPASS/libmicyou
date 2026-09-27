/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! MicYou network transport: TCP/UDP/web servers, mDNS, ADB, jitter buffer
//! and stream validation.
//!
//! # Module map
//!
//! - [`tcp`] — control plane (handshake, takeover, ping/pong, mute sync,
//!   TCP-only audio, cross-device plugin message relay).
//! - [`udp`] — audio plane (magic-framed datagrams, session binding,
//!   loss/jitter measurement).
//! - [`web`] — browser mode: TLS WebSocket server with an embedded capture
//!   page (feature `web`, on by default).
//! - [`discovery`] — mDNS service advertisement for the mobile client.
//! - [`adb`] — `adb reverse` setup and device listing for USB mode.
//! - [`jitter`] — reorder buffer with XOR FEC recovery.
//! - [`stream`] — audio packet validation and session binding rules.
//! - [`stats`] — shared atomic session statistics and [`stats::AudioMetrics`].
//! - [`host_info`] — LAN interface enumeration for binding and QR display.
//! - [`events`] — the [`events::TransportEvents`] observer contract that
//!   decouples this crate from the core, plugins and frontends.

pub mod adb;
pub mod discovery;
pub mod events;
pub mod host_info;
pub mod jitter;
pub mod net;
pub mod stats;
pub mod stream;
pub mod tcp;
pub mod udp;
#[cfg(feature = "web")]
pub mod web;

pub use events::{
    ControlChannel, NullTransportEvents, SharedTransportEvents, TransportConfig, TransportEvents,
    TransportMode,
};
pub use host_info::{query_network_interfaces, NetworkInfo, NetworkInterfaceInfo};
pub use jitter::JitterBuffer;
pub use net::{format_url_host_str, normalize_ip, parse_bind, wildcard_dual};
pub use stats::{AudioMetrics, NetworkStats};
pub use stream::{AudioStreamEvent, ExpectedAudioSession};
pub use tcp::{ActiveConnection, DeviceInfo, SharedActiveConnection, SharedTakeoverLock};
pub use udp::{ActiveAudioSession, SharedActiveAudioSession};

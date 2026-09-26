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
pub use stats::{AudioMetrics, NetworkStats};
pub use stream::{AudioStreamEvent, ExpectedAudioSession};
pub use tcp::{ActiveConnection, DeviceInfo, SharedActiveConnection, SharedTakeoverLock};
pub use udp::{ActiveAudioSession, SharedActiveAudioSession};

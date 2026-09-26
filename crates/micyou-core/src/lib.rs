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

//! MicYou backend core: server lifecycle orchestration, audio pipeline,
//! shared configuration, platform virtual devices and the headless plugin
//! host.
//!
//! # Module map
//!
//! - [`server`] — [`server::ServerCore`]: the shared runtime and the
//!   start/stop transactions (this is the heart of the backend).
//! - [`lifecycle`] — start/stop state machine with bounded audio-thread join.
//! - [`pipeline`] — the dedicated audio thread: jitter buffer → decode →
//!   resample → DSP chain → virtual mic output.
//! - [`events`] — [`events::EventBus`] and the [`events::ServerEvent`]
//!   catalogue every frontend/RPC consumer subscribes to.
//! - [`plugins`] — headless plugin host: manager wiring, DSP registry,
//!   cross-device sync adapter, hotkeys, [`plugins::UiBridge`] delegation.
//! - [`config`] — the shared `~/.config/micyou/*.json` files (compatible
//!   with the stock GUI/CLI/TUI).
//! - [`platform`] — VB-CABLE / BlackHole / PipeWire virtual device helpers.
//! - [`resources`] — ONNX runtime and bundled model discovery.
//! - [`logging`] — dependency-free file+stderr `log` backend.
//! - [`mode_lock`] — single-backend guard shared with GUI/CLI/TUI.
//! - [`audio_output`], [`sound`] — persistent cpal device thread and plugin
//!   sound-effect playback.

pub mod audio_output;
pub mod config;
pub mod events;
pub mod lifecycle;
pub mod logging;
pub mod mode_lock;
pub mod pipeline;
pub mod platform;
pub mod plugins;
pub mod resources;
pub mod server;

pub use audio_output::AudioOutputHandle;
pub use events::{AecStatus, CoreTransportBridge, EventBus, ServerEvent, UiRequest};
pub use lifecycle::{ServerLifecyclePhase, ServerLifecycleState};
pub use plugins::{PluginHost, UiBridge, UiBridgeSlot};
pub use server::{ServerCore, StartParams};

/// Convenience re-exports for RPC/service layers.
pub mod prelude {
    pub use crate::events::{EventBus, ServerEvent, UiRequest};
    pub use crate::server::{ServerCore, StartParams};
    pub use micyou_transport::events::TransportMode;
}

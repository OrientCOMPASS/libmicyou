/*
 * libmicyou — micyou-gui frontend (original MicYou Vue UI).
 * Derived from MicYou <https://github.com/LanRhyme/MicYou> events.rs.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou event semantics)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou port)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Event bridge: daemon `ServerEvent`s → original Tauri event names.
//!
//! The ported Vue code listens to the stock MicYou event surface
//! (`audio-level`, `mute-state-changed`, `device-connected`, …) with the
//! stock payload shapes (bare numbers/booleans/strings where upstream emitted
//! them). The libmicyou daemon instead publishes a single tagged
//! `ServerEvent` enum over JSON-RPC notifications; this module unwraps each
//! variant back into the upstream wire shape.

use micyou_api::events::{ServerEvent, UiRequest};
use micyou_client::Client;
use tauri::{AppHandle, Emitter, Manager};

/// Forward one backend event to the webview(s).
pub async fn forward(app: &AppHandle, client: &Client, event: &ServerEvent) {
    match event {
        ServerEvent::DeviceConnected { device } => {
            let _ = app.emit("device-connected", device);
        }
        ServerEvent::DeviceDisconnected => {
            let _ = app.emit("device-disconnected", ());
        }
        ServerEvent::AudioMetrics { metrics } => {
            let _ = app.emit("audio-metrics", metrics);
        }
        ServerEvent::AudioLevel { level } => {
            // Upstream emitted the bare u32; Vue reads `event.payload` as number.
            let _ = app.emit("audio-level", *level);
        }
        ServerEvent::AudioSpectrum { raw, processed } => {
            // Upstream scoped the spectrum to the main window only.
            if let Some(main_window) = app.get_webview_window("main") {
                let _ = main_window.emit(
                    "audio-spectrum",
                    serde_json::json!({ "raw": raw, "processed": processed }),
                );
            }
        }
        ServerEvent::MuteStateChanged { muted } => {
            let _ = app.emit("mute-state-changed", *muted);
        }
        ServerEvent::MonitoringChanged { enabled } => {
            let _ = app.emit("monitoring-enabled-changed", *enabled);
        }
        ServerEvent::SpectrumStreamingChanged { .. } => {
            // The Vue UI drives spectrum streaming itself; no listener.
        }
        ServerEvent::ServerStopped => {
            let _ = app.emit("server-stopped", ());
        }
        ServerEvent::WebClientCount { count } => {
            let _ = app.emit("web-client-count", *count);
        }
        ServerEvent::UdpAudioWarning => {
            let _ = app.emit("udp_audio_warning", ());
        }
        ServerEvent::AecStatusChanged { status } => {
            let _ = app.emit("aec-status-changed", status);
        }
        ServerEvent::InstallProgress { message } => {
            // Upstream emitted the bare progress line for the VB-CABLE wizard.
            let _ = app.emit("vbcable-install-progress", message);
        }
        ServerEvent::PluginDownloadProgress {
            id,
            downloaded,
            total,
            done,
        } => {
            let _ = app.emit(
                "plugin-download-progress",
                serde_json::json!({
                    "id": id,
                    "downloaded": downloaded,
                    "total": total,
                    "done": done,
                }),
            );
        }
        ServerEvent::UiRequest { request } => match request {
            UiRequest::OpenPluginPanel {
                plugin_id,
                panel_id,
            } => {
                if let Err(e) =
                    crate::windows::open_plugin_panel(app, client, plugin_id, panel_id).await
                {
                    log::warn!("open plugin panel {plugin_id}/{panel_id} failed: {e}");
                }
            }
        },
        ServerEvent::PluginListChanged { .. } | ServerEvent::PluginLog { .. } => {
            // The Vue UI refreshes plugin state on demand; log lines stay in
            // the daemon log (reachable via plugins/logs).
        }
    }
}

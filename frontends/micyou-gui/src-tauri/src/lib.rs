/*
 * libmicyou — micyou-gui frontend (original MicYou Vue UI).
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou desktop application)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou port)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! The original MicYou desktop GUI (Vue 3 + Tailwind, ported verbatim from
//! upstream `tauri-app`) running on the experimental libmicyou backend.
//!
//! Architecture:
//! - The Vue bundle is unmodified upstream code. A build-time shim
//!   (`src/adapter/tauri-core.ts`, injected by `vite.config.ts`) maps its
//!   `invoke()` command surface onto either local shell commands (below) or
//!   the single `backend_rpc` bridge.
//! - All business logic lives in the `micyou-daemon` sidecar, spawned with
//!   `--stdio` and driven over newline-delimited JSON-RPC (see `backend.rs`).
//! - Daemon `ServerEvent`s are re-emitted under the original Tauri event
//!   names/payloads (see `events.rs`), so the Vue listeners work unchanged.

#![allow(unexpected_cfgs)]

pub mod accent;
pub mod backend;
pub mod commands;
pub mod events;
pub mod rpc;
pub mod shelllog;
pub mod theme;
pub mod tray;
pub mod windows;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(backend::BackendState::default())
        .manage(tray::TrayContext::default())
        .setup(|app| {
            shelllog::init(app.handle());
            log::info!(
                "micyou-gui {} starting (libmicyou backend)",
                env!("CARGO_PKG_VERSION")
            );

            if let Err(e) = tray::build_tray(app.handle()) {
                log::warn!(target: "tray", "failed to build tray: {e}");
            }

            // Spawn + supervise the daemon sidecar (connect, hello,
            // subscribe, event pump, auto-respawn).
            backend::spawn_session_manager(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Generic JSON-RPC bridge used by the webview adapter.
            rpc::backend_rpc,
            // Shell-local commands (original Tauri command names).
            commands::exit_app,
            commands::show_main_window,
            commands::hide_main_window,
            commands::minimize_main_window,
            commands::start_window_drag,
            commands::set_tray_state,
            commands::set_tray_strings,
            commands::get_app_version,
            commands::get_log_path,
            commands::get_log_content,
            commands::export_log,
            commands::open_log_dir,
            commands::open_plugins_dir,
            commands::switch_to_cli,
            commands::switch_to_tui,
            accent::get_system_accent_color,
            theme::install_theme,
            theme::list_installed_themes,
            theme::get_installed_theme,
            theme::remove_installed_theme,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

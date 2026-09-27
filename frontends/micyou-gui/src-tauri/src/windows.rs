/*
 * libmicyou — micyou-gui frontend (original MicYou Vue UI).
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>
 * commands/plugins.rs (open_plugin_window_impl).
 *
 * Copyright (C) 2026 LanRhyme (original MicYou window semantics)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou port)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Runtime window creation owned by the shell.
//!
//! Plugin panels are requested by the daemon (`uiRequest::openPluginPanel`,
//! e.g. a soundpad plugin calling `host.open_window`); the shell resolves the
//! suggested title via `plugins/panel` and opens the same
//! `index.html#/plugin/<id>/<panel>` route the stock GUI used.

use micyou_api::methods::PluginPanel;
use micyou_client::Client;
use tauri::{AppHandle, Manager};

/// Open (or focus) the window hosting a plugin panel.
pub async fn open_plugin_panel(
    app: &AppHandle,
    client: &Client,
    plugin_id: &str,
    panel_id: &str,
) -> Result<(), String> {
    let label = format!("plugin-window-{}", plugin_id.replace('.', "-"));
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
        return Ok(());
    }

    // Round-trip the backend for the panel document: validates that the
    // plugin/panel exist and yields the suggested window title.
    let panel: PluginPanel = client
        .call(
            "plugins/panel",
            serde_json::json!({ "pluginId": plugin_id, "panelId": panel_id }),
        )
        .await
        .map_err(crate::backend::rpc_err)?;

    tauri::WebviewWindowBuilder::new(
        app,
        &label,
        tauri::WebviewUrl::App(format!("index.html#/plugin/{plugin_id}/{panel_id}").into()),
    )
    .title(&panel.title)
    .inner_size(520.0, 720.0)
    .min_inner_size(360.0, 480.0)
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

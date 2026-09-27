/*
 * libmicyou — micyou-gui frontend (original MicYou Vue UI).
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>
 * commands/{system,about,plugins}.rs.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou command semantics)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou port)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Shell-local Tauri commands: everything that is a GUI concern (windows,
//! tray, dialogs, shell log files) plus the run-mode stubs. Backend-domain
//! commands are not re-implemented here — the webview adapter routes them
//! through `backend_rpc` (see src/adapter/tauri-core.ts).

use serde_json::Value;
use tauri::{AppHandle, Manager, State};

use crate::backend::{rpc_state, BackendState};
use crate::shelllog;
use crate::tray::{rebuild_menu, TrayContext, TrayMenuStrings, TrayState};

fn main_window<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<tauri::WebviewWindow<R>, String> {
    app.get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())
}

// ── window management ────────────────────────────────────────────────────────

#[tauri::command]
pub fn show_main_window(app: AppHandle) -> Result<(), String> {
    let win = main_window(&app)?;
    let _ = win.unminimize();
    win.show().map_err(|e| e.to_string())?;
    win.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn minimize_main_window(app: AppHandle) -> Result<(), String> {
    let win = main_window(&app)?;

    // Wayland compositors may ignore xdg_toplevel.set_minimized. Hiding the
    // window keeps the minimize-to-tray action reliable across Linux WMs.
    #[cfg(target_os = "linux")]
    return win.hide().map_err(|e| e.to_string());

    #[cfg(not(target_os = "linux"))]
    win.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn hide_main_window(app: AppHandle) -> Result<(), String> {
    let win = main_window(&app)?;
    win.hide().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn start_window_drag(app: AppHandle) -> Result<(), String> {
    let win = main_window(&app)?;
    win.start_dragging().map_err(|e| e.to_string())
}

// ── tray ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn set_tray_strings(app: AppHandle, strings: TrayMenuStrings) -> Result<(), String> {
    {
        let ctx = app.state::<TrayContext>();
        *ctx.strings.lock().map_err(|e| e.to_string())? = strings;
    }
    rebuild_menu(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_tray_state(app: AppHandle, state: TrayState) -> Result<(), String> {
    {
        let ctx = app.state::<TrayContext>();
        *ctx.state.lock().map_err(|e| e.to_string())? = state;
    }
    rebuild_menu(&app).map_err(|e| e.to_string())
}

// ── lifecycle ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn exit_app(app: AppHandle, state: State<'_, BackendState>) -> Result<(), String> {
    // Best-effort server stop so listeners (phones/browsers) see a clean
    // shutdown; the daemon child itself exits on stdin EOF once we're gone.
    if state.client.lock().await.is_some() {
        let _ = rpc_state(&state, "server/stop", Value::Null).await;
    }
    log::info!(target: "tray", "exit_app: stopping application");
    app.exit(0);
    Ok(())
}

/// The stock GUI spawns bundled `micyou-cli` sidecars here; this experimental
/// frontend ships GUI + daemon only.
#[tauri::command]
pub fn switch_to_cli() -> Result<(), String> {
    Err("CLI mode is not bundled with the libmicyou experimental GUI \
       (no micyou-cli sidecar); run micyou-daemon / a CLI frontend separately"
        .to_string())
}

/// See [`switch_to_cli`].
#[tauri::command]
pub fn switch_to_tui() -> Result<(), String> {
    Err("TUI mode is not bundled with the libmicyou experimental GUI \
       (no micyou-tui sidecar); run micyou-daemon / a TUI frontend separately"
        .to_string())
}

// ── about / logs ─────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
pub fn get_log_path(app: AppHandle) -> Result<String, String> {
    let log_file = shelllog::log_file_path(&app)?;
    Ok(log_file.display().to_string())
}

#[tauri::command]
pub fn get_log_content(app: AppHandle) -> Result<String, String> {
    let log_file = shelllog::log_file_path(&app)?;
    if !log_file.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(&log_file).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn export_log(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;

    let log_file = shelllog::log_file_path(&app)?;
    if !log_file.exists() {
        return Err("Log file not found".to_string());
    }

    app.dialog().file().save_file(move |file_path| {
        if let Some(path) = file_path {
            if let Ok(destination) = path.into_path() {
                let _ = std::fs::copy(&log_file, destination);
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub fn open_log_dir(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_opener::OpenerExt;

    let log_dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(log_dir.display().to_string(), None::<&str>)
        .map_err(|e| format!("open log dir: {e}"))?;
    Ok(log_dir.display().to_string())
}

// ── plugins directory ────────────────────────────────────────────────────────

/// Ask the backend where the plugins directory lives, then reveal it in the
/// system file manager (upstream did both inside one command).
#[tauri::command]
pub async fn open_plugins_dir(
    app: AppHandle,
    state: State<'_, BackendState>,
) -> Result<String, String> {
    use tauri_plugin_opener::OpenerExt;

    let result = rpc_state(&state, "plugins/dir", Value::Null).await?;
    let path = result
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "malformed plugins/dir result".to_string())?
        .to_string();
    app.opener()
        .open_path(path.clone(), None::<&str>)
        .map_err(|e| format!("open plugins dir: {e}"))?;
    Ok(path)
}

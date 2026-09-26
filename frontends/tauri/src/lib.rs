/*
 * libmicyou — Tauri 2 reference frontend.
 *
 * Copyright (C) 2026 OrientCOMPASS
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Tauri shell: thin commands over the backend session + event forwarding to
//! the webview. All business logic lives in the daemon; this layer only
//! translates between Tauri's invoke/event model and the JSON-RPC client.

pub mod controller;

use controller::Session;
use micyou_api::methods::ServerStatus;
use tauri::{Emitter, Manager, State};
use tokio::sync::Mutex;

struct AppState {
    session: Mutex<Option<Session>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
        }
    }
}

const NO_SESSION: &str = "backend session not connected";

#[tauri::command]
async fn backend_connected(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.session.lock().await.is_some())
}

#[tauri::command]
async fn backend_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    let guard = state.session.lock().await;
    let session = guard.as_ref().ok_or(NO_SESSION)?;
    session.status().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn backend_start(
    state: State<'_, AppState>,
    port: Option<u16>,
    mode: Option<String>,
) -> Result<String, String> {
    let port = port.unwrap_or(18554);
    let mode = mode.unwrap_or_else(|| "wifi".to_string());
    let guard = state.session.lock().await;
    let session = guard.as_ref().ok_or(NO_SESSION)?;
    session.start(port, &mode).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn backend_stop(state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.session.lock().await;
    let session = guard.as_ref().ok_or(NO_SESSION)?;
    session.stop().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn backend_set_mute(state: State<'_, AppState>, muted: bool) -> Result<(), String> {
    let guard = state.session.lock().await;
    let session = guard.as_ref().ok_or(NO_SESSION)?;
    session.set_muted(muted).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn backend_set_monitoring(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let guard = state.session.lock().await;
    let session = guard.as_ref().ok_or(NO_SESSION)?;
    session
        .set_monitoring(enabled)
        .await
        .map_err(|e| e.to_string())
}

/// Generic JSON-RPC pass-through: the webview drives the entire backend
/// contract through this single command (params/result are raw JSON).
#[tauri::command]
async fn backend_call(
    state: State<'_, AppState>,
    method: String,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let guard = state.session.lock().await;
    let session = guard.as_ref().ok_or(NO_SESSION)?;
    session
        .call_raw(&method, params)
        .await
        .map_err(|e| e.to_string())
}

/// Backend identity reported at session/hello.
#[tauri::command]
async fn backend_info(
    state: State<'_, AppState>,
) -> Result<Option<micyou_api::methods::SessionInfo>, String> {
    let guard = state.session.lock().await;
    Ok(guard.as_ref().map(|s| s.info().clone()))
}

pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match Session::sidecar().await {
                    Ok(session) => {
                        let _ = handle.emit("backend-state", "connected");
                        let mut events = session.events();
                        {
                            let state = handle.state::<AppState>();
                            *state.session.lock().await = Some(session);
                        }
                        // Forward backend events verbatim to the webview.
                        loop {
                            match events.recv().await {
                                Ok(event) => {
                                    let payload = serde_json::to_value(&*event).ok();
                                    if let Some(payload) = payload {
                                        let _ = handle.emit("backend-event", payload);
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("daemon sidecar connection failed: {e}");
                        let _ = handle.emit("backend-state", format!("error: {e}"));
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Drop the session (kills the sidecar child via Client::drop).
                let handle = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(state) = handle.try_state::<AppState>() {
                        let mut guard = state.session.lock().await;
                        *guard = None;
                    }
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            backend_connected,
            backend_status,
            backend_start,
            backend_stop,
            backend_set_mute,
            backend_set_monitoring,
            backend_call,
            backend_info
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

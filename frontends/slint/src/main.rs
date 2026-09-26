/*
 * libmicyou — Slint reference frontend.
 *
 * Copyright (C) 2026 OrientCOMPASS
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Slint reference frontend: embeds the libmicyou backend in-process and
//! drives it over the local JSON-RPC transport. Doubles as a live example of
//! the client SDK for native-toolkit frontends.

use std::sync::Arc;

use micyou_api::events::ServerEvent;
use micyou_slint_frontend::controller::Session;
use slint::{ComponentHandle as _, SharedString, Weak};

slint::include_modules!();

/// Commands from the UI thread to the controller task.
enum UiCommand {
    Start { port: u16, mode: String },
    Stop,
    Mute(bool),
    Monitoring(bool),
}

fn post_message(ui: &Weak<MainWindow>, line: &str) {
    let ui = ui.clone();
    let line = line.to_string();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            let current = ui.get_message().to_string();
            let next = if current.is_empty() {
                line
            } else {
                // keep the newest ~40 lines
                let mut lines: Vec<&str> = current.lines().collect();
                lines.push(line.as_str());
                if lines.len() > 40 {
                    let drop = lines.len() - 40;
                    lines.drain(..drop);
                }
                lines.join("\n")
            };
            ui.set_message(SharedString::from(next));
        }
    });
}

fn apply_status(ui: &Weak<MainWindow>, status: &micyou_api::methods::ServerStatus) {
    let ui = ui.clone();
    let phase = status.phase.clone();
    let connected = status.is_connected;
    let muted = status.is_muted;
    let monitoring = status.is_monitoring;
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            ui.set_phase(SharedString::from(phase));
            ui.set_connected(connected);
            ui.set_muted(muted);
            ui.set_monitoring(monitoring);
        }
    });
}

async fn refresh(session: &Session, ui: &Weak<MainWindow>) {
    match session.status().await {
        Ok(status) => apply_status(ui, &status),
        Err(e) => post_message(ui, &format!("status query failed: {e}")),
    }
}

fn main() {
    let ui = MainWindow::new().expect("create main window");

    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<UiCommand>();

    {
        let tx = cmd_tx.clone();
        ui.on_start_clicked(move |port, mode| {
            let port: u16 = port.to_string().trim().parse().unwrap_or(18554);
            let mode = mode.to_string().trim().to_lowercase();
            let mode = if ["wifi", "usb", "web"].contains(&mode.as_str()) {
                mode
            } else {
                "wifi".to_string()
            };
            let _ = tx.send(UiCommand::Start { port, mode });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_stop_clicked(move || {
            let _ = tx.send(UiCommand::Stop);
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_mute_toggled(move |muted| {
            let _ = tx.send(UiCommand::Mute(muted));
        });
    }
    {
        let tx = cmd_tx;
        ui.on_monitoring_toggled(move |enabled| {
            let _ = tx.send(UiCommand::Monitoring(enabled));
        });
    }

    // Controller task: owns the tokio runtime and the backend session.
    let ui_weak = ui.as_weak();
    std::thread::Builder::new()
        .name("slint-controller".into())
        .spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            runtime.block_on(async move {
                let session = match Session::embedded().await {
                    Ok(session) => session,
                    Err(e) => {
                        post_message(&ui_weak, &format!("backend connect failed: {e}"));
                        return;
                    }
                };
                post_message(&ui_weak, "backend connected (in-process local transport)");
                let mut events = session.events();
                refresh(&session, &ui_weak).await;

                loop {
                    tokio::select! {
                        maybe_cmd = cmd_rx.recv() => {
                            let Some(cmd) = maybe_cmd else { break };
                            match cmd {
                                UiCommand::Start { port, mode } => {
                                    post_message(&ui_weak, &format!("starting server on {port} ({mode})…"));
                                    match session.start(port, &mode).await {
                                        Ok(message) => post_message(&ui_weak, &message),
                                        Err(e) => post_message(&ui_weak, &format!("start failed: {e}")),
                                    }
                                }
                                UiCommand::Stop => match session.stop().await {
                                    Ok(message) => post_message(&ui_weak, &message),
                                    Err(e) => post_message(&ui_weak, &format!("stop failed: {e}")),
                                },
                                UiCommand::Mute(muted) => {
                                    if let Err(e) = session.set_muted(muted).await {
                                        post_message(&ui_weak, &format!("mute failed: {e}"));
                                    }
                                }
                                UiCommand::Monitoring(enabled) => {
                                    if let Err(e) = session.set_monitoring(enabled).await {
                                        post_message(&ui_weak, &format!("monitoring failed: {e}"));
                                    }
                                }
                            }
                            refresh(&session, &ui_weak).await;
                        }
                        event = events.recv() => {
                            match event {
                                Ok(event) => handle_event(&ui_weak, &event),
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    log::debug!("event stream lagged by {n}");
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    }
                }
                session.shutdown();
            });
        })
        .expect("spawn controller thread");

    ui.run().expect("run slint event loop");
}

fn handle_event(ui: &Weak<MainWindow>, event: &Arc<ServerEvent>) {
    match &**event {
        ServerEvent::AudioLevel { level } => {
            let ui = ui.clone();
            let level = *level;
            let text = format!("Level: {level}%");
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui.upgrade() {
                    ui.set_level(level as f32);
                    ui.set_level_text(SharedString::from(text));
                }
            });
        }
        ServerEvent::MuteStateChanged { muted } => {
            let ui = ui.clone();
            let muted = *muted;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui.upgrade() {
                    ui.set_muted(muted);
                }
            });
        }
        ServerEvent::MonitoringChanged { enabled } => {
            let ui = ui.clone();
            let enabled = *enabled;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui.upgrade() {
                    ui.set_monitoring(enabled);
                }
            });
        }
        ServerEvent::DeviceConnected { device } => {
            post_message(
                ui,
                &format!("device connected: {} ({})", device.name, device.ip),
            );
            let _ = slint::invoke_from_event_loop({
                let ui = ui.clone();
                move || {
                    if let Some(ui) = ui.upgrade() {
                        ui.set_connected(true);
                    }
                }
            });
        }
        ServerEvent::DeviceDisconnected => {
            post_message(ui, "device disconnected");
            let _ = slint::invoke_from_event_loop({
                let ui = ui.clone();
                move || {
                    if let Some(ui) = ui.upgrade() {
                        ui.set_connected(false);
                        ui.set_level(0.0);
                    }
                }
            });
        }
        ServerEvent::ServerStopped => {
            post_message(ui, "server stopped");
            let _ = slint::invoke_from_event_loop({
                let ui = ui.clone();
                move || {
                    if let Some(ui) = ui.upgrade() {
                        ui.set_phase(SharedString::from("stopped"));
                        ui.set_connected(false);
                    }
                }
            });
        }
        ServerEvent::UdpAudioWarning => {
            post_message(ui, "warning: no UDP audio received (firewall?)");
        }
        other => {
            log::debug!("event: {}", other.tag());
        }
    }
}

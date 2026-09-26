/*
 * libmicyou — Tauri 2 reference frontend.
 *
 * Copyright (C) 2026 OrientCOMPASS
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Backend session logic for the Tauri frontend, GUI-free so CI can drive it
//! headless (`tests/session.rs`).
//!
//! The production path spawns `micyou-daemon --stdio` as a sidecar child and
//! speaks newline-delimited JSON-RPC over its pipes — the classic
//! frontend/backend split. Tests use the embedded (in-process) constructor.

use std::sync::Arc;

use micyou_api::methods::{
    MonitoringParams, MuteParams, ServerStatus, StartServerParams, VersionInfo,
};
use micyou_client::{Client, ClientError};

/// Resolve the daemon binary: `$MICYOU_DAEMON` → sibling of this executable →
/// `micyou-daemon` on PATH.
pub fn daemon_command() -> tokio::process::Command {
    let mut command = if let Ok(path) = std::env::var("MICYOU_DAEMON") {
        tokio::process::Command::new(path)
    } else if let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
        .map(|dir| {
            dir.join(if cfg!(windows) {
                "micyou-daemon.exe"
            } else {
                "micyou-daemon"
            })
        })
        .filter(|path| path.exists())
    {
        tokio::process::Command::new(sibling)
    } else {
        tokio::process::Command::new(if cfg!(windows) {
            "micyou-daemon.exe"
        } else {
            "micyou-daemon"
        })
    };
    command.arg("--stdio").arg("--no-mode-lock");
    command
}

/// A connected backend session.
pub struct Session {
    client: Client,
    /// Identity/capabilities returned by `session/hello`.
    info: micyou_api::methods::SessionInfo,
    /// Owning handle for embedded sessions (None for sidecar).
    _managed: Option<Arc<libmicyou::Managed>>,
}

impl Session {
    /// Spawn the daemon sidecar and connect over stdio.
    pub async fn sidecar() -> Result<Self, ClientError> {
        let mut command = daemon_command();
        log::info!("spawning daemon sidecar: {command:?}");
        let client = Client::connect_stdio(&mut command).await?;
        let mut session = Self {
            client,
            info: placeholder_info(),
            _managed: None,
        };
        session.info = session.handshake("tauri-frontend").await?;
        Ok(session)
    }

    /// Embed the backend in-process (headless tests, single-binary mode).
    pub async fn embedded() -> Result<Self, ClientError> {
        let managed = libmicyou::Builder::new()
            .log_level(log::LevelFilter::Warn)
            .log_to_file(false)
            .mode_lock(false)
            .build()
            .map_err(|e| ClientError::Transport(format!("build backend: {e}")))?;
        let conn = managed.attach_local();
        let client = Client::connect_channels(conn.inbound, conn.outbound);
        let mut session = Self {
            client,
            info: placeholder_info(),
            _managed: Some(Arc::new(managed)),
        };
        session.info = session.handshake("tauri-frontend-embedded").await?;
        Ok(session)
    }

    async fn handshake(&self, name: &str) -> Result<micyou_api::methods::SessionInfo, ClientError> {
        let info = self.client.hello(name, true).await?;
        self.client.subscribe(&["*"]).await?;
        Ok(info)
    }

    /// Identity reported by the backend at hello.
    pub fn info(&self) -> &micyou_api::methods::SessionInfo {
        &self.info
    }

    /// Generic pass-through to any contract method (the webview UI drives
    /// the full surface through this single Tauri command).
    pub async fn call_raw(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, ClientError> {
        self.client
            .call::<serde_json::Value, serde_json::Value>(
                method,
                params.unwrap_or_else(|| serde_json::json!({})),
            )
            .await
    }

    pub fn events(&self) -> tokio::sync::broadcast::Receiver<Arc<micyou_api::events::ServerEvent>> {
        self.client.events()
    }

    pub async fn version(&self) -> Result<VersionInfo, ClientError> {
        self.client.version().await
    }

    pub async fn status(&self) -> Result<ServerStatus, ClientError> {
        self.client.server_status().await
    }

    pub async fn start(&self, port: u16, mode: &str) -> Result<String, ClientError> {
        let result = self
            .client
            .start_server(StartServerParams {
                port: Some(port),
                mode: Some(mode.to_string()),
                bind_address: None,
                output_device: None,
                usb_device_serial: None,
            })
            .await?;
        Ok(result.message)
    }

    pub async fn stop(&self) -> Result<String, ClientError> {
        let result = self.client.stop_server().await?;
        Ok(result.message)
    }

    pub async fn set_muted(&self, muted: bool) -> Result<(), ClientError> {
        let _: micyou_api::methods::Ack = self
            .client
            .call(micyou_api::methods::AUDIO_MUTE_SET, MuteParams { muted })
            .await?;
        Ok(())
    }

    pub async fn set_monitoring(&self, enabled: bool) -> Result<(), ClientError> {
        let _: micyou_api::methods::Ack = self
            .client
            .call(
                micyou_api::methods::AUDIO_MONITORING_SET,
                MonitoringParams { enabled },
            )
            .await?;
        Ok(())
    }
}

fn placeholder_info() -> micyou_api::methods::SessionInfo {
    micyou_api::methods::SessionInfo {
        backend: String::new(),
        version: String::new(),
        api_version: 0,
        os: String::new(),
        arch: String::new(),
    }
}

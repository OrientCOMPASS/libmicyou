/*
 * libmicyou — Slint reference frontend.
 *
 * Copyright (C) 2026 OrientCOMPASS
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Backend session logic, deliberately GUI-free so it can run headless in CI
//! (`tests/session.rs`) and be driven by the Slint shell (`main.rs`).
//!
//! This frontend **embeds** the backend in-process and talks to it through
//! the local JSON-RPC transport — exercising `libmicyou::Builder`,
//! `Managed::attach_local`, the router, sessions and the event pump.

use std::sync::Arc;

use micyou_api::methods::{MonitoringParams, MuteParams, ServerStatus, StartServerParams};
use micyou_client::{Client, ClientError};

/// A connected backend session with the few calls this frontend needs.
pub struct Session {
    client: Client,
    /// Kept alive for embedded sessions (owns the backend); None for remote.
    _managed: Option<Arc<libmicyou::Managed>>,
}

impl Session {
    /// Embed the backend in this process (config isolated via
    /// `MICYOU_CONFIG_DIR` when set by the caller/tests).
    pub async fn embedded() -> Result<Self, ClientError> {
        let managed = libmicyou::Builder::new()
            .log_level(log::LevelFilter::Warn)
            .log_to_file(false)
            .mode_lock(false)
            .build()
            .map_err(|e| ClientError::Transport(format!("build backend: {e}")))?;
        let conn = managed.attach_local();
        let client = Client::connect_channels(conn.inbound, conn.outbound);
        let session = Self {
            client,
            _managed: Some(Arc::new(managed)),
        };
        session.handshake("slint-frontend").await?;
        Ok(session)
    }

    async fn handshake(&self, name: &str) -> Result<(), ClientError> {
        self.client.hello(name, true).await?;
        self.client.subscribe(&["*"]).await?;
        Ok(())
    }

    /// Independent event stream receiver.
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<Arc<micyou_api::events::ServerEvent>> {
        self.client.events()
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

    /// Clean shutdown for embedded sessions.
    pub fn shutdown(&self) {
        if let Some(managed) = &self._managed {
            managed.shutdown();
        }
    }
}

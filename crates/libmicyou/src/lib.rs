/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 *
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! libmicyou — the embeddable MicYou backend facade.
//!
//! One entry point for hosts that want the whole stack:
//!
//! ```no_run
//! use libmicyou::Builder;
//!
//! # async fn run() -> Result<(), String> {
//! let managed = Builder::new().log_mirror(true).build()?;
//! managed.ensure_audio_output();
//! managed.serve_stdio().await;
//! managed.shutdown();
//! # Ok(())
//! # }
//! ```
//!
//! Frontends talk to [`Managed`] through any of the three equivalent
//! transports — stdio pipes (`serve_stdio`), WebSocket (`serve_ws`) or an
//! in-process channel pair ([`Managed::attach_local`]) — all speaking the
//! same JSON-RPC contract defined by [`micyou_api`]. Hosts needing finer
//! control can compose [`micyou_core::Backend`] and [`micyou_rpc::RpcService`]
//! directly; every crate in the stack is public API.

use std::path::PathBuf;
use std::sync::Arc;

use micyou_core::service::Backend;
use micyou_rpc::local::LocalConnection;
use micyou_rpc::router::RpcService;

pub use micyou_api;
pub use micyou_audio;
pub use micyou_core;
pub use micyou_plugin;
pub use micyou_protocol;
pub use micyou_rpc;
pub use micyou_transport;

pub use micyou_core::events::{EventBus, ServerEvent};
pub use micyou_core::mode_lock::RunMode;
pub use micyou_core::{ServerCore, UiBridge};
pub use micyou_transport::TransportMode;

/// Configures and constructs a managed backend.
#[derive(Debug, Clone)]
pub struct Builder {
    config_dir: Option<PathBuf>,
    resource_dir: Option<PathBuf>,
    log_level: log::LevelFilter,
    log_mirror: bool,
    log_to_file: bool,
    mode_lock: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    /// Builder with daemon-friendly defaults: file+stderr logging off/on by
    /// flag, mode lock enabled, autodetected resources.
    pub fn new() -> Self {
        Self {
            config_dir: None,
            resource_dir: None,
            log_level: log::LevelFilter::Info,
            log_mirror: false,
            log_to_file: true,
            mode_lock: true,
        }
    }

    /// Override the shared config directory (`settings.json`, plugins, …).
    pub fn config_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config_dir = Some(dir.into());
        self
    }

    /// Explicit resource directory (ONNX models / ALSA config).
    pub fn resource_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.resource_dir = Some(dir.into());
        self
    }

    /// Log verbosity.
    pub fn log_level(mut self, level: log::LevelFilter) -> Self {
        self.log_level = level;
        self
    }

    /// Mirror log lines to stderr (foreground runs).
    pub fn log_mirror(mut self, enabled: bool) -> Self {
        self.log_mirror = enabled;
        self
    }

    /// Write logs to `<config_dir>/logs/daemon.log` (default true).
    pub fn log_to_file(mut self, enabled: bool) -> Self {
        self.log_to_file = enabled;
        self
    }

    /// Take the shared `mode.lock` (mutual exclusion with GUI/CLI/TUI and
    /// other daemons). Disable for tests or deliberately parallel instances.
    pub fn mode_lock(mut self, enabled: bool) -> Self {
        self.mode_lock = enabled;
        self
    }

    /// Initialize logging, take the mode lock (if enabled) and construct the
    /// backend + RPC service.
    pub fn build(self) -> Result<Managed, String> {
        if let Some(dir) = &self.config_dir {
            micyou_core::config::set_config_dir(dir.clone());
        }

        if self.log_to_file {
            micyou_core::logging::init(self.log_level, self.log_mirror)
                .map_err(|e| format!("init logging: {e}"))?;
        } else {
            micyou_core::logging::init_stderr_only(self.log_level, self.log_mirror);
        }

        if self.mode_lock {
            micyou_core::mode_lock::acquire(RunMode::Daemon)?;
        }

        let mut backend = Backend::new();
        backend.resource_dir = self.resource_dir.clone();
        let backend = Arc::new(backend);
        let rpc = RpcService::new(backend.clone());
        rpc.spawn_event_pump();

        log::info!(
            "libmicyou {} (api v{}) ready; config={}",
            env!("CARGO_PKG_VERSION"),
            micyou_api::API_VERSION,
            micyou_core::config::config_dir().display()
        );

        Ok(Managed {
            backend,
            rpc,
            mode_lock: self.mode_lock,
        })
    }
}

/// A fully wired backend: core service + RPC router + session registry.
pub struct Managed {
    /// The backend service facade (all RPC methods; also usable in-process).
    pub backend: Arc<Backend>,
    /// The RPC service (dispatch + sessions + event pump).
    pub rpc: Arc<RpcService>,
    mode_lock: bool,
}

impl Managed {
    /// Direct access to the shared server core.
    pub fn core(&self) -> &Arc<ServerCore> {
        &self.backend.core
    }

    /// Open the persistent audio output device (virtual mic) ahead of the
    /// first server start, mirroring the stock GUI's startup behaviour.
    /// Idempotent; safe to call when no audio hardware exists (logs a
    /// warning and returns false).
    pub fn ensure_audio_output(&self) -> bool {
        let prefs = micyou_core::config::load_server_prefs();
        let device = micyou_core::service::normalize_output_device(&prefs.output_device);
        self.backend
            .core
            .ensure_audio_output(device, self.backend.resource_dir.as_deref())
    }

    /// Serve JSON-RPC over stdin/stdout until EOF (sidecar mode).
    pub async fn serve_stdio(&self) {
        if let Err(e) = micyou_rpc::stdio::serve_stdio(self.rpc.clone()).await {
            log::error!("[rpc] stdio transport failed: {e}");
        }
    }

    /// Serve JSON-RPC over WebSocket at `addr` (`ws://addr/rpc`). Runs until
    /// cancelled/aborted; returns the bind error, if any.
    pub async fn serve_ws(&self, addr: std::net::SocketAddr) -> Result<(), String> {
        micyou_rpc::ws::serve_ws(self.rpc.clone(), addr).await
    }

    /// Attach an in-process session (embedded Rust frontends).
    pub fn attach_local(&self) -> LocalConnection {
        micyou_rpc::local::attach(self.rpc.clone())
    }

    /// Graceful process-exit teardown: release the mode lock and close the
    /// persistent audio device. Does not stop a running server (call
    /// `backend.stop_server()` first when needed).
    pub fn shutdown(&self) {
        if self.mode_lock {
            micyou_core::mode_lock::release();
        }
        self.backend.core.shutdown();
    }
}

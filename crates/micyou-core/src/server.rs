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

//! Server core: owns the shared runtime state and the start/stop
//! transactions for the whole backend.
//!
//! This module replaces the upstream `ServerState` + `start_server_inner` /
//! `stop_server_inner` trio that lived inside the Tauri command layer. The
//! orchestration is unchanged in behaviour — lifecycle gate, bounded audio
//! join, mDNS advertisement, transport startup with rollback — but every
//! frontend concern is gone: notifications flow through the [`EventBus`],
//! transport reporting through [`CoreTransportBridge`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use micyou_audio::dsp::AudioDspSettings;
use micyou_transport::discovery::NetworkManager;
use micyou_transport::events::{SharedTransportEvents, TransportConfig, TransportMode};
use micyou_transport::stats::NetworkStats;
use micyou_transport::stream::AudioStreamEvent;
use micyou_transport::tcp::{self, SharedActiveConnection, SharedTakeoverLock};
use micyou_transport::udp::{self, SharedActiveAudioSession};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::audio_output::AudioOutputHandle;
use crate::events::{CoreTransportBridge, EventBus, ServerEvent};
use crate::lifecycle::{
    await_startup_ready, ServerLifecycleGate, ServerLifecyclePhase, ServerLifecycleState,
    AUDIO_JOIN_TIMEOUT, STARTUP_TIMEOUT,
};
use crate::pipeline::{self, PipelineParams};
use crate::plugins::PluginHost;

const NETWORK_TASK_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Parameters for one server start transaction.
#[derive(Debug, Clone, Default)]
pub struct StartParams {
    /// Streaming port (TCP control; UDP audio = port + 1; web TLS port in
    /// web mode).
    pub port: u16,
    /// Connection mode.
    pub mode: TransportMode,
    /// Bind address (`None` → `0.0.0.0`).
    pub bind_address: Option<String>,
    /// Explicit output device (`None` → system default / virtual device).
    pub output_device: Option<String>,
    /// Host-provided resource directory (ONNX models, ALSA config).
    /// Searched before the executable-relative and dev fallbacks.
    pub resource_dir: Option<PathBuf>,
    /// USB mode only: adb device serial when several devices are attached.
    pub usb_device_serial: Option<String>,
}

/// The shared backend runtime. One instance per process; cheap to clone
/// fields out of, but start/stop go through the lifecycle gate.
pub struct ServerCore {
    /// Serializes complete start/stop transactions.
    pub lifecycle_gate: ServerLifecycleGate,
    /// Lifecycle state machine (phase + audio thread tracking).
    pub lifecycle: Arc<Mutex<ServerLifecycleState>>,
    /// Token cancelling all transport tasks of the running server.
    pub cancel_token: Arc<Mutex<Option<CancellationToken>>>,
    /// Transport tasks to join/abort on stop.
    pub background_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// mDNS advertisement of the TCP control service.
    pub mdns: Arc<Mutex<Option<NetworkManager>>>,
    /// mDNS advertisement of the web-mode service.
    pub web_mdns: Arc<Mutex<Option<NetworkManager>>>,
    /// Live DSP settings (shared with the audio pipeline).
    pub dsp_settings: Arc<RwLock<AudioDspSettings>>,
    /// Ear-return monitoring flag.
    pub is_monitoring: Arc<AtomicBool>,
    /// Spectrum streaming flag.
    pub spectrum_enabled: Arc<AtomicBool>,
    /// Shared session statistics.
    pub stats: Arc<NetworkStats>,
    /// Active TCP control connection (takeover-aware).
    pub active_connection: SharedActiveConnection,
    /// Serializes connection takeover publication.
    pub takeover_lock: SharedTakeoverLock,
    /// UDP audio session binding.
    pub active_audio_session: SharedActiveAudioSession,
    /// Persistent audio output device thread.
    pub audio_output: Arc<AudioOutputHandle>,
    /// Plugin host (manager, bus, DSP registry, host API).
    pub plugins: Arc<PluginHost>,
    /// Core event bus (frontends subscribe via the RPC layer).
    pub bus: EventBus,
    /// Runtime-tunable transport flags (mute sync).
    pub transport_config: Arc<TransportConfig>,
    /// Bridge translating transport notifications to bus + plugin events.
    bridge: Arc<CoreTransportBridge>,
    /// One-shot flag for the plugin event pump task.
    plugin_pump_started: AtomicBool,
    /// Web-mode server instance.
    #[cfg(feature = "web")]
    pub web_server: Arc<Mutex<Option<micyou_transport::web::WebServer>>>,
}

impl Default for ServerCore {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerCore {
    /// Build the core from persisted configuration.
    ///
    /// Spawns the persistent audio output thread (watching the shared mute
    /// flag) and the plugin host; loads saved plugin state lazily on first
    /// server start (matching upstream behaviour shared by GUI/CLI/TUI).
    pub fn new() -> Self {
        let stats = Arc::new(NetworkStats::default());
        // The output engine watches the stats' mute flag directly: switching
        // to muted drops queued audio and outputs silence within one device
        // callback period.
        let audio_output = AudioOutputHandle::spawn_with_mute_flag(stats.mute_flag());
        let active_connection: SharedActiveConnection = Arc::new(Mutex::new(None));
        let active_audio_session: SharedActiveAudioSession =
            Arc::new(RwLock::new(udp::ActiveAudioSession::Inactive));
        let lifecycle = Arc::new(Mutex::new(ServerLifecycleState::default()));
        #[cfg(feature = "web")]
        let web_server = Arc::new(Mutex::new(None));

        let bus = EventBus::default();
        let prefs = crate::config::load_server_prefs();
        let transport_config = Arc::new(TransportConfig::new(prefs.mute_sync));

        let plugins = Arc::new(PluginHost::new(
            audio_output.clone(),
            stats.clone(),
            active_connection.clone(),
            active_audio_session.clone(),
            lifecycle.clone(),
            #[cfg(feature = "web")]
            web_server.clone(),
        ));

        let bridge = Arc::new(CoreTransportBridge::new(bus.clone(), plugins.clone()));

        let core = Self {
            lifecycle_gate: ServerLifecycleGate::default(),
            lifecycle,
            cancel_token: Arc::new(Mutex::new(None)),
            background_tasks: Arc::new(Mutex::new(Vec::new())),
            mdns: Arc::new(Mutex::new(None)),
            web_mdns: Arc::new(Mutex::new(None)),
            dsp_settings: Arc::new(RwLock::new(crate::config::load_dsp_settings())),
            is_monitoring: Arc::new(AtomicBool::new(false)),
            spectrum_enabled: Arc::new(AtomicBool::new(false)),
            stats,
            active_connection,
            takeover_lock: Arc::new(Mutex::new(())),
            active_audio_session,
            audio_output,
            plugins,
            bus,
            transport_config,
            bridge,
            plugin_pump_started: AtomicBool::new(false),
            #[cfg(feature = "web")]
            web_server,
        };
        core.wire_control_handlers();
        core
    }

    /// Start the pump delivering subscribed backend events to plugins as
    /// `host:event` bus messages. Idempotent; requires a tokio runtime
    /// (called from `start()` and from the RPC layer when available).
    pub fn ensure_plugin_event_pump(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return; // no runtime yet; start() will retry
        }
        if self.plugin_pump_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let mut rx = self.bus.subscribe();
        let plugins = self.plugins.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if plugins.event_subs.is_empty() {
                            continue;
                        }
                        let tag = event.tag();
                        let Ok(json) = serde_json::to_vec(&*event) else {
                            continue;
                        };
                        for (plugin_id, filters) in plugins.event_subs.snapshot() {
                            if filters
                                .iter()
                                .any(|f| crate::plugins::event_filter_matches(f, tag))
                            {
                                plugins.deliver_event(&plugin_id, &json);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Install the plugin control-plane handlers (mute/monitoring/DSP).
    /// Wired once at construction — all referenced state outlives restarts.
    fn wire_control_handlers(&self) {
        let stats = self.stats.clone();
        let bus = self.bus.clone();
        let plugins = self.plugins.clone();
        let transport_config = self.transport_config.clone();
        let is_monitoring = self.is_monitoring.clone();
        let audio_output = self.audio_output.clone();
        let dsp_settings = self.dsp_settings.clone();

        self.plugins
            .set_control_handlers(crate::plugins::ControlPlaneHandlers {
                get_muted: Some(Arc::new({
                    let stats = stats.clone();
                    move || Ok(stats.is_muted())
                })),
                set_muted: Some(Arc::new({
                    let stats = stats.clone();
                    let bus = bus.clone();
                    let plugins = plugins.clone();
                    let transport_config = transport_config.clone();
                    move |muted: bool| {
                        stats.set_muted(muted);
                        bus.publish(ServerEvent::MuteStateChanged { muted });
                        plugins.broadcast_event(&micyou_plugin::PluginEvent::MuteChanged { muted });
                        // Mute sync disabled (server.json): keep the mute
                        // local and do not push it to the mobile client.
                        if transport_config.mute_sync_enabled() {
                            push_mute_to_device(&plugins, muted);
                        }
                        Ok(())
                    }
                })),
                get_monitoring: Some(Arc::new({
                    let mon = is_monitoring.clone();
                    move || Ok(mon.load(Ordering::Relaxed))
                })),
                set_monitoring: Some(Arc::new({
                    let mon = is_monitoring.clone();
                    let output = audio_output.clone();
                    let bus = bus.clone();
                    let plugins = plugins.clone();
                    move |enabled: bool| {
                        mon.store(enabled, Ordering::Relaxed);
                        output.set_monitoring(enabled);
                        bus.publish(ServerEvent::MonitoringChanged { enabled });
                        plugins.broadcast_event(&micyou_plugin::PluginEvent::MonitoringChanged {
                            enabled,
                        });
                        Ok(())
                    }
                })),
                get_dsp_settings: Some(Arc::new({
                    let dsp = dsp_settings.clone();
                    move || {
                        let guard = dsp.read().map_err(|_| {
                            micyou_plugin::PluginError::Runtime("dsp settings lock error".into())
                        })?;
                        serde_json::to_string(&*guard).map_err(|e| {
                            micyou_plugin::PluginError::Runtime(format!("dsp serialize error: {e}"))
                        })
                    }
                })),
                set_dsp_settings: Some(Arc::new({
                    let dsp = dsp_settings.clone();
                    let plugins = plugins.clone();
                    move |settings_json: &str| {
                        let current = {
                            let guard = dsp.read().map_err(|_| {
                                micyou_plugin::PluginError::Runtime(
                                    "dsp settings lock error".into(),
                                )
                            })?;
                            guard.clone()
                        };
                        let mut val = serde_json::to_value(&current).map_err(|e| {
                            micyou_plugin::PluginError::Validation(format!(
                                "serialize current dsp: {e}"
                            ))
                        })?;
                        if let Ok(patch_map) = serde_json::from_str::<
                            serde_json::Map<String, serde_json::Value>,
                        >(settings_json)
                        {
                            if let Some(obj) = val.as_object_mut() {
                                for (k, v) in patch_map {
                                    obj.insert(k, v);
                                }
                            }
                        } else if let Ok(direct) =
                            serde_json::from_str::<AudioDspSettings>(settings_json)
                        {
                            val = serde_json::to_value(&direct).map_err(|e| {
                                micyou_plugin::PluginError::Validation(format!(
                                    "serialize direct dsp: {e}"
                                ))
                            })?;
                        } else {
                            return Err(micyou_plugin::PluginError::Validation(
                                "invalid dsp json".into(),
                            ));
                        }
                        let mut updated: AudioDspSettings =
                            serde_json::from_value(val).map_err(|e| {
                                micyou_plugin::PluginError::Validation(format!(
                                    "parse updated dsp: {e}"
                                ))
                            })?;
                        updated.normalize();
                        // Plugin writes must not drop/leak plugin chain nodes
                        // (upstream issue #347).
                        plugins.reconcile_settings_chain(&mut updated);
                        {
                            let mut guard = dsp.write().map_err(|_| {
                                micyou_plugin::PluginError::Runtime(
                                    "dsp settings lock error".into(),
                                )
                            })?;
                            *guard = updated.clone();
                        }
                        let _ = crate::config::save_dsp_settings(&updated);
                        plugins.broadcast_event(&micyou_plugin::PluginEvent::DspSettingsChanged);
                        Ok(())
                    }
                })),
            });
    }

    /// Push a mute message to the connected device through the plugin sync
    /// channel (which mirrors the active TCP control channel).
    fn push_mute(&self, muted: bool) {
        push_mute_to_device(&self.plugins, muted);
    }

    /// Current lifecycle phase.
    pub async fn phase(&self) -> ServerLifecyclePhase {
        self.lifecycle.lock().await.phase()
    }

    /// Whether the audio server is running.
    pub async fn is_running(&self) -> bool {
        matches!(self.phase().await, ServerLifecyclePhase::Running)
    }

    /// Set the hard-mute flag: silences local output immediately, notifies
    /// bus + plugins, and (when mute sync is on) pushes to the device.
    pub fn set_muted(&self, muted: bool) {
        self.stats.set_muted(muted);
        self.plugins
            .broadcast_event(&micyou_plugin::PluginEvent::MuteChanged { muted });
        self.bus.publish(ServerEvent::MuteStateChanged { muted });
        if self.transport_config.mute_sync_enabled() {
            self.push_mute(muted);
        }
    }

    /// Toggle ear-return monitoring.
    pub fn set_monitoring(&self, enabled: bool) {
        self.is_monitoring.store(enabled, Ordering::Relaxed);
        self.audio_output.set_monitoring(enabled);
        self.plugins
            .broadcast_event(&micyou_plugin::PluginEvent::MonitoringChanged { enabled });
        self.bus.publish(ServerEvent::MonitoringChanged { enabled });
    }

    /// Toggle spectrum streaming (opt-in for frontends rendering analyzers).
    pub fn set_spectrum_streaming(&self, enabled: bool) {
        self.spectrum_enabled.store(enabled, Ordering::Release);
        self.bus
            .publish(ServerEvent::SpectrumStreamingChanged { enabled });
    }

    /// Apply, validate and persist new DSP settings (RPC `audio/settings`
    /// and plugin `set_dsp_settings` share this path).
    pub fn update_dsp_settings(&self, mut settings: AudioDspSettings) -> Result<(), String> {
        settings.normalize();
        // AEC must always run first in the processing chain.
        if let Some(pos) = settings.processing_chain.iter().position(|s| s == "AEC") {
            if pos != 0 {
                let stage = settings.processing_chain.remove(pos);
                settings.processing_chain.insert(0, stage);
            }
        }
        // Keep plugin chain nodes in sync with the live DSP registry (#347).
        self.plugins.reconcile_settings_chain(&mut settings);
        match self.dsp_settings.write() {
            Ok(mut current) => {
                if settings.aec_enabled && !current.aec_enabled && !aec_supported() {
                    return Err("AEC is not supported on macOS".to_string());
                }
                // Persist to the shared settings.json first so a failed write
                // never leaves memory and disk inconsistent.
                crate::config::save_dsp_settings(&settings)
                    .map_err(|e| format!("Failed to persist settings: {e}"))?;
                *current = settings;
                self.plugins
                    .broadcast_event(&micyou_plugin::PluginEvent::DspSettingsChanged);
                Ok(())
            }
            Err(e) => Err(format!("Failed to update settings: {}", e)),
        }
    }

    /// Refresh the shared mute-sync flag from persisted server preferences
    /// (called by the service layer after `save_server_prefs`).
    pub fn reload_transport_config(&self) {
        let prefs = crate::config::load_server_prefs();
        self.transport_config.set_mute_sync(prefs.mute_sync);
    }

    /// Open the persistent audio output device (idempotent). Called at
    /// daemon startup so the virtual mic exists before any client connects.
    pub fn ensure_audio_output(
        &self,
        output_device: Option<String>,
        resource_dir: Option<&std::path::Path>,
    ) -> bool {
        let buffer_ms = self
            .dsp_settings
            .read()
            .map(|s| (s.output_buffer_ms as usize).clamp(100, 1200))
            .unwrap_or(800);
        pipeline::ensure_audio_output_started(
            &self.audio_output,
            output_device,
            buffer_ms,
            resource_dir,
        )
    }

    /// Tear down the persistent output device. Process-exit only.
    pub fn shutdown(&self) {
        pipeline::shutdown_audio_output(&self.audio_output);
    }

    /// Start the audio server (full transaction; serialized by the gate).
    pub async fn start(&self, params: StartParams) -> Result<String, String> {
        let StartParams {
            port,
            mode,
            bind_address,
            output_device,
            resource_dir,
            usb_device_serial,
        } = params;

        let udp_port = validate_server_port(port, mode)?;
        let bind_addr = bind_address.unwrap_or_else(|| "0.0.0.0".to_string());

        let _lifecycle_guard = self.lifecycle_gate.enter().await;
        self.lifecycle.lock().await.begin_start().await?;
        self.ensure_plugin_event_pump();

        let cancel_token = {
            let mut token_lock = self.cancel_token.lock().await;
            if token_lock.is_some() {
                return Err("Server is already running".to_string());
            }
            let token = CancellationToken::new();
            *token_lock = Some(token.clone());
            token
        };

        // Reload shared settings.json before starting so external edits
        // (other frontends sharing the config dir) apply.
        let file_settings = crate::config::load_dsp_settings();
        if let Ok(mut current) = self.dsp_settings.write() {
            *current = file_settings;
        }
        self.reload_transport_config();

        // USB mode: set up adb port forwarding before binding servers.
        if mode == TransportMode::Usb {
            if let Err(e) =
                micyou_transport::adb::enable_usb_mode(port, usb_device_serial.as_deref())
                    .map(|_| ())
            {
                let rollback = self.rollback_start(&cancel_token, Vec::new()).await;
                return Err(match rollback {
                    Ok(()) => format!("USB mode setup failed: {e}"),
                    Err(cleanup) => format!("USB mode setup failed: {e}; {cleanup}"),
                });
            }
        }

        // mDNS advertisement (wifi/usb modes; web mode advertises its own
        // service type below).
        if mode != TransportMode::Web {
            let mut mdns_lock = self.mdns.lock().await;
            match NetworkManager::start_mdns(port, &bind_addr) {
                Ok(manager) => *mdns_lock = Some(manager),
                Err(e) => log::error!("Failed to start mDNS: {}", e),
            }
        }

        // Scan & load persisted plugin state across (re)starts.
        self.plugins.load_saved_plugins();

        // Sync the runtime chain with the DSP plugin registry: every
        // registered plugin gets its own `Plugin:<id>` node (#347).
        self.plugins.ensure_plugin_chain_node(&self.dsp_settings);
        let output_buffer_ms = self
            .dsp_settings
            .read()
            .map(|s| (s.output_buffer_ms as usize).clamp(100, 1200))
            .unwrap_or(800);

        // Locate bundled resources (ONNX models + ALSA config) once for the
        // whole startup, then load the ONNX Runtime shared library. The
        // official Microsoft build uses runtime CPUID dispatch for AVX2/SSE
        // kernels, so it works on CPUs without AVX2.
        let resource_root = crate::resources::find_resource_dir(resource_dir.as_deref());
        if let Some(ort_path) = crate::resources::find_ort_runtime(resource_root.as_deref()) {
            if let Err(e) = micyou_audio::init_ort_runtime(&ort_path) {
                log::error!(
                    "Failed to load ONNX Runtime from {}: {e}",
                    ort_path.display()
                );
            }
        } else {
            log::warn!(
                "ONNX Runtime library ({}) not found; AI noise suppression will be unavailable",
                crate::resources::ort_runtime_filename()
            );
        }

        // Pre-open the persistent output device OUTSIDE the pipeline's
        // startup budget: first open can be slow (Linux PipeWire virtual
        // device creation, cold cpal/ALSA enumeration) and must not consume
        // the 10 s pipeline ready timeout. Idempotent — the pipeline thread
        // re-checks cheaply.
        {
            let output = self.audio_output.clone();
            let device = output_device.clone();
            let res_root = resource_root.clone();
            let warm = tokio::task::spawn_blocking(move || {
                pipeline::ensure_audio_output_started(
                    &output,
                    device,
                    output_buffer_ms,
                    res_root.as_deref(),
                )
            });
            match tokio::time::timeout(std::time::Duration::from_secs(30), warm).await {
                Ok(Ok(true)) => log::info!("[Audio] Output device pre-opened"),
                Ok(Ok(false)) => log::warn!(
                    "[Audio] Output device unavailable (continuing; audio will be silent)"
                ),
                Ok(Err(join)) => log::error!("[Audio] Output warm-up task failed: {join}"),
                Err(_) => log::error!("[Audio] Output warm-up timed out after 30 s (continuing)"),
            }
        }

        // Audio pipeline (shared by all modes). Android packets are ~7 ms,
        // so 128 slots bound queued latency while leaving scheduling headroom.
        self.bridge.set_mode(mode);
        let events: SharedTransportEvents = self.bridge.clone();
        let (audio_tx, audio_rx) = mpsc::channel::<AudioStreamEvent>(128);

        let (audio_thread, audio_ready_rx) = pipeline::spawn(
            PipelineParams {
                dsp_settings: self.dsp_settings.clone(),
                output: self.audio_output.clone(),
                dsp_hook: self.plugins.dsp_hook(),
                stats: self.stats.clone(),
                bus: self.bus.clone(),
                is_monitoring: self.is_monitoring.clone(),
                spectrum_enabled: self.spectrum_enabled.clone(),
                active_audio_session: self.active_audio_session.clone(),
                resource_root: resource_root.clone(),
                skip_dsp: mode == TransportMode::Web,
                output_device,
                output_buffer_ms,
            },
            audio_rx,
        );

        self.lifecycle.lock().await.set_audio_thread(audio_thread);
        let audio_ready = await_startup_ready(audio_ready_rx, "Audio output", STARTUP_TIMEOUT)
            .await
            .map_err(|error| format!("Failed to start audio output: {}", error));
        if let Err(error) = audio_ready {
            let rollback = self.rollback_start(&cancel_token, Vec::new()).await;
            return Err(match rollback {
                Ok(()) => error,
                Err(cleanup) => format!("{}; {}", error, cleanup),
            });
        }

        // Web mode: TLS WebSocket server + generation-aware audio bridge.
        #[cfg(feature = "web")]
        if mode == TransportMode::Web {
            return self
                .start_web(port, bind_addr, events, audio_tx, &cancel_token)
                .await;
        }
        #[cfg(not(feature = "web"))]
        if mode == TransportMode::Web {
            let error = "Web server feature not enabled".to_string();
            let rollback = self.rollback_start(&cancel_token, Vec::new()).await;
            return Err(match rollback {
                Ok(()) => error,
                Err(cleanup) => format!("{}; {}", error, cleanup),
            });
        }

        // Wifi/USB: TCP control plane + UDP audio plane.
        let tcp_ctx = tcp::TcpServerContext {
            events: events.clone(),
            audio_tx: audio_tx.clone(),
            stats: self.stats.clone(),
            mode,
            config: self.transport_config.clone(),
            active_connection: self.active_connection.clone(),
            takeover_lock: self.takeover_lock.clone(),
            active_audio_session: self.active_audio_session.clone(),
        };
        let token_tcp = cancel_token.clone();
        let bind_addr_tcp = bind_addr.clone();
        let (tcp_ready_tx, tcp_ready_rx) = tokio::sync::oneshot::channel();
        let tcp_task = tokio::spawn(async move {
            if let Err(e) =
                tcp::start_tcp_server(tcp_ctx, port, bind_addr_tcp, token_tcp, tcp_ready_tx).await
            {
                log::error!("TCP Server error: {}", e);
            }
        });

        let token_udp = cancel_token.clone();
        let udp_port = udp_port.expect("non-web port validation must produce a UDP port");
        let stats_udp = self.stats.clone();
        let active_audio_session_udp = self.active_audio_session.clone();
        let (udp_ready_tx, udp_ready_rx) = tokio::sync::oneshot::channel();
        let udp_task = tokio::spawn(async move {
            if let Err(e) = udp::start_udp_server(
                audio_tx,
                udp_port,
                bind_addr,
                token_udp,
                stats_udp,
                active_audio_session_udp,
                udp_ready_tx,
            )
            .await
            {
                log::error!("UDP Server error: {}", e);
            }
        });

        let (tcp_ready, udp_ready) = tokio::join!(
            await_startup_ready(tcp_ready_rx, "TCP server", STARTUP_TIMEOUT),
            await_startup_ready(udp_ready_rx, "UDP server", STARTUP_TIMEOUT),
        );
        if let Err(error) = tcp_ready.and(udp_ready) {
            let error = format!("Failed to start network server: {}", error);
            let rollback = self
                .rollback_start(&cancel_token, vec![tcp_task, udp_task])
                .await;
            return Err(match rollback {
                Ok(()) => error,
                Err(cleanup) => format!("{}; {}", error, cleanup),
            });
        }
        self.background_tasks
            .lock()
            .await
            .extend([tcp_task, udp_task]);
        self.lifecycle.lock().await.mark_running();

        Ok(format!("Server started on port {} ({} mode)", port, mode))
    }

    /// Web-mode startup: TLS server, web mDNS and the packet bridge that
    /// re-sessions browser audio generations into the shared pipeline.
    #[cfg(feature = "web")]
    async fn start_web(
        &self,
        port: u16,
        bind_addr: String,
        events: SharedTransportEvents,
        audio_tx: mpsc::Sender<AudioStreamEvent>,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        use micyou_transport::stream::ExpectedAudioSession;

        let web_server_instance = micyou_transport::web::WebServer::new();
        let (web_audio_tx, mut web_audio_rx) =
            mpsc::channel::<(u64, micyou_protocol::micyou::AudioPacketMessage)>(128);

        if let Err(e) = web_server_instance
            .start(port, events.clone(), web_audio_tx)
            .await
        {
            let error = format!("Failed to start web server: {}", e);
            let rollback = self.rollback_start(cancel_token, Vec::new()).await;
            return Err(match rollback {
                Ok(()) => error,
                Err(cleanup) => format!("{}; {}", error, cleanup),
            });
        }

        {
            let mut web_mdns_lock = self.web_mdns.lock().await;
            match NetworkManager::start_web_mdns(port, &bind_addr) {
                Ok(manager) => *web_mdns_lock = Some(manager),
                Err(e) => log::error!("Failed to start web mDNS: {}", e),
            }
        }
        *self.web_server.lock().await = Some(web_server_instance);

        let web_audio_task = tokio::spawn(async move {
            let mut seq: i32 = 0;
            let mut active_generation = 0u64;
            while let Some((generation, packet)) = web_audio_rx.recv().await {
                if generation < active_generation {
                    continue; // stale client
                }
                if generation > active_generation {
                    active_generation = generation;
                    seq = 0;
                    if audio_tx
                        .send(AudioStreamEvent::SessionStarting {
                            expected: ExpectedAudioSession::Bound(0),
                            epoch: generation,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                let Some(ordered) = pipeline::wrap_web_packet(packet, seq) else {
                    continue;
                };
                seq += 1;
                if audio_tx
                    .send(AudioStreamEvent::Packet {
                        packet: ordered,
                        epoch: generation,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        self.background_tasks.lock().await.push(web_audio_task);
        self.lifecycle.lock().await.mark_running();

        Ok(format!("Web server started on port {}", port))
    }

    /// Stop the running server (full transaction). Errors when not running.
    pub async fn stop(&self) -> Result<String, String> {
        let _lifecycle_guard = self.lifecycle_gate.enter().await;
        self.spectrum_enabled.store(false, Ordering::Release);

        #[cfg(feature = "web")]
        {
            let mut web_lock = self.web_server.lock().await;
            if let Some(web) = web_lock.take() {
                web.stop().await;
            }
            let mut web_mdns_lock = self.web_mdns.lock().await;
            if let Some(web_mdns) = web_mdns_lock.take() {
                web_mdns.stop_mdns();
            }
        }

        {
            let mut mdns_lock = self.mdns.lock().await;
            if let Some(mdns) = mdns_lock.take() {
                mdns.stop_mdns();
            }
        }

        let token = self.cancel_token.lock().await.take();
        let had_token = token.is_some();
        if let Some(token) = token {
            token.cancel();
        }
        self.lifecycle.lock().await.begin_stopping();
        let tasks = std::mem::take(&mut *self.background_tasks.lock().await);
        tcp::cleanup_session_state(&self.active_connection, &self.active_audio_session).await;
        join_tasks_bounded(tasks, NETWORK_TASK_JOIN_TIMEOUT).await;
        let audio_result = self
            .lifecycle
            .lock()
            .await
            .join_audio_bounded(AUDIO_JOIN_TIMEOUT)
            .await;
        // Restore the original input device on macOS (BlackHole cleanup).
        #[cfg(target_os = "macos")]
        {
            let _ = crate::platform::blackhole::do_restore_input_device().await;
        }
        audio_result?;
        if had_token {
            self.bus.publish(ServerEvent::ServerStopped);
            Ok("Server stopped".to_string())
        } else {
            Err("Server is not running".to_string())
        }
    }

    /// Undo a failed start: cancel transports, clean the session state, join
    /// tasks and the audio thread with bounds.
    async fn rollback_start(
        &self,
        cancel_token: &CancellationToken,
        tasks: Vec<JoinHandle<()>>,
    ) -> Result<(), String> {
        cancel_token.cancel();
        tcp::cleanup_session_state(&self.active_connection, &self.active_audio_session).await;
        join_tasks_bounded(tasks, NETWORK_TASK_JOIN_TIMEOUT).await;
        self.cancel_token.lock().await.take();
        if let Some(mdns) = self.mdns.lock().await.take() {
            mdns.stop_mdns();
        }
        let mut lifecycle = self.lifecycle.lock().await;
        lifecycle.begin_stopping();
        lifecycle.join_audio_bounded(AUDIO_JOIN_TIMEOUT).await
    }

    /// Is the device connected (TCP session, UDP audio, or web clients)?
    pub async fn is_device_connected(&self) -> bool {
        let conn_active = self.active_connection.lock().await.is_some();
        let audio_active = self
            .active_audio_session
            .read()
            .map(|s| !matches!(*s, udp::ActiveAudioSession::Inactive))
            .unwrap_or(false);
        if conn_active || audio_active {
            return true;
        }
        #[cfg(feature = "web")]
        {
            let web_lock = self.web_server.lock().await;
            if web_lock.as_ref().is_some_and(|w| w.client_count() > 0) {
                return true;
            }
        }
        false
    }
}

/// Send a mute message over the active control channel (best effort).
fn push_mute_to_device(plugins: &PluginHost, muted: bool) {
    let mute_msg = micyou_protocol::micyou::MessageWrapper {
        audio_packet: None,
        connect: None,
        mute: Some(micyou_protocol::micyou::MuteMessage {
            is_muted: Some(muted),
        }),
        ping: None,
        pong: None,
        plugin_message: None,
    };
    plugins.sync.try_send_wrapper(mute_msg);
}

/// Whether this platform has a usable AEC reference capture path.
pub const fn aec_supported() -> bool {
    !cfg!(target_os = "macos")
}

/// Validate the streaming port; returns the UDP audio port (TCP + 1) for
/// non-web modes and `None` for web mode.
pub fn validate_server_port(port: u16, mode: TransportMode) -> Result<Option<u16>, String> {
    if mode == TransportMode::Web {
        return if port == 0 {
            Err("Web server port must be between 1 and 65535".to_string())
        } else {
            Ok(None)
        };
    }

    if port == 0 {
        return Err("Audio server port must be between 1 and 65534".to_string());
    }

    port.checked_add(1).map(Some).ok_or_else(|| {
        "Audio server port must be between 1 and 65534 so the following UDP port is valid"
            .to_string()
    })
}

async fn join_tasks_bounded(mut tasks: Vec<JoinHandle<()>>, timeout: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while let Some(mut task) = tasks.pop() {
        if tokio::time::timeout_at(deadline, &mut task).await.is_err() {
            task.abort();
            let _ = task.await;
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_validation_matches_upstream_rules() {
        assert_eq!(
            validate_server_port(0, TransportMode::Wifi).unwrap_err(),
            "Audio server port must be between 1 and 65534"
        );
        assert_eq!(
            validate_server_port(65535, TransportMode::Wifi).unwrap_err(),
            "Audio server port must be between 1 and 65534 so the following UDP port is valid"
        );
        assert_eq!(
            validate_server_port(8554, TransportMode::Usb).unwrap(),
            Some(8555)
        );
        assert_eq!(
            validate_server_port(8443, TransportMode::Web).unwrap(),
            None
        );
        assert!(validate_server_port(0, TransportMode::Web).is_err());
    }

    #[tokio::test]
    async fn join_tasks_bounded_aborts_stalled_tasks() {
        let stalled = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        join_tasks_bounded(vec![stalled], std::time::Duration::from_millis(20)).await;
        // Returning at all proves the abort path ran within the bound.
    }

    #[tokio::test]
    async fn stop_without_start_reports_not_running() {
        let core = ServerCore::new();
        let result = core.stop().await;
        assert_eq!(result.unwrap_err(), "Server is not running");
        core.shutdown();
    }
}

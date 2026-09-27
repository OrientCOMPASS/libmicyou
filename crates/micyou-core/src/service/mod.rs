/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! Backend service facade.
//!
//! [`Backend`] is the single object the RPC layer (and in-process embedders)
//! call into. Every upstream Tauri command that owned backend state has a
//! method here; window/tray commands deliberately do not — they are frontend
//! concerns now. Methods return the contract DTOs from [`micyou_api`] so the
//! same code serves stdio, WebSocket and embedded clients.

pub mod about;
pub mod plugins;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU8, Ordering};
use std::sync::Arc;

use micyou_api::config::{ServerPrefs, ThemeColors, UiPrefs};
use micyou_api::methods::*;
use micyou_audio::dsp::AudioDspSettings;
use micyou_transport::adb::{self, AdbDevice, UsbModeResult};
use micyou_transport::host_info::{query_network_interfaces, NetworkInfo, NetworkInterfaceInfo};
use micyou_transport::TransportMode;

use crate::events::ServerEvent;
use crate::lifecycle::ServerLifecyclePhase;
use crate::server::{ServerCore, StartParams};

/// Normalize a persisted output-device value (`""`, `"auto"`, `"default"`
/// all mean "no explicit device").
pub fn normalize_output_device(raw: &str) -> Option<String> {
    let d = raw.trim();
    if d.is_empty() || d == "auto" || d == "default" {
        None
    } else {
        Some(d.to_string())
    }
}

/// The backend service: owns the [`ServerCore`] and implements every RPC
/// method. Cloneable via `Arc`.
pub struct Backend {
    /// Shared server runtime.
    pub core: Arc<ServerCore>,
    /// Host-provided resource directory (ONNX models); `None` → autodetect.
    pub resource_dir: Option<PathBuf>,
    last_mode: AtomicU8,
    last_port: AtomicU16,
}

impl Default for Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend {
    /// Create a backend with a fresh [`ServerCore`].
    pub fn new() -> Self {
        Self::with_core(Arc::new(ServerCore::new()))
    }

    /// Wrap an existing core (embedded hosts sharing state).
    pub fn with_core(core: Arc<ServerCore>) -> Self {
        Self {
            core,
            resource_dir: None,
            last_mode: AtomicU8::new(u8::MAX),
            last_port: AtomicU16::new(0),
        }
    }

    /// Set the resource directory used for ONNX model/runtime discovery.
    pub fn with_resource_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.resource_dir = dir;
        self
    }

    // ── session ────────────────────────────────────────────────────────────

    /// Identity + capability handshake result.
    pub fn session_hello(&self) -> SessionInfo {
        SessionInfo {
            backend: "libmicyou".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: micyou_api::API_VERSION,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }

    // ── server lifecycle ───────────────────────────────────────────────────

    /// Start the audio server. Unset parameters fall back to `server.json`.
    pub async fn start_server(
        &self,
        params: StartServerParams,
    ) -> Result<ServerActionResult, String> {
        let prefs = crate::config::load_server_prefs();

        let mode = match params.mode.as_deref() {
            Some(raw) => TransportMode::parse(raw)
                .ok_or_else(|| format!("invalid mode '{raw}' (expected wifi, usb or web)"))?,
            None => prefs.transport_mode().unwrap_or(TransportMode::Wifi),
        };

        let port = params.port.unwrap_or(if mode == TransportMode::Web {
            prefs.web_port
        } else {
            prefs.port
        });

        let bind_address = params.bind_address.or_else(|| {
            if prefs.auto_bind || prefs.bind_address.is_empty() || prefs.bind_address == "0.0.0.0" {
                None
            } else {
                Some(prefs.bind_address.clone())
            }
        });

        let output_device = params
            .output_device
            .as_deref()
            .map(normalize_output_device)
            .unwrap_or_else(|| normalize_output_device(&prefs.output_device));

        let message = self
            .core
            .start(StartParams {
                port,
                mode,
                bind_address,
                output_device,
                resource_dir: self.resource_dir.clone(),
                usb_device_serial: params.usb_device_serial.clone(),
            })
            .await?;

        self.last_mode.store(mode_index(mode), Ordering::Relaxed);
        self.last_port.store(port, Ordering::Relaxed);
        Ok(ServerActionResult { message })
    }

    /// Stop the audio server.
    pub async fn stop_server(&self) -> Result<ServerActionResult, String> {
        let message = self.core.stop().await?;
        Ok(ServerActionResult { message })
    }

    /// Aggregate lifecycle/connection/mute status.
    pub async fn server_status(&self) -> ServerStatus {
        let phase = self.core.phase().await;
        let mode_index = self.last_mode.load(Ordering::Relaxed);
        ServerStatus {
            phase: phase_str(phase).to_string(),
            is_server_running: phase == ServerLifecyclePhase::Running,
            is_connected: self.core.is_device_connected().await,
            is_muted: self.core.stats.is_muted(),
            is_monitoring: self
                .core
                .is_monitoring
                .load(std::sync::atomic::Ordering::Relaxed),
            mode: mode_from_index(mode_index).map(|m| m.to_string()),
            port: if mode_index == u8::MAX {
                None
            } else {
                Some(self.last_port.load(Ordering::Relaxed))
            },
        }
    }

    /// `server.json` preferences.
    pub fn server_prefs(&self) -> ServerPrefs {
        crate::config::load_server_prefs()
    }

    /// Persist `server.json` and apply live flags (mute sync).
    pub fn save_server_prefs(&self, prefs: ServerPrefs) -> Result<MessageResult, String> {
        crate::config::save_server_prefs(&prefs)?;
        self.core.reload_transport_config();
        Ok(MessageResult::new("Server prefs saved"))
    }

    /// Whether `server.json` already exists (frontend migration hint).
    pub fn server_prefs_exists(&self) -> bool {
        std::path::Path::new(&crate::config::server_prefs_path()).exists()
    }

    // ── audio ──────────────────────────────────────────────────────────────

    /// Sorted list of system output device names.
    pub fn audio_devices(&self) -> Vec<String> {
        use cpal::traits::{DeviceTrait, HostTrait};
        // Serialize against engine device init (WASAPI is not concurrency-safe).
        let _device_guard = micyou_audio::device_init_lock();
        let mut names = Vec::new();
        let host = cpal::default_host();
        if let Ok(devices) = host.output_devices() {
            for dev in devices {
                if let Ok(name) = dev.name() {
                    names.push(name);
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// Current DSP settings. Prefers the shared `settings.json` so edits made
    /// by other frontends are reflected; falls back to live state.
    pub fn audio_settings(&self) -> AudioDspSettings {
        if std::path::Path::new(&crate::config::settings_path()).exists() {
            return crate::config::load_dsp_settings();
        }
        self.core
            .dsp_settings
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// Validate, persist and hot-apply new DSP settings.
    pub fn update_audio_settings(
        &self,
        settings: AudioDspSettings,
    ) -> Result<MessageResult, String> {
        self.core.update_dsp_settings(settings)?;
        Ok(MessageResult::new("Settings updated"))
    }

    /// Set the hard-mute flag (event + device sync handled by the core).
    pub fn set_muted(&self, muted: bool) -> Ack {
        self.core.set_muted(muted);
        Ack::ok()
    }

    /// Current hard-mute flag.
    pub fn muted(&self) -> MuteState {
        MuteState {
            muted: self.core.stats.is_muted(),
        }
    }

    /// Toggle ear-return monitoring.
    pub fn set_monitoring(&self, enabled: bool) -> Ack {
        self.core.set_monitoring(enabled);
        Ack::ok()
    }

    /// Toggle spectrum streaming (opt-in for analyzer UIs).
    pub fn set_spectrum_streaming(&self, enabled: bool) -> Ack {
        self.core.set_spectrum_streaming(enabled);
        Ack::ok()
    }

    // ── network / usb ──────────────────────────────────────────────────────

    /// LAN IPs for QR display + the legacy protocol port constant.
    pub fn network_info(&self) -> NetworkInfo {
        let interfaces = query_network_interfaces();
        NetworkInfo {
            ips: interfaces.iter().map(|i| i.ip.clone()).collect(),
            port: micyou_protocol::PORT,
        }
    }

    /// Candidate LAN interfaces, best first.
    pub fn network_interfaces(&self) -> Vec<NetworkInterfaceInfo> {
        query_network_interfaces()
    }

    /// Request Windows Firewall inbound rules for this binary (UAC prompt).
    /// No-op success on other platforms.
    pub async fn firewall_allow(&self) -> Result<Ack, String> {
        #[cfg(windows)]
        {
            let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
            let exe_str = exe_path.to_string_lossy();
            let script = format!(
                "netsh advfirewall firewall add rule name=\"MicYou Backend (TCP-In)\" dir=in action=allow program=\"{}\" protocol=TCP enable=yes; netsh advfirewall firewall add rule name=\"MicYou Backend (UDP-In)\" dir=in action=allow program=\"{}\" protocol=UDP enable=yes",
                exe_str, exe_str
            );
            let status = std::process::Command::new("powershell")
                .args([
                    "-Command",
                    &format!(
                        "Start-Process cmd -ArgumentList '/c {}' -Verb RunAs -WindowStyle Hidden",
                        script
                    ),
                ])
                .status()
                .map_err(|e| e.to_string())?;
            if status.success() {
                log::info!(target: "system", "Firewall permission requested on Windows");
                Ok(Ack::ok())
            } else {
                Err("Failed to execute firewall rule addition".to_string())
            }
        }
        #[cfg(not(windows))]
        {
            Ok(Ack::ok())
        }
    }

    /// Set up `adb reverse` forwarding for USB mode.
    pub fn usb_enable(&self, params: UsbEnableParams) -> Result<UsbModeResult, String> {
        let port = params
            .port
            .unwrap_or_else(|| crate::config::load_server_prefs().port);
        adb::enable_usb_mode(port, params.device_serial.as_deref())
    }

    /// Attached adb devices.
    pub fn usb_devices(&self) -> Result<Vec<AdbDevice>, String> {
        adb::list_adb_devices()
    }

    // ── virtual devices ────────────────────────────────────────────────────

    /// Whether VB-CABLE appears installed (Windows).
    pub async fn vbcable_check(&self) -> Result<VbcableStatus, String> {
        Ok(VbcableStatus {
            installed: tokio::task::spawn_blocking(crate::platform::vbcable::is_installed)
                .await
                .map_err(|e| e.to_string())?,
        })
    }

    /// Download + install VB-CABLE (Windows, UAC). Progress flows through
    /// [`ServerEvent::InstallProgress`].
    #[cfg(feature = "vbcable")]
    pub async fn vbcable_install(&self) -> Result<crate::platform::vbcable::VBCableResult, String> {
        let bus = self.core.bus.clone();
        let progress: crate::platform::vbcable::ProgressFn = Arc::new(move |message: String| {
            bus.publish(ServerEvent::InstallProgress { message });
        });
        Ok(crate::platform::vbcable::install(progress).await)
    }

    /// Feature-disabled stub.
    #[cfg(not(feature = "vbcable"))]
    pub async fn vbcable_install(&self) -> Result<crate::platform::vbcable::VBCableResult, String> {
        Ok(crate::platform::vbcable::VBCableResult {
            success: false,
            error_type: Some("feature_disabled".to_string()),
            message: Some("VB-Cable installation feature not enabled".to_string()),
        })
    }

    /// BlackHole status (macOS).
    pub async fn blackhole_check(
        &self,
    ) -> Result<crate::platform::blackhole::BlackHoleStatus, String> {
        crate::platform::blackhole::check_blackhole().await
    }

    /// Switch the macOS system input to BlackHole.
    pub async fn blackhole_set_input(
        &self,
    ) -> Result<crate::platform::blackhole::BlackHoleResult, String> {
        crate::platform::blackhole::set_blackhole_as_input().await
    }

    /// Restore the previous macOS system input device.
    pub async fn blackhole_restore(
        &self,
    ) -> Result<crate::platform::blackhole::BlackHoleResult, String> {
        crate::platform::blackhole::restore_input_device().await
    }

    /// PipeWire virtual-device status (Linux).
    pub fn pipewire_check(&self) -> PipeWireStatus {
        #[cfg(target_os = "linux")]
        {
            let (distro, install_command) = crate::platform::pipewire::detect_install_info();
            PipeWireStatus {
                available: crate::platform::pipewire::is_available(),
                setup: crate::platform::pipewire::is_setup(),
                device_exists: crate::platform::pipewire::device_exists(),
                install_command,
                distro,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            PipeWireStatus {
                available: false,
                setup: false,
                device_exists: false,
                install_command: String::new(),
                distro: String::new(),
            }
        }
    }

    /// Web-mode server status.
    pub async fn web_status(&self) -> WebStatus {
        #[cfg(feature = "web")]
        {
            let lock = self.core.web_server.lock().await;
            if let Some(web) = lock.as_ref() {
                return WebStatus {
                    running: web.is_running(),
                    client_count: web.client_count() as u32,
                };
            }
        }
        WebStatus {
            running: false,
            client_count: 0,
        }
    }

    // ── shared config ──────────────────────────────────────────────────────

    /// `ui.json` preferences (language / theme seed).
    pub fn ui_prefs(&self) -> UiPrefs {
        crate::config::load_ui_prefs()
    }

    /// Persist `ui.json`.
    pub fn save_ui_prefs(&self, prefs: UiPrefs) -> Result<Ack, String> {
        crate::config::save_ui_prefs(&prefs)?;
        Ok(Ack::ok())
    }

    /// `theme.json` colors exported by the GUI.
    pub fn theme_colors(&self) -> ThemeColors {
        crate::config::load_theme_colors()
    }

    /// Persist `theme.json`.
    pub fn save_theme_colors(&self, colors: ThemeColors) -> Result<Ack, String> {
        crate::config::save_theme_colors(&colors)?;
        Ok(Ack::ok())
    }

    // ── mode lock ──────────────────────────────────────────────────────────

    /// Current `mode.lock` status.
    pub fn mode_status(&self) -> ModeStatus {
        match crate::mode_lock::read_lock() {
            Some(lock_info) => {
                let running = crate::mode_lock::pid_alive_public(lock_info.pid);
                let mode = match lock_info.mode {
                    crate::mode_lock::RunMode::Gui => "gui",
                    crate::mode_lock::RunMode::Cli => "cli",
                    crate::mode_lock::RunMode::Tui => "tui",
                    crate::mode_lock::RunMode::Daemon => "daemon",
                };
                ModeStatus {
                    mode: mode.to_string(),
                    pid: Some(lock_info.pid),
                    running,
                }
            }
            None => ModeStatus {
                mode: "none".to_string(),
                pid: None,
                running: false,
            },
        }
    }

    /// Release the mode lock when this process owns it.
    pub fn release_mode_lock(&self) -> Result<Ack, String> {
        if let Some(info) = crate::mode_lock::read_lock() {
            if info.pid == std::process::id() {
                crate::mode_lock::release();
            }
        }
        Ok(Ack::ok())
    }

    // ── system ─────────────────────────────────────────────────────────────

    /// Backend + contract version.
    pub fn version(&self) -> VersionInfo {
        VersionInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: micyou_api::API_VERSION,
        }
    }

    /// Daemon log file path.
    pub fn log_path(&self) -> LogPathResult {
        LogPathResult {
            path: crate::logging::log_file_path().display().to_string(),
        }
    }

    /// Tail of the daemon log.
    pub fn log_content(&self, params: LogContentParams) -> LogContentResult {
        let max_bytes = params
            .max_bytes
            .unwrap_or(256 * 1024)
            .clamp(1, 16 * 1024 * 1024);
        LogContentResult {
            content: crate::logging::read_log_tail(max_bytes).unwrap_or_default(),
        }
    }

    /// Copy the daemon log to a timestamped export location.
    pub fn log_export(&self) -> Result<LogPathResult, String> {
        let path = crate::logging::export_log(None).map_err(|e| e.to_string())?;
        Ok(LogPathResult {
            path: path.display().to_string(),
        })
    }

    /// Frontend locale (from `ui.json`).
    pub fn locale(&self) -> LocaleResult {
        LocaleResult {
            language: crate::config::load_ui_prefs().language,
        }
    }
}

fn phase_str(phase: ServerLifecyclePhase) -> &'static str {
    match phase {
        ServerLifecyclePhase::Stopped => "stopped",
        ServerLifecyclePhase::Starting => "starting",
        ServerLifecyclePhase::Running => "running",
        ServerLifecyclePhase::Stopping => "stopping",
        ServerLifecyclePhase::StoppingResidual => "stoppingResidual",
    }
}

fn mode_index(mode: TransportMode) -> u8 {
    match mode {
        TransportMode::Wifi => 0,
        TransportMode::Usb => 1,
        TransportMode::Web => 2,
    }
}

fn mode_from_index(index: u8) -> Option<TransportMode> {
    match index {
        0 => Some(TransportMode::Wifi),
        1 => Some(TransportMode::Usb),
        2 => Some(TransportMode::Web),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_device_normalization_matches_upstream() {
        assert_eq!(normalize_output_device(""), None);
        assert_eq!(normalize_output_device("  auto "), None);
        assert_eq!(normalize_output_device("default"), None);
        assert_eq!(
            normalize_output_device("CABLE Output"),
            Some("CABLE Output".to_string())
        );
    }

    #[test]
    fn phase_strings_are_stable() {
        assert_eq!(phase_str(ServerLifecyclePhase::Running), "running");
        assert_eq!(
            phase_str(ServerLifecyclePhase::StoppingResidual),
            "stoppingResidual"
        );
    }

    #[test]
    fn mode_index_roundtrips() {
        for mode in [TransportMode::Wifi, TransportMode::Usb, TransportMode::Web] {
            assert_eq!(mode_from_index(mode_index(mode)), Some(mode));
        }
        assert_eq!(mode_from_index(u8::MAX), None);
    }
}

/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! `micyou-daemon` — the headless MicYou backend.
//!
//! Typical deployments:
//! - **Sidecar**: a GUI spawns `micyou-daemon --stdio` and speaks JSON-RPC
//!   over the child's pipes.
//! - **Local service**: `micyou-daemon --ws 127.0.0.1:9610` for browser /
//!   multi-frontend setups.
//! - **Both at once** (stdio for the spawning GUI, ws for extra tools).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// MicYou headless backend daemon (libmicyou).
#[derive(Parser, Debug)]
#[command(name = "micyou-daemon", version, about, long_about = None)]
struct Args {
    /// Serve JSON-RPC over stdin/stdout (default when no --ws is given).
    #[arg(long)]
    stdio: bool,

    /// Serve JSON-RPC over WebSocket at this address, e.g. 127.0.0.1:9610
    /// (or [::1]:9610 / [::]:9610 for IPv6; the socket is dual-stack).
    #[arg(long, value_name = "ADDR")]
    ws: Option<SocketAddr>,

    /// Host a static web-UI bundle (e.g. the Flutter-web build) at
    /// http://<ws-addr>/ui/ alongside the WebSocket endpoint.
    #[arg(long, value_name = "DIR", requires = "ws")]
    web_ui: Option<PathBuf>,

    /// Override the shared config directory (default: platform micyou dir).
    #[arg(long, value_name = "DIR")]
    config_dir: Option<PathBuf>,

    /// Explicit resource directory (ONNX models / ALSA config).
    #[arg(long, value_name = "DIR")]
    resources: Option<PathBuf>,

    /// Log level: off | error | warn | info | debug | trace.
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: String,

    /// Mirror log lines to stderr (foreground/debug runs).
    #[arg(long)]
    mirror_logs: bool,

    /// Disable file logging (stderr only, per --mirror-logs).
    #[arg(long)]
    no_log_file: bool,

    /// Do not take the shared mode.lock (allows parallel backends).
    #[arg(long)]
    no_mode_lock: bool,

    /// Start the audio server immediately using server.json preferences.
    #[arg(long)]
    autostart: bool,

    /// Open the virtual audio output device at startup even before a server
    /// start (matches the stock GUI behaviour; on by default).
    #[arg(long)]
    no_output_warmup: bool,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = Args::parse();

    let level = micyou_core::logging::parse_level(&args.log_level).ok_or_else(|| {
        format!(
            "invalid --log-level {:?} (expected off|error|warn|info|debug|trace)",
            args.log_level
        )
    })?;

    let mut builder = libmicyou::Builder::new()
        .log_level(level)
        .log_mirror(args.mirror_logs)
        .log_to_file(!args.no_log_file)
        .mode_lock(!args.no_mode_lock);
    if let Some(dir) = args.config_dir {
        builder = builder.config_dir(dir);
    }
    if let Some(dir) = args.resources {
        builder = builder.resource_dir(dir);
    }
    let managed = builder.build()?;

    // Virtual mic warm-up so conferencing apps see the device immediately.
    if !args.no_output_warmup {
        let core = managed.core().clone();
        tokio::task::spawn_blocking(move || {
            let prefs = micyou_core::config::load_server_prefs();
            let device = micyou_core::service::normalize_output_device(&prefs.output_device);
            if core.ensure_audio_output(device, None) {
                log::info!("[Audio] Virtual device ready at daemon startup");
            } else {
                log::warn!(
                    "[Audio] Output device unavailable at startup (will retry on server start)"
                );
            }
        })
        .await
        .map_err(|e| format!("output warm-up panicked: {e}"))?;
    }

    if args.autostart {
        match managed.backend.start_server(Default::default()).await {
            Ok(result) => log::info!("[autostart] {}", result.message),
            Err(e) => log::error!("[autostart] failed: {e}"),
        }
    }

    let use_stdio = args.stdio || args.ws.is_none();

    let ws_task = args.ws.map(|addr| {
        let managed_ws = managed.rpc.clone();
        let web_ui = args.web_ui.clone();
        tokio::spawn(async move {
            if let Err(e) = micyou_rpc::ws::serve_ws_with_ui(managed_ws, addr, web_ui).await {
                log::error!("[rpc] websocket transport failed: {e}");
            }
        })
    });

    let result = if use_stdio {
        // stdio ends on EOF (parent process closed the pipes) — treat that
        // as the shutdown signal for sidecar deployments.
        managed.serve_stdio().await;
        Ok::<(), String>(())
    } else {
        // ws-only: run until Ctrl+C.
        tokio::signal::ctrl_c()
            .await
            .map_err(|e| format!("signal handler: {e}"))
    };

    // Graceful teardown.
    if managed.backend.core.is_running().await {
        if let Err(e) = managed.backend.stop_server().await {
            log::warn!("[shutdown] stop_server: {e}");
        }
    }
    if let Some(task) = ws_task {
        task.abort();
    }
    managed.shutdown();
    log::info!("micyou-daemon exited");
    result
}

/*
 * libmicyou — Tauri 2 reference frontend.
 *
 * Copyright (C) 2026 OrientCOMPASS
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Headless end-to-end test of the Tauri frontend's controller: embedded
//! backend over the local JSON-RPC transport (no webview, no display, no
//! daemon binary needed). The sidecar path (`Session::sidecar`) shares the
//! same controller code and is exercised by the daemon stdio smoke test in
//! CI plus manual runs.

use std::sync::Arc;
use std::time::Duration;

use micyou_api::events::ServerEvent;
use micyou_tauri_lib::controller::Session;

fn free_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

fn isolated_config_dir() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("libmicyou-tauri-fe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("MICYOU_CONFIG_DIR", &dir);
        dir
    })
    .clone()
}

async fn wait_for_event(
    rx: &mut tokio::sync::broadcast::Receiver<Arc<ServerEvent>>,
    pred: impl Fn(&ServerEvent) -> bool,
    what: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => {
                if pred(event.as_ref()) {
                    return;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("event stream closed while waiting for {what}")
            }
            Err(_) => panic!("timeout waiting for event: {what}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn embedded_session_full_cycle() {
    let scenario = async {
        let _dir = isolated_config_dir();
        let session = Session::embedded().await.expect("embedded session");

        // version handshake proves the whole stack is wired
        let version = session.version().await.expect("version");
        assert_eq!(version.api_version, micyou_api::API_VERSION);

        let mut events = session.events();

        let status = session.status().await.expect("status");
        assert_eq!(status.phase, "stopped");

        let port = free_port();
        session.start(port, "wifi").await.expect("start");

        let status = session.status().await.expect("status");
        assert!(status.is_server_running, "running after start: {status:?}");

        session.set_muted(true).await.expect("mute");
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::MuteStateChanged { muted: true }),
            "muteStateChanged(true)",
        )
        .await;

        session.stop().await.expect("stop");
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::ServerStopped),
            "serverStopped",
        )
        .await;

        let status = session.status().await.expect("status");
        assert_eq!(status.phase, "stopped");
    };

    tokio::time::timeout(Duration::from_secs(120), scenario)
        .await
        .expect("tauri controller scenario timed out");
}

/// The daemon command resolver must always produce something runnable-shaped
/// (env override wins, flags are present).
#[test]
fn daemon_command_resolution_is_stable() {
    std::env::set_var("MICYOU_DAEMON", "/tmp/definitely-not-here/micyou-daemon");
    let command = micyou_tauri_lib::controller::daemon_command();
    let debug = format!("{command:?}");
    assert!(debug.contains("--stdio"), "stdio flag present: {debug}");
    assert!(debug.contains("--no-mode-lock"), "mode-lock off: {debug}");
    std::env::remove_var("MICYOU_DAEMON");
}

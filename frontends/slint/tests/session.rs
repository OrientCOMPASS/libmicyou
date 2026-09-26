/*
 * libmicyou — Slint reference frontend.
 *
 * Copyright (C) 2026 OrientCOMPASS
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Headless end-to-end test of the frontend's backend session: embedded
//! libmicyou over the local JSON-RPC transport. Runs in CI without a display
//! (the Slint window is never created here).

use std::sync::Arc;
use std::time::Duration;

use micyou_api::events::ServerEvent;
use micyou_slint_frontend::controller::Session;

fn free_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

/// One isolated config dir per test *process* (parallel tests in this binary
/// share it; env mutation must happen exactly once).
fn isolated_config_dir() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("libmicyou-slint-{}", std::process::id()));
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
                if pred(&event) {
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
        let mut events = session.events();

        // initial status
        let status = session.status().await.expect("status");
        assert_eq!(status.phase, "stopped");
        assert!(!status.is_server_running);

        // start on loopback
        let port = free_port();
        let message = session.start(port, "wifi").await.expect("start");
        assert!(!message.is_empty());

        let status = session.status().await.expect("status");
        assert!(status.is_server_running, "running after start: {status:?}");
        assert_eq!(status.port, Some(port));

        // mute toggle surfaces as an event
        session.set_muted(true).await.expect("mute");
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::MuteStateChanged { muted: true }),
            "muteStateChanged(true)",
        )
        .await;

        // monitoring toggle
        session.set_monitoring(true).await.expect("monitoring");
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::MonitoringChanged { enabled: true }),
            "monitoringChanged(true)",
        )
        .await;

        // stop
        session.stop().await.expect("stop");
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::ServerStopped),
            "serverStopped",
        )
        .await;

        let status = session.status().await.expect("status");
        assert_eq!(status.phase, "stopped");

        session.shutdown();
    };

    tokio::time::timeout(Duration::from_secs(120), scenario)
        .await
        .expect("slint session scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_start_reports_error_not_panic() {
    let scenario = async {
        let _dir = isolated_config_dir();
        let session = Session::embedded().await.expect("embedded session");
        let port = free_port();
        session.start(port, "wifi").await.expect("first start");

        // A second start while running must fail cleanly (lifecycle gate).
        let second = session.start(port, "wifi").await;
        assert!(second.is_err(), "double start must be rejected");

        session.stop().await.expect("stop");
        session.shutdown();
    };

    tokio::time::timeout(Duration::from_secs(90), scenario)
        .await
        .expect("double-start scenario timed out");
}

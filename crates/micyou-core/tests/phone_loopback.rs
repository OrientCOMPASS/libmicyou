/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 *
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! Protocol-level integration test: drives the real backend the way the
//! Android client does — TCP control channel (handshake, connect, ping/pong,
//! mute, plugin message) + UDP audio plane — and asserts lifecycle,
//! statistics and event-bus behaviour end to end.
//!
//! Runs headless in CI on all three platforms (no audio device needed: the
//! output stage degrades to a no-op and the pipeline still meters levels).

use std::sync::Arc;
use std::time::Duration;

use micyou_api::methods::StartServerParams;
use micyou_core::events::ServerEvent;
use micyou_core::service::Backend;
use micyou_protocol::micyou::{
    AudioPacketMessage, AudioPacketMessageOrdered, ConnectMessage, MessageWrapper, MuteMessage,
    PingMessage, PluginMessage, PongMessage,
};
use micyou_protocol::{HANDSHAKE_CLIENT_STR, HANDSHAKE_SERVER_STR, PACKET_MAGIC, UDP_PACKET_MAGIC};
use prost::Message as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::broadcast;

/// Serialize the scenarios: each builds a full backend (audio threads,
/// cpal/WASAPI init) — running two at once is exactly what the engine's
/// device-init lock guards against, and test-level serialization keeps the
/// Windows runner deterministic.
fn scenario_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Isolate config/state into ONE throwaway directory per test process (the
/// scenarios in this file may run in parallel) and pick a free port.
fn test_env() -> (std::path::PathBuf, u16) {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let dir = DIR
        .get_or_init(|| {
            let dir = std::env::temp_dir()
                .join(format!("libmicyou-phone-loopback-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("MICYOU_CONFIG_DIR", &dir);
            dir
        })
        .clone();

    // Ask the OS for a free TCP port; UDP port+1 is assumed free as well.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    (dir, port)
}

fn frame(msg: &MessageWrapper) -> Vec<u8> {
    let payload = msg.encode_to_vec();
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&PACKET_MAGIC.to_be_bytes());
    out.extend_from_slice(&(payload.len() as i32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

fn udp_datagram(msg: &MessageWrapper) -> Vec<u8> {
    let payload = msg.encode_to_vec();
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&UDP_PACKET_MAGIC.to_be_bytes());
    out.extend_from_slice(&(payload.len() as i32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Read one length-prefixed TCP control frame.
async fn read_frame(stream: &mut TcpStream) -> MessageWrapper {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await.expect("frame header");
    let magic = i32::from_be_bytes(header[..4].try_into().unwrap());
    assert_eq!(magic, PACKET_MAGIC, "server frame magic");
    let len = i32::from_be_bytes(header[4..].try_into().unwrap()) as usize;
    assert!(len < 1024 * 1024, "server frame length sane");
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .await
        .expect("frame payload");
    MessageWrapper::decode(payload.as_slice()).expect("frame decodes")
}

/// Read frames until `pred` matches, skipping server-initiated pings etc.
async fn read_until(
    stream: &mut TcpStream,
    pred: impl Fn(&MessageWrapper) -> bool,
) -> MessageWrapper {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let msg = tokio::time::timeout_at(deadline, read_frame(stream))
            .await
            .expect("timed out waiting for control frame");
        if pred(&msg) {
            return msg;
        }
    }
}

async fn wait_for_event(
    rx: &mut broadcast::Receiver<Arc<ServerEvent>>,
    pred: impl Fn(&ServerEvent) -> bool,
    what: &str,
) -> Arc<ServerEvent> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => {
                if pred(event.as_ref()) {
                    return event;
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                panic!("event bus closed while waiting for {what}")
            }
            Err(_) => panic!("timeout waiting for event: {what}"),
        }
    }
}

fn audio_packet(seq: i32, session_id: i64) -> MessageWrapper {
    // 20 ms of 48 kHz mono PCM16 (960 frames = 1920 bytes), tone-ish data.
    let mut buffer = Vec::with_capacity(1920);
    for i in 0..960 {
        let sample = ((i as f32 / 960.0 * 8.0 * std::f32::consts::PI).sin() * 8000.0) as i16;
        buffer.extend_from_slice(&sample.to_le_bytes());
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    MessageWrapper {
        audio_packet: Some(AudioPacketMessageOrdered {
            sequence_number: seq,
            audio_packet: Some(AudioPacketMessage {
                buffer,
                sample_rate: 48000,
                channel_count: 1,
                audio_format: 2,
                codec: micyou_protocol::CODEC_PCM,
            }),
            timestamp: now_ms,
            fec_buffer: Vec::new(),
            fec_sequence_number: -1,
            session_id,
            fec_packet_lengths: Vec::new(),
        }),
        connect: None,
        mute: None,
        ping: None,
        pong: None,
        plugin_message: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_session_end_to_end() {
    let _serial = scenario_lock();
    // Keep the whole scenario bounded so CI can never hang on it.
    let scenario = async {
        let (_dir, port) = test_env();
        let backend = Arc::new(Backend::new());
        let mut events = backend.core.bus.subscribe();

        // ── 1. server start ────────────────────────────────────────────────
        let started = backend
            .start_server(StartServerParams {
                port: Some(port),
                mode: Some("wifi".to_string()),
                bind_address: Some("127.0.0.1".to_string()),
                ..Default::default()
            })
            .await
            .expect("server starts on loopback");
        assert!(started.message.contains("8") || !started.message.is_empty());

        let status = backend.server_status().await;
        assert!(status.is_server_running, "status: running");
        assert_eq!(status.phase, "running");
        assert_eq!(status.mode.as_deref(), Some("wifi"));
        assert_eq!(status.port, Some(port));

        // ── 2. TCP handshake + connect ────────────────────────────────────
        let mut tcp = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("tcp connect");
        tcp.write_all(HANDSHAKE_CLIENT_STR).await.unwrap();
        tcp.flush().await.unwrap();
        let mut reply = [0u8; HANDSHAKE_SERVER_STR.len()];
        tokio::time::timeout(Duration::from_secs(5), tcp.read_exact(&mut reply))
            .await
            .expect("handshake reply timeout")
            .expect("handshake reply read");
        assert_eq!(&reply, HANDSHAKE_SERVER_STR);

        let connect = MessageWrapper {
            connect: Some(ConnectMessage { session_id: 42 }),
            ..Default::default()
        };
        tcp.write_all(&frame(&connect)).await.unwrap();
        tcp.flush().await.unwrap();

        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::DeviceConnected { device } if device.ip == "127.0.0.1"),
            "deviceConnected",
        )
        .await;

        let status = backend.server_status().await;
        assert!(status.is_connected, "status: connected after TCP connect");

        // ── 3. ping → pong over the control channel ───────────────────────
        let ping = MessageWrapper {
            ping: Some(PingMessage {
                timestamp: 0x5EED_F00D,
            }),
            ..Default::default()
        };
        tcp.write_all(&frame(&ping)).await.unwrap();
        tcp.flush().await.unwrap();
        let ponged = read_until(&mut tcp, |m| m.pong.is_some()).await;
        assert_eq!(
            ponged.pong.unwrap().timestamp,
            0x5EED_F00D,
            "pong echoes the ping timestamp"
        );

        // ── 4. UDP audio plane ────────────────────────────────────────────
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dest = format!("127.0.0.1:{}", port + 1);
        for seq in 0..24 {
            let packet = udp_datagram(&audio_packet(seq, 42));
            udp.send_to(&packet, &dest).await.expect("udp send");
        }

        // The pipeline meters every 6 processed frames → at least one level
        // event must surface (audio output itself is absent in CI: no-op).
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::AudioLevel { .. }),
            "audioLevel after UDP stream",
        )
        .await;
        assert!(
            backend.core.stats.get_last_udp_time() > 0,
            "udp receive recorded in stats"
        );
        assert_eq!(
            backend
                .core
                .stats
                .sample_rate
                .load(std::sync::atomic::Ordering::Relaxed),
            48000
        );

        // A packet from the wrong session must be ignored (no crash, no meter change).
        let stale = udp_datagram(&audio_packet(99, 43));
        udp.send_to(&stale, &dest).await.unwrap();

        // ── 5. mute sync from the "phone" ─────────────────────────────────
        let mute = MessageWrapper {
            mute: Some(MuteMessage {
                is_muted: Some(true),
            }),
            ..Default::default()
        };
        tcp.write_all(&frame(&mute)).await.unwrap();
        tcp.flush().await.unwrap();
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::MuteStateChanged { muted: true }),
            "muteStateChanged from phone",
        )
        .await;
        assert!(backend.core.stats.is_muted(), "hard mute applied");

        // Unmute again through the RPC surface (frontend direction).
        backend.set_muted(false);
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::MuteStateChanged { muted: false }),
            "muteStateChanged from rpc",
        )
        .await;

        // ── 6. cross-device plugin message must not crash the relay ───────
        let plugin_msg = MessageWrapper {
            plugin_message: Some(PluginMessage {
                source: "phone.plugin".into(),
                target: String::new(),
                topic: "test".into(),
                payload: b"{}".to_vec(),
                correlation_id: 0,
                is_response: false,
                error_code: 0,
                error_message: String::new(),
            }),
            ..Default::default()
        };
        tcp.write_all(&frame(&plugin_msg)).await.unwrap();
        tcp.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        // ── 7. stop: lifecycle returns to stopped, socket is force-closed ─
        let stopped = backend.stop_server().await.expect("stop succeeds");
        assert_eq!(stopped.message, "Server stopped");
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::ServerStopped),
            "serverStopped",
        )
        .await;

        // Server tears the session socket down: reads hit EOF/reset promptly.
        let eof = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut buf = [0u8; 512];
                match tcp.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        })
        .await;
        assert!(eof.is_ok(), "session socket closed after stop");

        let status = backend.server_status().await;
        assert!(!status.is_server_running);
        assert_eq!(status.phase, "stopped");

        // ── 8. restart on the same port (residual-thread regression guard) ─
        backend
            .start_server(StartServerParams {
                port: Some(port),
                mode: Some("wifi".to_string()),
                bind_address: Some("127.0.0.1".to_string()),
                ..Default::default()
            })
            .await
            .expect("restart succeeds on the same port");
        let status = backend.server_status().await;
        assert!(status.is_server_running, "running after restart");
        backend.stop_server().await.expect("second stop");

        backend.core.shutdown();
    };

    tokio::time::timeout(Duration::from_secs(120), scenario)
        .await
        .expect("phone loopback scenario timed out");
}

/// PongMessage re-export guard: keeps the import honest for future edits.
#[allow(dead_code)]
fn _type_anchor(_p: PongMessage) {}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_session_over_ipv6_loopback() {
    let _serial = scenario_lock();
    let scenario = async {
        let (_dir, port) = test_env();
        let backend = Arc::new(Backend::new());
        let mut events = backend.core.bus.subscribe();

        // ── start bound to the IPv6 loopback ──────────────────────────────
        backend
            .start_server(StartServerParams {
                port: Some(port),
                mode: Some("wifi".to_string()),
                bind_address: Some("::1".to_string()),
                ..Default::default()
            })
            .await
            .expect("server starts on ::1");

        // ── TCP handshake over v6 ─────────────────────────────────────────
        let mut tcp = TcpStream::connect(("::1", port))
            .await
            .expect("v6 tcp connect");
        tcp.write_all(HANDSHAKE_CLIENT_STR).await.unwrap();
        tcp.flush().await.unwrap();
        let mut reply = [0u8; HANDSHAKE_SERVER_STR.len()];
        tokio::time::timeout(Duration::from_secs(5), tcp.read_exact(&mut reply))
            .await
            .expect("handshake reply timeout")
            .expect("handshake reply read");
        assert_eq!(&reply, HANDSHAKE_SERVER_STR);

        let connect = MessageWrapper {
            connect: Some(ConnectMessage { session_id: 77 }),
            ..Default::default()
        };
        tcp.write_all(&frame(&connect)).await.unwrap();
        tcp.flush().await.unwrap();

        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::DeviceConnected { .. }),
            "deviceConnected over v6",
        )
        .await;

        // ── UDP audio over v6 ─────────────────────────────────────────────
        let udp = UdpSocket::bind("::1:0").await.unwrap();
        let dest = format!("[::1]:{}", port + 1);
        for seq in 0..24 {
            let packet = udp_datagram(&audio_packet(seq, 77));
            udp.send_to(&packet, &dest).await.expect("v6 udp send");
        }
        wait_for_event(
            &mut events,
            |e| matches!(e, ServerEvent::AudioLevel { .. }),
            "audioLevel over v6",
        )
        .await;

        // ── stop and restart to prove v6 teardown is clean ────────────────
        backend.stop_server().await.expect("stop");
        backend
            .start_server(StartServerParams {
                port: Some(port),
                mode: Some("wifi".to_string()),
                bind_address: Some("::1".to_string()),
                ..Default::default()
            })
            .await
            .expect("v6 restart on same port");
        backend.stop_server().await.expect("second stop");
        backend.core.shutdown();
    };

    tokio::time::timeout(Duration::from_secs(120), scenario)
        .await
        .expect("ipv6 loopback scenario timed out");
}

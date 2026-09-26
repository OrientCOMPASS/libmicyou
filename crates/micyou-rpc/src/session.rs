/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 *
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! RPC sessions: one per connected frontend (stdio process, WebSocket
//! client, in-process channel). Sessions own an outbound line queue, an
//! event subscription filter set and optional capability flags declared via
//! `session/hello`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::mpsc;

use micyou_api::events::ServerEvent;
use micyou_api::jsonrpc::Notification;

/// Outbound line sender for one session (serialized JSON-RPC messages,
/// one per line/frame).
pub type OutboundTx = mpsc::UnboundedSender<String>;

/// A connected frontend session.
pub struct Session {
    /// Monotonic session id (per backend process).
    pub id: u64,
    /// Human-readable client name declared at hello (best effort).
    pub name: RwLock<String>,
    /// Whether the client can render plugin panels / handle UI requests.
    pub ui_capable: AtomicBool,
    /// Event filters: `"*"` or tag prefixes (`"audio"` matches audioLevel,
    /// audioMetrics, audioSpectrum). Empty = low-frequency events only.
    subscriptions: RwLock<Vec<String>>,
    out: OutboundTx,
    closed: AtomicBool,
}

impl Session {
    fn new(id: u64, out: OutboundTx) -> Self {
        Self {
            id,
            name: RwLock::new(String::from("anonymous")),
            ui_capable: AtomicBool::new(false),
            subscriptions: RwLock::new(Vec::new()),
            out,
            closed: AtomicBool::new(false),
        }
    }

    /// Replace the subscription filter set.
    pub fn set_subscriptions(&self, filters: Vec<String>) {
        if let Ok(mut subs) = self.subscriptions.write() {
            *subs = filters;
        }
    }

    /// Add filters to the current set (deduplicated).
    pub fn add_subscriptions(&self, filters: &[String]) {
        if let Ok(mut subs) = self.subscriptions.write() {
            for filter in filters {
                if !subs.iter().any(|s| s == filter) {
                    subs.push(filter.clone());
                }
            }
        }
    }

    /// Remove filters from the current set.
    pub fn remove_subscriptions(&self, filters: &[String]) {
        if let Ok(mut subs) = self.subscriptions.write() {
            subs.retain(|s| !filters.iter().any(|f| f == s));
        }
    }

    /// Current filters.
    pub fn subscriptions(&self) -> Vec<String> {
        self.subscriptions
            .read()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Whether this session wants `event` (high-frequency events need an
    /// explicit matching filter; everything else is delivered by default).
    pub fn wants(&self, event: &ServerEvent) -> bool {
        if !event.is_high_frequency() {
            return true;
        }
        let tag = event.tag();
        self.subscriptions
            .read()
            .map(|subs| subs.iter().any(|filter| matches_filter(filter, tag)))
            .unwrap_or(false)
    }

    /// Queue a raw JSON line for delivery. Returns false when the session
    /// is gone (transport dropped its receiver).
    pub fn send_line(&self, line: String) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return false;
        }
        match self.out.send(line) {
            Ok(()) => true,
            Err(_) => {
                self.closed.store(true, Ordering::Relaxed);
                false
            }
        }
    }

    /// Queue a server event notification (already filtered by [`Session::wants`]).
    pub fn send_event(&self, event: &ServerEvent) -> bool {
        let Ok(value) = serde_json::to_value(event) else {
            return true; // unserializable event: skip, session stays healthy
        };
        let note = Notification::new(micyou_api::jsonrpc::EVENT_METHOD, value);
        match serde_json::to_string(&note) {
            Ok(line) => self.send_line(line),
            Err(_) => true,
        }
    }
}

/// Filter semantics: `"*"` matches everything; otherwise the tag must start
/// with the filter (filters act as prefixes, e.g. `"audio"` → audioLevel,
/// audioMetrics, audioSpectrum; a trailing `*` is accepted and ignored).
fn matches_filter(filter: &str, tag: &str) -> bool {
    let filter = filter.trim();
    if filter == "*" {
        return true;
    }
    let prefix = filter.trim_end_matches('*');
    !prefix.is_empty() && tag.starts_with(prefix)
}

/// Shared handle to one session.
pub type SessionHandle = Arc<Session>;

/// Registry of all live sessions; the event pump iterates it on every bus
/// event, the UI bridge queries it for graphical frontends.
#[derive(Default)]
pub struct SessionRegistry {
    sessions: RwLock<HashMap<u64, SessionHandle>>,
    next_id: AtomicU64,
}

impl SessionRegistry {
    /// Create a registry.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a new session writing to `out`.
    pub fn create(&self, out: OutboundTx) -> SessionHandle {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let session = Arc::new(Session::new(id, out));
        if let Ok(mut map) = self.sessions.write() {
            map.insert(id, session.clone());
        }
        session
    }

    /// Drop a session (transport closed).
    pub fn remove(&self, id: u64) {
        if let Ok(mut map) = self.sessions.write() {
            map.remove(&id);
        }
    }

    /// Number of live sessions.
    pub fn count(&self) -> usize {
        self.sessions.read().map(|m| m.len()).unwrap_or(0)
    }

    /// Number of sessions that declared UI capability at hello.
    pub fn ui_capable_count(&self) -> usize {
        self.sessions
            .read()
            .map(|m| {
                m.values()
                    .filter(|s| s.ui_capable.load(Ordering::Relaxed))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Deliver an event to every session whose filter accepts it.
    pub fn broadcast_event(&self, event: &ServerEvent) {
        let Ok(map) = self.sessions.read() else {
            return;
        };
        for session in map.values() {
            if session.wants(event) {
                session.send_event(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_filters(filters: &[&str]) -> (SessionHandle, mpsc::UnboundedReceiver<String>) {
        let registry = SessionRegistry::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let session = registry.create(tx);
        session.set_subscriptions(filters.iter().map(|s| s.to_string()).collect());
        drop(registry); // the SessionHandle Arc keeps the session alive
        (session, rx)
    }

    #[test]
    fn low_frequency_events_always_pass() {
        let (session, _rx) = session_with_filters(&[]);
        assert!(session.wants(&ServerEvent::ServerStopped));
        assert!(session.wants(&ServerEvent::MuteStateChanged { muted: true }));
    }

    #[test]
    fn high_frequency_events_need_matching_filters() {
        let (session, _rx) = session_with_filters(&[]);
        assert!(!session.wants(&ServerEvent::AudioLevel { level: 1 }));

        let (session, _rx) = session_with_filters(&["audio"]);
        assert!(session.wants(&ServerEvent::AudioLevel { level: 1 }));
        assert!(session.wants(&ServerEvent::AudioMetrics {
            metrics: serde_json::from_value(serde_json::json!({
                "bitrate": 0, "sampleRate": 0, "latencyMs": 0, "networkLatencyMs": 0,
                "packetLossRate": 0.0, "jitterMs": 0.0, "bufferDurationMs": 0
            }))
            .unwrap()
        }));

        let (session, _rx) = session_with_filters(&["*"]);
        assert!(session.wants(&ServerEvent::AudioSpectrum {
            raw: vec![],
            processed: vec![]
        }));
    }

    #[test]
    fn filter_prefix_semantics() {
        assert!(matches_filter("*", "anything"));
        assert!(matches_filter("audio*", "audioLevel"));
        assert!(matches_filter("audio", "audioLevel"));
        assert!(!matches_filter("audio", "deviceConnected"));
        assert!(!matches_filter("", "audioLevel"));
    }

    #[test]
    fn registry_tracks_sessions_and_ui_capability() {
        let registry = SessionRegistry::new();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let s1 = registry.create(tx1);
        let s2 = registry.create(tx2);
        assert_eq!(registry.count(), 2);
        assert_eq!(registry.ui_capable_count(), 0);
        s2.ui_capable.store(true, Ordering::Relaxed);
        assert_eq!(registry.ui_capable_count(), 1);
        registry.remove(s1.id);
        assert_eq!(registry.count(), 1);
    }
}

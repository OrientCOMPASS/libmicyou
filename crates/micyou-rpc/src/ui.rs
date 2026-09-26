/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 *
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! UI delegation: the backend has no windows, so UI actions (opening plugin
//! panels) are forwarded to attached graphical frontends as `uiRequest`
//! events. [`RpcUiBridge`] is the [`micyou_core::UiBridge`] implementation
//! installed by [`crate::router::RpcService`].

use std::sync::Arc;

use micyou_api::events::{ServerEvent, UiRequest};
use micyou_core::UiBridge;
use micyou_plugin::{PluginError, PluginResult};

use crate::session::SessionRegistry;

/// Forwards UI requests to every UI-capable session.
pub struct RpcUiBridge {
    /// Live sessions; UI-capable ones (declared at `session/hello`) receive
    /// the requests.
    pub sessions: Arc<SessionRegistry>,
}

impl UiBridge for RpcUiBridge {
    fn open_plugin_panel(&self, plugin_id: &str, panel_id: &str) -> PluginResult<()> {
        if self.sessions.ui_capable_count() == 0 {
            return Err(PluginError::Runtime(
                "no graphical frontend is connected; cannot open plugin windows".into(),
            ));
        }
        self.sessions.broadcast_event(&ServerEvent::UiRequest {
            request: UiRequest::OpenPluginPanel {
                plugin_id: plugin_id.to_string(),
                panel_id: panel_id.to_string(),
            },
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionRegistry;
    use std::sync::atomic::Ordering;
    use tokio::sync::mpsc;

    #[test]
    fn without_ui_session_the_bridge_fails_cleanly() {
        let registry = SessionRegistry::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let _headless = registry.create(tx);
        let bridge = RpcUiBridge { sessions: registry };
        let error = bridge
            .open_plugin_panel("p", "panel")
            .expect_err("must fail without UI");
        assert!(matches!(error, PluginError::Runtime(_)));
    }

    #[test]
    fn ui_capable_session_receives_panel_request() {
        let registry = SessionRegistry::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = registry.create(tx);
        session.ui_capable.store(true, Ordering::Relaxed);
        let bridge = RpcUiBridge {
            sessions: registry.clone(),
        };
        bridge.open_plugin_panel("dev.plugin", "console").unwrap();

        let line = rx.try_recv().expect("ui request delivered");
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["params"]["type"], "uiRequest");
        assert_eq!(
            value["params"]["data"]["request"]["kind"],
            "openPluginPanel"
        );
        assert_eq!(value["params"]["data"]["request"]["pluginId"], "dev.plugin");
        drop(session);
    }
}

/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 *
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! The plugin → backend bridge: implements [`micyou_core::plugins::HostRpc`]
//! on top of [`RpcService::dispatch`], so plugins reach the exact same method
//! surface as remote frontends — the "plugin liberation" layer.
//!
//! Access is capability-gated twice: the manifest must declare `host.call`
//! (read/control surface) or `host.admin` (mutations/installs/lifecycle), and
//! every method is classified by [`micyou_api::methods::method_access`].
//!
//! Execution model: the dispatch future is spawned onto the captured tokio
//! runtime while the calling (synchronous, possibly foreign) thread waits on
//! a channel with a 10 s timeout — safe from native plugin threads, WASM
//! host calls and RPC worker threads alike (the daemon runtime is
//! multi-threaded, so a blocked worker never starves the spawned future).

use std::sync::Arc;

use micyou_api::methods::{method_access, MethodAccess, CAP_HOST_ADMIN, CAP_HOST_CALL};
use micyou_core::plugins::HostRpc;
use micyou_plugin::{PluginError, PluginResult};

use crate::router::RpcService;
use crate::session::SessionHandle;

/// Timeout for one bridged call (guards single-worker deadlock edge cases).
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct RpcHostBridge {
    service: Arc<RpcService>,
    /// Synthetic session context for dispatch (never receives events).
    session: SessionHandle,
    /// Runtime captured at install time (None outside a runtime → calls fail
    /// with a clear error instead of panicking).
    handle: Option<tokio::runtime::Handle>,
}

impl RpcHostBridge {
    /// Create the bridge bound to `service`'s runtime context.
    pub fn new(service: Arc<RpcService>, session: SessionHandle) -> Self {
        Self {
            service,
            session,
            handle: tokio::runtime::Handle::try_current().ok(),
        }
    }
}

impl HostRpc for RpcHostBridge {
    fn call(
        &self,
        capabilities: &[String],
        method: &str,
        params_json: &str,
    ) -> PluginResult<String> {
        let has_admin = capabilities.iter().any(|c| c == CAP_HOST_ADMIN);
        let has_call = has_admin || capabilities.iter().any(|c| c == CAP_HOST_CALL);

        match method_access(method) {
            MethodAccess::Denied => {
                return Err(PluginError::PermissionDenied(format!(
                    "method {method} is not available to plugins"
                )))
            }
            MethodAccess::Admin if !has_admin => {
                return Err(PluginError::PermissionDenied(format!(
                    "method {method} requires the host.admin capability"
                )))
            }
            MethodAccess::Call if !has_call => {
                return Err(PluginError::PermissionDenied(format!(
                    "method {method} requires the host.call capability"
                )))
            }
            _ => {}
        }

        let params: serde_json::Value = if params_json.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(params_json)
                .map_err(|e| PluginError::Validation(format!("invalid params json: {e}")))?
        };

        let handle = self.handle.clone().ok_or_else(|| {
            PluginError::Runtime("host bridge has no tokio runtime (backend not serving?)".into())
        })?;

        let service = self.service.clone();
        let session = self.session.clone();
        let method_owned = method.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        handle.spawn(async move {
            let result = service
                .dispatch(&session, &method_owned, Some(params))
                .await;
            let _ = tx.send(result);
        });

        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(Ok(value)) => {
                serde_json::to_string(&serde_json::json!({ "ok": true, "result": value }))
                    .map_err(|e| PluginError::Runtime(format!("encode result: {e}")))
            }
            Ok(Err(error)) => serde_json::to_string(&serde_json::json!({
                "ok": false,
                "error": {
                    "code": error.code,
                    "message": error.message,
                    "data": error.data,
                }
            }))
            .map_err(|e| PluginError::Runtime(format!("encode error: {e}"))),
            Err(_) => Err(PluginError::Runtime(format!(
                "call_host({method}) timed out after {} s",
                CALL_TIMEOUT.as_secs()
            ))),
        }
    }
}

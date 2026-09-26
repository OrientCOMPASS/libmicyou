/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 *
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE.
 */

//! JSON-RPC 2.0 wire envelope shared by the backend router, the client SDK
//! and any third-party integration.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version string sent in every message.
pub const VERSION: &str = "2.0";

/// Method name of server→client event notifications. The params object is
/// `{"type": <event tag>, "data": <payload>}` — the serialized
/// [`crate::events::ServerEvent`].
pub const EVENT_METHOD: &str = "event";

/// Request/notification identifier. `Null` appears only in error responses
/// to unparsable requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Num(i64),
    Str(String),
    Null,
}

/// An incoming JSON-RPC request or notification (no `id` → notification).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Must be `"2.0"`; validated by the router.
    pub jsonrpc: String,
    /// Absent for notifications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    /// Method name, e.g. `"server/start"`.
    pub method: String,
    /// Parameters object (all MicYou methods use by-name params).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Whether this message expects a response.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Error code (JSON-RPC reserved or [`crate::error`] application range).
    pub code: i64,
    /// Short human-readable description.
    pub message: String,
    /// Optional structured details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Convenience constructor.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Constructor with structured data.
    pub fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    /// Parse error (`-32700`).
    pub fn parse_error() -> Self {
        Self::new(crate::error::PARSE_ERROR, "Parse error")
    }

    /// Invalid request (`-32600`).
    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self::new(crate::error::INVALID_REQUEST, detail)
    }

    /// Method not found (`-32601`).
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            crate::error::METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        )
    }

    /// Invalid params (`-32602`).
    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self::new(crate::error::INVALID_PARAMS, detail)
    }

    /// Internal error (`-32603`).
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(crate::error::INTERNAL_ERROR, detail)
    }
}

/// An outgoing JSON-RPC response (success or error, never both).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Echoed request id.
    pub id: Id,
    /// Present on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Present on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// Success response.
    pub fn ok(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    pub fn err(id: Id, error: RpcError) -> Self {
        Self {
            jsonrpc: VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// An outgoing JSON-RPC notification (no id, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Method name; [`EVENT_METHOD`] for backend events.
    pub method: String,
    /// Notification payload.
    pub params: Value,
}

impl Notification {
    /// Build a notification message.
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: VERSION.to_string(),
            method: method.into(),
            params,
        }
    }

    /// Build an event notification from a serialized [`crate::events::ServerEvent`].
    pub fn event(event: &crate::events::ServerEvent) -> serde_json::Result<Self> {
        Ok(Self::new(EVENT_METHOD, serde_json::to_value(event)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_by_name_params_request() {
        let raw = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "server/start",
            "params": {"port": 8554}
        });
        let req: Request = serde_json::from_value(raw).unwrap();
        assert_eq!(req.id, Some(Id::Num(7)));
        assert_eq!(req.method, "server/start");
        assert!(!req.is_notification());
        assert_eq!(req.params.unwrap()["port"], 5554 + 3000);
    }

    #[test]
    fn parses_notification_without_id() {
        let raw = json!({"jsonrpc": "2.0", "method": "session/subscribe"});
        let req: Request = serde_json::from_value(raw).unwrap();
        assert!(req.is_notification());
    }

    #[test]
    fn response_skips_absent_fields() {
        let ok = Response::ok(Id::Num(1), json!({"message": "done"}));
        let value = serde_json::to_value(&ok).unwrap();
        assert!(value.get("error").is_none());
        assert_eq!(value["result"]["message"], "done");

        let err = Response::err(Id::Str("a".into()), RpcError::method_not_found("x/y"));
        let value = serde_json::to_value(&err).unwrap();
        assert!(value.get("result").is_none());
        assert_eq!(value["error"]["code"], crate::error::METHOD_NOT_FOUND);
    }

    #[test]
    fn event_notification_shape_is_stable() {
        let note =
            Notification::event(&crate::events::ServerEvent::AudioLevel { level: 3 }).unwrap();
        let value = serde_json::to_value(&note).unwrap();
        assert_eq!(value["method"], EVENT_METHOD);
        assert_eq!(value["params"]["type"], "audioLevel");
        assert_eq!(value["params"]["data"]["level"], 3);
    }
}

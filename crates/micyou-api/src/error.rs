/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! Error codes.
//!
//! `-32768..=-32000` is the JSON-RPC reserved range; application errors use
//! `-32000..=-31000` as allowed by the specification.

/// Invalid JSON was received.
pub const PARSE_ERROR: i64 = -32700;
/// The JSON is not a valid Request object.
pub const INVALID_REQUEST: i64 = -32600;
/// The method does not exist / is not available.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Invalid method parameter(s).
pub const INVALID_PARAMS: i64 = -32602;
/// Internal JSON-RPC error.
pub const INTERNAL_ERROR: i64 = -32603;

// --- application range -----------------------------------------------------

/// Generic backend failure (message carries details).
pub const SERVER_ERROR: i64 = -32000;
/// The audio server is not running (or already running) for this operation.
pub const BAD_STATE: i64 = -32001;
/// The caller lacks the capability/permission for this method (plugin bridge).
pub const FORBIDDEN: i64 = -32002;
/// No graphical frontend is attached to fulfil a UI request.
pub const UI_UNAVAILABLE: i64 = -32003;
/// A resource (plugin, device, file) was not found.
pub const NOT_FOUND: i64 = -32004;
/// The operation timed out.
pub const TIMEOUT: i64 = -32005;

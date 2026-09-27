/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! MicYou JSON-RPC 2.0 layer.
//!
//! - [`router::RpcService`] — method dispatch + event pump (transport-agnostic).
//! - [`session`] — per-frontend sessions with event subscription filters.
//! - [`stdio`] — newline-delimited JSON over stdin/stdout (sidecar deployments).
//! - [`ws`] — WebSocket transport (`ws://addr/rpc`) for browser/remote clients.
//! - [`local`] — in-process duplex channel transport for embedded Rust hosts
//!   (a Tauri backend, CLI or TUI can drive the daemon without sockets).
//! - [`ui`] — UI-request delegation to attached graphical frontends.

pub mod host_bridge;
pub mod local;
pub mod router;
pub mod session;
pub mod stdio;
pub mod ui;
pub mod ws;

pub use router::RpcService;
pub use session::{Session, SessionHandle, SessionRegistry};

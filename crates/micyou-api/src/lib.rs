/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! MicYou frontend–backend contract.
//!
//! This crate is the single source of truth for what crosses the process
//! boundary between a frontend (Tauri GUI, CLI, TUI, web page, third-party
//! tool) and the libmicyou backend:
//!
//! - [`jsonrpc`] — the JSON-RPC 2.0 wire envelope and standard error codes;
//! - [`methods`] — every RPC method name plus its parameter/result DTOs;
//! - [`events`] — the [`events::ServerEvent`] catalogue pushed to subscribers;
//! - [`config`] — the shared on-disk preference files (settings/server/ui/theme);
//! - [`error`] — application error codes beyond the JSON-RPC reserved range.
//!
//! It deliberately depends only on serde plus the light domain crates
//! (`micyou-audio`, `micyou-transport`, `micyou-plugin`) so client SDKs and
//! third-party frontends can pull in the contract without the backend stack.

pub mod config;
pub mod error;
pub mod events;
pub mod jsonrpc;
pub mod methods;

/// Version of this contract. Bumped on any breaking change to method names,
/// parameter shapes or event payloads; reported by `session/hello`.
pub const API_VERSION: u32 = 1;

pub use config::{ServerPrefs, ThemeColors, UiPrefs};
pub use events::{AecStatus, ServerEvent, UiRequest};
pub use jsonrpc::{Id, Request, Response, RpcError};

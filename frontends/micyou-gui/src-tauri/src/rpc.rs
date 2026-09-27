/* libmicyou — micyou-gui frontend (original MicYou Vue UI). */

//! The single generic bridge command used by the webview adapter: every
//! mapped backend call funnels through here as raw JSON-RPC (method + params
//! in, result out), so the shell needs no per-command wrappers.

use serde_json::Value;
use tauri::State;

use crate::backend::{rpc, BackendState};

/// Invoke one libmicyou RPC method on the connected daemon.
#[tauri::command]
pub async fn backend_rpc(
    state: State<'_, BackendState>,
    method: String,
    params: Option<Value>,
) -> Result<Value, String> {
    let client = { state.client.lock().await.clone() }.ok_or(crate::backend::NO_SESSION)?;
    // Normalize missing/null params to `{}`: the router deserializes
    // by-name params and only tolerates `null` on methods with defaults.
    let params = match params {
        None | Some(Value::Null) => serde_json::json!({}),
        Some(value) => value,
    };
    rpc(&client, &method, params).await
}

//! Authentication-related Tauri commands.
//!
//! These commands handle token storage and retrieval, exposed to the
//! JavaScript frontend via Tauri's IPC system.
//!
//! # Rust Learning: Tauri Commands
//!
//! The `#[tauri::command]` attribute macro transforms a regular Rust function
//! into a command that can be called from JavaScript:
//!
//! ```javascript
//! // From JavaScript:
//! import { invoke } from '@tauri-apps/api/core';
//! const result = await invoke('set_token', { token: 'abc123' });
//! ```
//!
//! The function parameters become the expected JavaScript object properties.

use tauri::State;

use crate::agents::init::clear_all_instances;
use crate::AppState;

/// Stores the API token securely in the OS keychain.
///
/// # Arguments
///
/// * `token` - The bearer token to store
///
/// # Returns
///
/// * `Ok(())` - Token stored successfully
/// * `Err(String)` - Error message if storage failed
///
/// # Rust Learning: Async Commands
///
/// The `async` keyword allows us to use `.await` inside the function.
/// Even though token storage is synchronous, we mark it async for
/// consistency with other commands and future flexibility.
#[tauri::command]
pub async fn set_token(token: String, state: State<'_, AppState>) -> Result<(), String> {
    state
        .token_storage
        .set_token(&token)
        .map_err(|e| format!("Failed to store token: {}", e))?;

    // Scheduled agents live in workspace_agents now and are populated by
    // populate_scheduler_from_workspace_agents at boot. No login-time restore needed.
    Ok(())
}

/// Checks if a token is stored (user is authenticated).
///
/// # Returns
///
/// * `Ok(true)` - Token exists (user is logged in)
/// * `Ok(false)` - No token stored (user not logged in)
/// * `Err(String)` - Error accessing keychain
///
/// # Why not `get_token`?
///
/// We intentionally don't expose the actual token to JavaScript.
/// The token stays in Rust and is used internally for API calls.
/// JS only needs to know if the user is authenticated, not the token itself.
#[tauri::command]
pub async fn has_token(state: State<'_, AppState>) -> Result<bool, String> {
    state
        .token_storage
        .get_token()
        .map(|opt| opt.is_some())
        .map_err(|e| format!("Failed to check token: {}", e))
}

/// Clears the stored token (logout).
///
/// This is idempotent - calling it when no token exists is not an error.
/// Also clears all agent instances since they require authentication.
#[tauri::command]
pub async fn clear_token(state: State<'_, AppState>) -> Result<(), String> {
    // Clear all agent instances first
    clear_all_instances(&state.scheduler).await;

    state
        .token_storage
        .clear_token()
        .map_err(|e| format!("Failed to clear token: {}", e))
}

/// Stores the API base URL.
///
/// The base URL is not sensitive, so we store it in memory (with file
/// persistence planned for later).
///
/// # Arguments
///
/// * `url` - The base URL (e.g., "https://app.netdata.cloud")
#[tauri::command]
pub async fn set_base_url(url: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut base_url = state
        .base_url
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    *base_url = url;
    Ok(())
}

/// Retrieves the current API base URL.
#[tauri::command]
pub async fn get_base_url(state: State<'_, AppState>) -> Result<String, String> {
    let base_url = state
        .base_url
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    Ok(base_url.clone())
}

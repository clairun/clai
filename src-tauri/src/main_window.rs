//! The app's single window: bringing it back to the front.

use tauri::{AppHandle, Manager};

/// Label of the window declared in `tauri.conf.json`, which sets none.
pub const MAIN_WINDOW: &str = "main";

/// Shows, unminimizes and focuses the main window.
pub fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        tracing::warn!("Main window not found; cannot bring it to the front");
        return;
    };
    if let Err(e) = window
        .show()
        .and_then(|()| window.unminimize())
        .and_then(|()| window.set_focus())
    {
        tracing::warn!("Failed to bring the main window to the front: {}", e);
    }
}

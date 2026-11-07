//! What the sandbox can enforce on this PC.

use crate::assistant::sandbox::SandboxStatus;

#[tauri::command]
pub fn get_sandbox_status() -> SandboxStatus {
    crate::assistant::sandbox::status()
}

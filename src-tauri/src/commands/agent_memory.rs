//! Commands for the settings UI to view and edit a shared agent definition's
//! professional memory. Archived definitions keep theirs.

use std::path::PathBuf;

use tauri::State;

use crate::agent_memory::{self, AgentMemoryFile};
use crate::AppState;

/// The memory directory of a definition in the library.
fn memory_dir(state: &AppState, definition_id: &str) -> Result<PathBuf, String> {
    let app = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?
        .get();
    if !app
        .agent_definitions
        .iter()
        .any(|definition| definition.id == definition_id)
    {
        return Err(format!("Agent definition not found: {definition_id}"));
    }
    agent_memory::memory_dir(&crate::paths::clai_agents_root(), definition_id)
}

#[tauri::command]
pub async fn agent_memory_list(
    state: State<'_, AppState>,
    definition_id: String,
) -> Result<Vec<AgentMemoryFile>, String> {
    agent_memory::list(&memory_dir(&state, &definition_id)?)
}

#[tauri::command]
pub async fn agent_memory_read(
    state: State<'_, AppState>,
    definition_id: String,
    path: String,
) -> Result<String, String> {
    agent_memory::read(&memory_dir(&state, &definition_id)?, &path)
}

#[tauri::command]
pub async fn agent_memory_write(
    state: State<'_, AppState>,
    definition_id: String,
    path: String,
    content: String,
) -> Result<(), String> {
    let dir = memory_dir(&state, &definition_id)?;
    agent_memory::ensure_memory_dir(&dir)?;
    agent_memory::write(&dir, &path, &content)
}

#[tauri::command]
pub async fn agent_memory_delete(
    state: State<'_, AppState>,
    definition_id: String,
    path: String,
) -> Result<(), String> {
    agent_memory::delete(&memory_dir(&state, &definition_id)?, &path)
}

//! Commands for the settings UI to view and edit a shared agent definition's
//! craft. Archived definitions keep theirs.

use std::path::PathBuf;

use tauri::State;

use crate::agent_craft::{self, AgentCraftFile};
use crate::AppState;

/// The craft directory of a definition in the library.
fn craft_dir(state: &AppState, definition_id: &str) -> Result<PathBuf, String> {
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
    agent_craft::craft_dir(&crate::paths::clai_agents_root(), definition_id)
}

#[tauri::command]
pub async fn agent_craft_list(
    state: State<'_, AppState>,
    definition_id: String,
) -> Result<Vec<AgentCraftFile>, String> {
    agent_craft::list(&craft_dir(&state, &definition_id)?)
}

#[tauri::command]
pub async fn agent_craft_read(
    state: State<'_, AppState>,
    definition_id: String,
    path: String,
) -> Result<String, String> {
    agent_craft::read(&craft_dir(&state, &definition_id)?, &path)
}

#[tauri::command]
pub async fn agent_craft_write(
    state: State<'_, AppState>,
    definition_id: String,
    path: String,
    content: String,
) -> Result<(), String> {
    let dir = craft_dir(&state, &definition_id)?;
    agent_craft::ensure_craft_dir(&dir)?;
    agent_craft::write(&dir, &path, &content)
}

#[tauri::command]
pub async fn agent_craft_delete(
    state: State<'_, AppState>,
    definition_id: String,
    path: String,
) -> Result<(), String> {
    agent_craft::delete(&craft_dir(&state, &definition_id)?, &path)
}

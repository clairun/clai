//! Commands for the settings UI to view and edit a shared agent definition's
//! craft. Archived definitions keep theirs.

use std::path::{Path, PathBuf};

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
    let library = app.agent_definitions.iter().map(|d| d.id.as_str());
    library_craft_dir(&crate::paths::clai_agents_root(), library, definition_id)
}

fn library_craft_dir<'a>(
    agents_root: &Path,
    mut library: impl Iterator<Item = &'a str>,
    definition_id: &str,
) -> Result<PathBuf, String> {
    let dir = agent_craft::craft_dir(agents_root, definition_id)?;
    if library.any(|id| id == definition_id) {
        Ok(dir)
    } else {
        Err(format!("Agent definition not found: {definition_id}"))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_valid_ids_in_the_library_have_a_craft_dir() {
        let root = Path::new("agents");
        let library = ["8d3bb9d1-a92a", "../escape"];
        assert_eq!(
            library_craft_dir(root, library.into_iter(), "8d3bb9d1-a92a"),
            Ok(root.join("8d3bb9d1-a92a").join("craft"))
        );
        assert_eq!(
            library_craft_dir(root, library.into_iter(), "unknown"),
            Err("Agent definition not found: unknown".to_string())
        );
        // Being in the library does not make an id safe.
        assert!(library_craft_dir(root, library.into_iter(), "../escape")
            .unwrap_err()
            .starts_with("Invalid agent definition id"));
    }
}

//! Tauri commands for "open in app" actions and the Settings →
//! Applications section. See `crate::system_apps` for the mechanics.

use std::path::Path;

use tauri::State;

use crate::commands::workspace::resolve_workspace_descriptor;
use crate::system_apps::{self, resolve_contained_path, SystemAppsConfig, SystemAppsStatus};
use crate::AppState;

fn workspace_root(
    state: &AppState,
    workspace_id: Option<String>,
) -> Result<std::path::PathBuf, String> {
    let descriptor = resolve_workspace_descriptor(state, workspace_id)?;
    descriptor
        .root_path
        .ok_or_else(|| "This workspace has no filesystem root.".to_string())
}

fn system_apps_config(state: &AppState) -> Result<SystemAppsConfig, String> {
    let manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    Ok(manager.get().system_apps)
}

/// Probe the host for known editors/terminals (Settings dropdowns).
#[tauri::command]
pub fn system_apps_detect() -> SystemAppsStatus {
    system_apps::detect_system_apps()
}

#[tauri::command]
pub fn get_system_apps_settings(state: State<'_, AppState>) -> Result<SystemAppsConfig, String> {
    system_apps_config(state.inner())
}

#[tauri::command]
pub fn set_system_apps_settings(
    settings: SystemAppsConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    manager
        .update(|config| config.system_apps = settings.clone())
        .map_err(|e| format!("Failed to save settings: {}", e))
}

/// Open a workspace file/folder in the requested target app.
/// `rel_path: None` targets the workspace root. Paths are contained to
/// the workspace root (canonicalized on both sides).
#[tauri::command]
pub fn open_workspace_path(
    workspace_id: Option<String>,
    rel_path: Option<String>,
    target: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = workspace_root(state.inner(), workspace_id)?;
    let path = resolve_contained_path(&root, rel_path.as_deref())?;
    let is_dir = path.is_dir();
    let config = system_apps_config(state.inner())?;
    match target.as_str() {
        "editor" => system_apps::open_in_editor(&config, &path, is_dir),
        "system" => system_apps::open_with_system(&path),
        "terminal" => {
            let dir = if is_dir {
                path.as_path()
            } else {
                path.parent().unwrap_or(root.as_path())
            };
            system_apps::open_terminal(&config, dir)
        }
        other => Err(format!("Unknown open target `{}`.", other)),
    }
}

/// Copy user-picked files into the workspace (the "+ Add files" action).
/// Sources come from the native file dialog; destination is the
/// workspace root (or `dest_rel_path` inside it). Name collisions get a
/// ` (n)` suffix rather than overwriting. Returns the copied file names.
#[tauri::command]
pub fn workspace_import_files(
    workspace_id: Option<String>,
    source_paths: Vec<String>,
    dest_rel_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let root = workspace_root(state.inner(), workspace_id)?;
    let dest_dir = resolve_contained_path(&root, dest_rel_path.as_deref())?;
    if !dest_dir.is_dir() {
        return Err("Destination is not a directory.".to_string());
    }

    let mut copied = Vec::new();
    for source in &source_paths {
        let source = Path::new(source);
        if !source.is_file() {
            return Err(format!("`{}` is not a regular file.", source.display()));
        }
        let name = source
            .file_name()
            .ok_or_else(|| format!("`{}` has no file name.", source.display()))?
            .to_string_lossy()
            .to_string();
        let dest = unique_destination(&dest_dir, &name);
        std::fs::copy(source, &dest)
            .map_err(|e| format!("Failed to copy `{}`: {}", source.display(), e))?;
        copied.push(
            dest.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or(name),
        );
    }
    Ok(copied)
}

/// `report.md` → `report (1).md` → `report (2).md` … until free.
fn unique_destination(dir: &Path, name: &str) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{}", ext)),
        _ => (name.to_string(), String::new()),
    };
    for n in 1u32.. {
        let candidate = dir.join(format!("{} ({}){}", stem, n, ext));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("u32 exhausted finding a unique file name");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_destination_suffixes_collisions() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            unique_destination(dir.path(), "report.md"),
            dir.path().join("report.md")
        );
        std::fs::write(dir.path().join("report.md"), "x").unwrap();
        assert_eq!(
            unique_destination(dir.path(), "report.md"),
            dir.path().join("report (1).md")
        );
        std::fs::write(dir.path().join("report (1).md"), "x").unwrap();
        assert_eq!(
            unique_destination(dir.path(), "report.md"),
            dir.path().join("report (2).md")
        );
        // No extension.
        std::fs::write(dir.path().join("Makefile"), "x").unwrap();
        assert_eq!(
            unique_destination(dir.path(), "Makefile"),
            dir.path().join("Makefile (1)")
        );
    }
}

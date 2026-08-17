//! Tauri commands for "open in app" actions and the Settings →
//! Applications section. See `crate::system_apps` for the mechanics.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tauri::{AppHandle, State};

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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceImportKind {
    Files,
    Folders,
}

/// Pick files/folders in the backend and copy them into the workspace (the "+ Add" action).
/// Destination is the workspace root (or `dest_rel_path` inside it). Name collisions
/// get a ` (n)` suffix rather than overwriting. Returns the copied entry names, or
/// an empty list when the user cancels the native picker.
#[tauri::command]
pub async fn workspace_import_files(
    workspace_id: Option<String>,
    import_kind: WorkspaceImportKind,
    dest_rel_path: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let root = workspace_root(state.inner(), workspace_id)?;
    let dest_dir = resolve_contained_path(&root, dest_rel_path.as_deref())?;
    if !dest_dir.is_dir() {
        return Err("Destination is not a directory.".to_string());
    }

    // The dialog runs in the backend, so a renderer can choose only the picker
    // mode, not arbitrary host paths to recursively read into the workspace.
    let picked = match import_kind {
        WorkspaceImportKind::Files => app
            .dialog()
            .file()
            .set_title("Add files to workspace")
            .blocking_pick_files(),
        WorkspaceImportKind::Folders => app
            .dialog()
            .file()
            .set_title("Add folders to workspace")
            .blocking_pick_folders(),
    };

    let Some(source_paths) = picked else {
        return Ok(Vec::new());
    };
    let source_paths = source_paths
        .into_iter()
        .map(|path| {
            path.into_path()
                .map_err(|error| format!("Invalid import path: {}", error))
        })
        .collect::<Result<Vec<_>, _>>()?;

    tokio::task::spawn_blocking(move || import_picked_paths(source_paths, dest_dir))
        .await
        .map_err(|error| format!("Import task did not complete: {}", error))?
}

fn import_picked_paths(
    source_paths: Vec<PathBuf>,
    dest_dir: PathBuf,
) -> Result<Vec<String>, String> {
    let mut copied = Vec::new();
    for source in &source_paths {
        let dest = copy_picked_source_to_unique_destination(source, &dest_dir)?;
        copied.push(
            dest.file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("Copied path `{}` has no file name.", dest.display()))?
                .to_string(),
        );
    }
    Ok(copied)
}

fn copy_picked_source_to_unique_destination(
    source: &Path,
    dest_dir: &Path,
) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|e| format!("Failed to stat `{}`: {}", source.display(), e))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Refusing to import a symlink: {}",
            source.display()
        ));
    }
    let is_dir = metadata.is_dir();
    if !metadata.is_file() && !is_dir {
        return Err(format!(
            "`{}` is not a regular file or directory.",
            source.display()
        ));
    }

    let name = source
        .file_name()
        .ok_or_else(|| format!("`{}` has no file name.", source.display()))?
        .to_string_lossy()
        .to_string();

    if is_dir && crate::commands::workspace::is_skipped_artifact_dir_name(&name) {
        return Err(format!(
            "Refusing to import a protected folder: {}",
            source.display()
        ));
    }

    if is_dir {
        let canon_source = source
            .canonicalize()
            .map_err(|error| format!("Failed to resolve `{}`: {}", source.display(), error))?;
        let canon_dest = dest_dir.canonicalize().map_err(|error| {
            format!(
                "Failed to resolve destination `{}`: {}",
                dest_dir.display(),
                error
            )
        })?;
        if canon_dest.starts_with(&canon_source) {
            return Err(format!(
                "Cannot import `{}` into itself or one of its subfolders.",
                source.display()
            ));
        }
    }

    crate::commands::workspace::copy_artifact_to_unique_destination(source, dest_dir, &name, is_dir)
}

/// `report.md` → `report (1).md` → `report (2).md` … until free.
pub(crate) fn destination_candidate(dir: &Path, name: &str, copy_index: u32) -> std::path::PathBuf {
    if copy_index == 0 {
        return dir.join(name);
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{}", ext)),
        _ => (name.to_string(), String::new()),
    };
    dir.join(format!("{} ({}){}", stem, copy_index, ext))
}

pub(crate) fn copy_to_unique_destination(
    source: &Path,
    dir: &Path,
    name: &str,
) -> Result<std::path::PathBuf, String> {
    let mut input =
        File::open(source).map_err(|e| format!("Failed to open `{}`: {}", source.display(), e))?;

    for n in 0u32.. {
        let candidate = destination_candidate(dir, name, n);
        let mut output = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create `{}`: {}",
                    candidate.display(),
                    error
                ));
            }
        };

        input
            .rewind()
            .map_err(|e| format!("Failed to rewind `{}`: {}", source.display(), e))?;
        if let Err(error) = io::copy(&mut input, &mut output) {
            let _ = std::fs::remove_file(&candidate);
            return Err(format!("Failed to copy `{}`: {}", source.display(), error));
        }
        return Ok(candidate);
    }

    unreachable!("u32 exhausted finding a unique file name")
}

#[cfg(test)]
fn unique_destination(dir: &Path, name: &str) -> std::path::PathBuf {
    for n in 1u32.. {
        let candidate = destination_candidate(dir, name, n - 1);
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

    #[test]
    fn copy_to_unique_destination_does_not_overwrite_existing_file() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("report.md");
        std::fs::write(&source, "new").unwrap();
        std::fs::write(dest_dir.path().join("report.md"), "old").unwrap();

        let copied = copy_to_unique_destination(&source, dest_dir.path(), "report.md").unwrap();

        assert_eq!(copied, dest_dir.path().join("report (1).md"));
        assert_eq!(
            std::fs::read_to_string(dest_dir.path().join("report.md")).unwrap(),
            "old"
        );
        assert_eq!(std::fs::read_to_string(copied).unwrap(), "new");
    }

    #[test]
    fn copy_to_unique_destination_handles_extensionless_names() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("Makefile");
        std::fs::write(&source, "new").unwrap();
        std::fs::write(dest_dir.path().join("Makefile"), "old").unwrap();

        let copied = copy_to_unique_destination(&source, dest_dir.path(), "Makefile").unwrap();

        assert_eq!(copied, dest_dir.path().join("Makefile (1)"));
        assert_eq!(std::fs::read_to_string(copied).unwrap(), "new");
    }

    #[test]
    fn copy_picked_source_accepts_files_and_directories() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();

        let file = source_dir.path().join("note.md");
        std::fs::write(&file, "file").unwrap();
        let copied_file = copy_picked_source_to_unique_destination(&file, dest_dir.path()).unwrap();
        assert_eq!(copied_file.file_name().unwrap(), "note.md");
        assert_eq!(std::fs::read_to_string(copied_file).unwrap(), "file");

        let tree = source_dir.path().join("project");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::create_dir_all(tree.join(".git")).unwrap();
        std::fs::write(tree.join("README.md"), "readme").unwrap();
        std::fs::write(tree.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(tree.join(".git/config"), "ignored").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hostname", tree.join("host")).unwrap();
        #[cfg(unix)]
        let _socket = std::os::unix::net::UnixListener::bind(tree.join("app.sock")).unwrap();

        let copied_dir = copy_picked_source_to_unique_destination(&tree, dest_dir.path()).unwrap();
        assert_eq!(copied_dir.file_name().unwrap(), "project");
        assert_eq!(
            std::fs::read_to_string(copied_dir.join("README.md")).unwrap(),
            "readme"
        );
        assert_eq!(
            std::fs::read_to_string(copied_dir.join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
        assert!(!copied_dir.join(".git").exists());
        #[cfg(unix)]
        assert!(!copied_dir.join("host").exists());
        #[cfg(unix)]
        assert!(!copied_dir.join("app.sock").exists());

        let copied_collision =
            copy_picked_source_to_unique_destination(&tree, dest_dir.path()).unwrap();
        assert_eq!(copied_collision.file_name().unwrap(), "project (1)");
    }

    #[test]
    fn copy_picked_source_rejects_folder_containing_destination() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = source_dir.path().join("workspace");
        std::fs::create_dir(&dest_dir).unwrap();

        let err =
            copy_picked_source_to_unique_destination(source_dir.path(), &dest_dir).unwrap_err();

        assert!(err.contains("Cannot import"));
    }

    #[test]
    fn copy_picked_source_multi_import_keeps_prior_copy_on_later_failure() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let file = source_dir.path().join("note.md");
        std::fs::write(&file, "copied").unwrap();
        let missing = source_dir.path().join("missing.md");

        let err = import_picked_paths(
            vec![file.clone(), missing.clone()],
            dest_dir.path().to_path_buf(),
        )
        .unwrap_err();

        assert!(err.contains("Failed to stat"));
        assert_eq!(
            std::fs::read_to_string(dest_dir.path().join("note.md")).unwrap(),
            "copied"
        );
    }

    #[test]
    fn copy_picked_source_rejects_protected_folder_as_root() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let protected = source_dir.path().join("node_modules");
        std::fs::create_dir(&protected).unwrap();

        let err =
            copy_picked_source_to_unique_destination(&protected, dest_dir.path()).unwrap_err();

        assert!(err.contains("protected folder"));
    }

    #[cfg(unix)]
    #[test]
    fn copy_picked_source_rejects_top_level_symlink() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let target = source_dir.path().join("target.txt");
        let link = source_dir.path().join("link.txt");
        std::fs::write(&target, "target").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = copy_picked_source_to_unique_destination(&link, dest_dir.path()).unwrap_err();

        assert!(err.contains("symlink"));
    }
}

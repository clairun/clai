//! Skill catalog commands.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::config::bundled;
use crate::config::{
    discover_skills, discover_skills_with_diagnostics, SkillDefinition, SkillSourceConfig,
    SkillSourceDiagnostic, SkillSourceKind,
};
use crate::AppState;

/// Removes all workspace-local skill references that belong to the given source.
///
/// Walks every `workspace_agents` row whose JSON-array `selected_skill_ids`
/// might reference the source (LIKE filter on the source id prefix), then
/// rewrites the array with the matching ids removed.
fn sweep_workspace_agent_skill_ids(state: &AppState, source_id: &str) -> Result<(), String> {
    let prefix = format!("{}:", source_id);
    let app_config = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get();
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();
    for locator in locators {
        // Atomic RMW (see workspace_config::update); unchanged configs are
        // rewritten with identical content, which the atomic save makes
        // harmless — sweeps only run on rare rename/delete actions.
        let (changed, config) =
            crate::config::workspace_config::update(&locator.root_path, |config| {
                let mut changed = false;
                let now = chrono::Utc::now().timestamp_millis();
                for agent in &mut config.agents {
                    let ids = crate::config::workspace_config::refs_to_skill_ids(
                        &app_config,
                        &agent.selected_skills,
                    );
                    if ids.iter().any(|skill_id| skill_id.starts_with(&prefix)) {
                        let filtered: Vec<String> = ids
                            .into_iter()
                            .filter(|skill_id| !skill_id.starts_with(&prefix))
                            .collect();
                        agent.selected_skills = crate::config::workspace_config::skill_ids_to_refs(
                            &app_config,
                            &filtered,
                        );
                        agent.updated_at = now;
                        changed = true;
                    }
                }
                if changed {
                    config.updated_at = now;
                }
                Ok(changed)
            })?;
        if changed {
            state
                .workspace_index
                .write()
                .map_err(|e| format!("Workspace index lock error: {}", e))?
                .insert_config(locator.root_path, &config);
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AddSkillSourceRequest {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct SetSkillSourceEnabledRequest {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct SkillSourceResponse {
    #[serde(flatten)]
    #[ts(flatten)]
    pub source: SkillSourceConfig,
    pub managed_kind: Option<String>,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct SkillCatalogResponse {
    pub sources: Vec<SkillSourceResponse>,
    pub skills: Vec<SkillDefinition>,
    pub diagnostics: Vec<SkillSourceDiagnostic>,
}

impl From<SkillSourceConfig> for SkillSourceResponse {
    fn from(source: SkillSourceConfig) -> Self {
        let managed_kind = if bundled::is_bundled_source(&source) {
            Some("bundled".to_string())
        } else {
            None
        };
        Self {
            source,
            managed_kind,
            read_only: true,
        }
    }
}

#[tauri::command]
pub fn skill_sources_list(state: State<'_, AppState>) -> Result<Vec<SkillSourceResponse>, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;

    Ok(config_manager
        .get_skill_sources()
        .into_iter()
        .map(SkillSourceResponse::from)
        .collect())
}

#[tauri::command]
pub fn skills_list(state: State<'_, AppState>) -> Result<Vec<SkillDefinition>, String> {
    let config = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager.get()
    };

    discover_skills(&config)
}

#[tauri::command]
pub fn skills_catalog(state: State<'_, AppState>) -> Result<SkillCatalogResponse, String> {
    let config = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager.get()
    };

    let (skills, diagnostics) = discover_skills_with_diagnostics(&config);
    Ok(SkillCatalogResponse {
        sources: config
            .skill_sources
            .clone()
            .into_iter()
            .map(SkillSourceResponse::from)
            .collect(),
        skills,
        diagnostics,
    })
}

#[tauri::command]
pub async fn skill_source_add(
    request: AddSkillSourceRequest,
    state: State<'_, AppState>,
) -> Result<SkillSourceConfig, String> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err("Skill source name is required.".to_string());
    }

    let kind = request
        .kind
        .as_deref()
        .unwrap_or("local")
        .trim()
        .to_ascii_lowercase();
    let mut source = match kind.as_str() {
        "local" => {
            let path = request.path.as_deref().unwrap_or("").trim();
            if path.is_empty() {
                return Err("Skill source path is required.".to_string());
            }
            SkillSourceConfig::new_local(name.to_string(), path.to_string())
        }
        "git" => {
            let uri = request.uri.as_deref().unwrap_or("").trim();
            if uri.is_empty() {
                return Err("Git skill source URI is required.".to_string());
            }
            let reference = request
                .reference
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let mut source =
                SkillSourceConfig::new_git(name.to_string(), uri.to_string(), reference, None);
            let local_path = skill_source_cache_root()?.join(&source.id);
            if let SkillSourceKind::Git {
                local_path: slot, ..
            } = &mut source.source
            {
                *slot = Some(local_path.display().to_string());
            }
            sync_git_skill_source_blocking(source, true).await?
        }
        other => {
            return Err(format!(
                "Unsupported skill source kind '{}'. Expected 'local' or 'git'.",
                other
            ));
        }
    };
    source.updated_at = chrono::Utc::now().to_rfc3339();

    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    config_manager
        .add_skill_source(source.clone())
        .map_err(|e| format!("Failed to add skill source: {}", e))?;

    Ok(source)
}

#[tauri::command]
pub async fn skill_source_refresh(
    id: String,
    state: State<'_, AppState>,
) -> Result<SkillSourceConfig, String> {
    let mut source = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager
            .get_skill_sources()
            .into_iter()
            .find(|source| source.id == id)
            .ok_or_else(|| format!("Skill source not found: {}", id))?
    };

    if matches!(source.source, SkillSourceKind::Git { .. }) {
        source = sync_git_skill_source_blocking(source, false).await?;
    }
    source.updated_at = chrono::Utc::now().to_rfc3339();

    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    config_manager
        .update(|config| {
            if let Some(existing) = config
                .skill_sources
                .iter_mut()
                .find(|existing| existing.id == source.id)
            {
                *existing = source.clone();
            }
        })
        .map_err(|e| format!("Failed to update skill source: {}", e))?;

    Ok(source)
}

#[tauri::command]
pub fn skill_source_set_enabled(
    request: SetSkillSourceEnabledRequest,
    state: State<'_, AppState>,
) -> Result<SkillSourceConfig, String> {
    let mut updated = None;
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    config_manager
        .update(|config| {
            if let Some(source) = config
                .skill_sources
                .iter_mut()
                .find(|source| source.id == request.id)
            {
                source.enabled = request.enabled;
                source.updated_at = chrono::Utc::now().to_rfc3339();
                updated = Some(source.clone());
            }
        })
        .map_err(|e| format!("Failed to update skill source: {}", e))?;

    updated.ok_or_else(|| format!("Skill source not found: {}", request.id))
}

#[tauri::command]
pub async fn skill_source_delete(id: String, state: State<'_, AppState>) -> Result<(), String> {
    let source = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager
            .get_skill_sources()
            .into_iter()
            .find(|source| source.id == id)
    };

    if let Some(source) = &source {
        if bundled::is_bundled_source(source) {
            return Err(
                "The default CLAI skills source is app-managed and cannot be deleted.".to_string(),
            );
        }
    }

    // Sweep while the source is still present in AppConfig so stable skill refs
    // can be resolved back to the frontend `sourceId:slug` form.
    sweep_workspace_agent_skill_ids(state.inner(), &id)?;

    let removed = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager
            .remove_skill_source(&id)
            .map_err(|e| format!("Failed to delete skill source: {}", e))?
    };

    if removed {
        if let Some(source) = source {
            let _ = remove_git_cache_if_owned(&source);
        }
        Ok(())
    } else {
        Err(format!("Skill source not found: {}", id))
    }
}

fn skill_source_cache_root() -> Result<PathBuf, String> {
    Ok(crate::paths::clai_cache_skill_sources_root())
}

pub async fn sync_default_skill_source(state: &AppState) -> Result<(), String> {
    let source = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager
            .get_skill_sources()
            .into_iter()
            .find(bundled::is_bundled_source)
    };

    let Some(mut source) = source else {
        return Ok(());
    };
    if !source.enabled {
        return Ok(());
    }
    if !matches!(source.source, SkillSourceKind::Git { .. }) {
        return Ok(());
    }

    source = sync_git_skill_source_blocking(source, false).await?;
    source.updated_at = chrono::Utc::now().to_rfc3339();

    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    config_manager
        .update(|config| {
            if let Some(existing) = config
                .skill_sources
                .iter_mut()
                .find(|existing| existing.id == source.id)
            {
                *existing = source.clone();
            }
        })
        .map_err(|e| format!("Failed to update default skill source: {}", e))
}

async fn sync_git_skill_source_blocking(
    mut source: SkillSourceConfig,
    cleanup_on_error: bool,
) -> Result<SkillSourceConfig, String> {
    let source_for_cleanup = source.clone();
    tauri::async_runtime::spawn_blocking(move || {
        sync_git_skill_source(&mut source)?;
        Ok(source)
    })
    .await
    .map_err(|error| format!("Git skill source task failed: {}", error))?
    .inspect_err(|_| {
        if cleanup_on_error {
            let _ = remove_git_cache_if_owned(&source_for_cleanup);
        }
    })
}

fn sync_git_skill_source(source: &mut SkillSourceConfig) -> Result<(), String> {
    let SkillSourceKind::Git {
        uri,
        reference,
        local_path,
    } = &mut source.source
    else {
        return Ok(());
    };

    let root = skill_source_cache_root()?;
    let path = local_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(&source.id));
    if local_path.is_none() {
        *local_path = Some(path.display().to_string());
    }

    if !path.starts_with(&root) {
        return Err("Git skill source cache path is outside the CLAI data directory.".to_string());
    }

    if path.join(".git").exists() {
        run_git(Some(&path), ["fetch", "--all", "--tags", "--prune"])?;
        if let Some(reference) = reference
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            run_git(Some(&path), ["checkout", "--force", reference])?;
            let _ = run_git(Some(&path), ["pull", "--ff-only"]);
        } else {
            run_git(Some(&path), ["pull", "--ff-only"])?;
        }
        return Ok(());
    }

    if path.exists()
        && fs::read_dir(&path)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(true)
    {
        return Err(format!(
            "Git skill source cache path already exists and is not empty: {}",
            path.display()
        ));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create skill source cache: {}", error))?;
    }

    run_git(
        None,
        [OsStr::new("clone"), OsStr::new(uri), path.as_os_str()],
    )?;
    if let Some(reference) = reference
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        run_git(Some(&path), ["checkout", "--force", reference])?;
    }
    Ok(())
}

/// Builds the `git` command, wrapping it with `flatpak-spawn --host` when
/// running inside a Flatpak sandbox. The sandbox image ships no `git` on its
/// PATH, so a bare `Command::new("git")` fails with `os error 2` (No such
/// file or directory) — the symptom users hit when adding a git skill source.
/// On the host `git` is available, so we hop out via `flatpak-spawn --host`,
/// mirroring how editor/terminal launches are spawned (see
/// `system_apps::spawn_host_detached`).
///
/// The working directory needs care across the Flatpak hop. Outside Flatpak we
/// set `current_dir` directly on `git`. Inside Flatpak we must pass it as
/// `flatpak-spawn --directory=<dir>`: setting `current_dir` on the
/// `flatpak-spawn` wrapper only moves the wrapper itself, not the host `git` it
/// spawns, so `git fetch`/`checkout`/`pull` would otherwise run in the wrong
/// directory and fail to find `.git/`. The skill-source cache lives under the
/// user's real home (`~/.clai/cache/skill-sources/...`), which Flatpak maps at
/// the same path inside the sandbox (`--filesystem=home`), so the directory
/// resolves to the same location on both sides of the hop.
fn build_git_command(in_flatpak: bool, current_dir: Option<&Path>) -> Command {
    if in_flatpak {
        let mut command = Command::new("flatpak-spawn");
        if let Some(current_dir) = current_dir {
            command.arg(format!("--directory={}", current_dir.display()));
        }
        command.arg("--host").arg("git");
        command
    } else {
        let mut command = Command::new("git");
        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }
        command
    }
}

fn run_git<I, S>(current_dir: Option<&Path>, args: I) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = build_git_command(crate::providers::is_flatpak(), current_dir);
    command.args(args);

    let output = command
        .output()
        .map_err(|error| format!("Failed to execute git: {}", error))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("git exited with status {}", output.status)
    };
    Err(message)
}

fn remove_git_cache_if_owned(source: &SkillSourceConfig) -> Result<(), String> {
    let SkillSourceKind::Git {
        local_path: Some(local_path),
        ..
    } = &source.source
    else {
        return Ok(());
    };

    let root = skill_source_cache_root()?;
    let path = PathBuf::from(local_path);
    if path.starts_with(&root) && path.exists() {
        fs::remove_dir_all(&path)
            .map_err(|error| format!("Failed to remove skill source cache: {}", error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_git_command_native_invokes_git_directly() {
        let command = build_git_command(false, None);
        assert_eq!(command.get_program(), OsStr::new("git"));
        assert_eq!(command.get_args().count(), 0);
        assert_eq!(command.get_current_dir(), None);
    }

    #[test]
    fn build_git_command_native_applies_cwd_directly() {
        let dir = Path::new("/tmp/clai-skill-cache");
        let command = build_git_command(false, Some(dir));
        assert_eq!(command.get_program(), OsStr::new("git"));
        assert_eq!(command.get_current_dir(), Some(dir));
        // Native path must NOT add a flatpak-spawn --directory flag.
        assert_eq!(command.get_args().count(), 0);
    }

    #[test]
    fn build_git_command_in_flatpak_wraps_with_flatpak_spawn() {
        let command = build_git_command(true, None);
        assert_eq!(command.get_program(), OsStr::new("flatpak-spawn"));
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, vec![OsStr::new("--host"), OsStr::new("git")]);
        // No cwd requested -> no --directory and no wrapper cwd.
        assert_eq!(command.get_current_dir(), None);
    }

    #[test]
    fn build_git_command_in_flatpak_forwards_cwd_via_directory_flag() {
        let dir = Path::new("/home/user/.clai/cache/skill-sources/abc");
        let command = build_git_command(true, Some(dir));
        assert_eq!(command.get_program(), OsStr::new("flatpak-spawn"));
        let args: Vec<&OsStr> = command.get_args().collect();
        // cwd is passed with --directory=, BEFORE --host git, so the host git
        // (not the flatpak-spawn wrapper) runs in the repo directory.
        assert_eq!(
            args,
            vec![
                OsStr::new("--directory=/home/user/.clai/cache/skill-sources/abc"),
                OsStr::new("--host"),
                OsStr::new("git"),
            ]
        );
        // The wrapper's own cwd must NOT be set — that would only move
        // flatpak-spawn, not the host git.
        assert_eq!(command.get_current_dir(), None);
    }
}

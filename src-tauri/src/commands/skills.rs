//! Skill catalog commands.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::config::{
    discover_skills, discover_skills_with_diagnostics, SkillDefinition, SkillSourceConfig,
    SkillSourceDiagnostic, SkillSourceKind, APP_IDENTIFIER,
};
use crate::AppState;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSkillSourceEnabledRequest {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogResponse {
    pub sources: Vec<SkillSourceConfig>,
    pub skills: Vec<SkillDefinition>,
    pub diagnostics: Vec<SkillSourceDiagnostic>,
}

#[tauri::command]
pub fn skill_sources_list(state: State<'_, AppState>) -> Result<Vec<SkillSourceConfig>, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;

    Ok(config_manager.get_skill_sources())
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
        sources: config.skill_sources.clone(),
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
pub fn skill_source_delete(id: String, state: State<'_, AppState>) -> Result<(), String> {
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
    let data_dir = dirs::data_dir()
        .ok_or_else(|| "Could not determine application data directory.".to_string())?;
    Ok(data_dir.join(APP_IDENTIFIER).join("skill-sources"))
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

fn run_git<I, S>(current_dir: Option<&Path>, args: I) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
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

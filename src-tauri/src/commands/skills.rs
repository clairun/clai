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
        let mut config =
            crate::config::workspace_config::load(&locator.root_path).map_err(|e| e.to_string())?;
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
                agent.selected_skills =
                    crate::config::workspace_config::skill_ids_to_refs(&app_config, &filtered);
                agent.updated_at = now;
                changed = true;
            }
        }
        if changed {
            config.updated_at = now;
            crate::config::workspace_config::save(&locator.root_path, &config)
                .map_err(|e| e.to_string())?;
            state
                .workspace_index
                .write()
                .map_err(|e| format!("Workspace index lock error: {}", e))?
                .insert_config(locator.root_path, &config);
        }
    }

    Ok(())
}

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
pub struct SkillSourceResponse {
    #[serde(flatten)]
    pub source: SkillSourceConfig,
    pub managed_kind: Option<String>,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogResponse {
    pub sources: Vec<SkillSourceResponse>,
    pub skills: Vec<SkillDefinition>,
    pub diagnostics: Vec<SkillSourceDiagnostic>,
}

impl From<SkillSourceConfig> for SkillSourceResponse {
    fn from(source: SkillSourceConfig) -> Self {
        let managed_kind = if bundled::is_bundled_source(&source) {
            Some("bundled".to_string())
        } else if bundled::is_personal_source(&source) {
            Some("personal".to_string())
        } else {
            None
        };
        let read_only = managed_kind.as_deref() == Some("bundled");
        Self {
            source,
            managed_kind,
            read_only,
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
pub fn skill_fork_bundled(
    source_skill_id: String,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<SkillDefinition, String> {
    let trimmed_name = new_name.trim();
    if trimmed_name.is_empty() {
        return Err("New skill name is required.".to_string());
    }
    let slug = slugify_skill_name(trimmed_name);
    if slug.is_empty() {
        return Err("New skill name must contain at least one ASCII letter or number.".to_string());
    }

    let (source_skill, source_config) = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let config = config_manager.get();
        let skills = discover_skills(&config)?;
        let skill = skills
            .into_iter()
            .find(|skill| skill.id == source_skill_id)
            .ok_or_else(|| format!("Skill not found: {}", source_skill_id))?;
        let source = config
            .skill_sources
            .iter()
            .find(|source| source.id == skill.source_id)
            .cloned()
            .ok_or_else(|| format!("Skill source not found: {}", skill.source_id))?;
        (skill, source)
    };

    if !bundled::is_bundled_source(&source_config) {
        return Err("Only bundled skills can be forked to the personal source.".to_string());
    }

    let target_dir = bundled::personal_skills_root().join(&slug);
    if target_dir.exists() {
        return Err(format!(
            "A personal skill with slug '{}' already exists.",
            slug
        ));
    }

    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let mut ensure_result: Result<bool, String> = Ok(false);
        config_manager
            .update(|config| {
                ensure_result = bundled::ensure_personal_skill_source_lazy(config);
            })
            .map_err(|e| format!("Failed to update skill sources: {}", e))?;
        ensure_result?;
    }

    fs::create_dir_all(&target_dir)
        .map_err(|error| format!("Failed to create personal skill directory: {}", error))?;
    let forked_at = chrono::Utc::now().to_rfc3339();
    let body = skill_body_without_frontmatter(&source_skill.content);
    let forked_content = build_forked_skill_content(
        trimmed_name,
        &source_skill.description,
        &source_skill.id,
        &forked_at,
        body,
    );
    fs::write(target_dir.join("SKILL.md"), forked_content)
        .map_err(|error| format!("Failed to write forked skill: {}", error))?;

    let config = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager.get()
    };
    let personal_source = config
        .skill_sources
        .iter()
        .find(|source| bundled::is_personal_source(source))
        .ok_or_else(|| "Personal skill source was not registered.".to_string())?;
    let expected_id = format!("{}:{}", personal_source.id, slug);
    discover_skills(&config)?
        .into_iter()
        .find(|skill| skill.id == expected_id)
        .ok_or_else(|| format!("Forked skill was not discovered: {}", expected_id))
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
            return Err("Bundled skill sources are app-managed and cannot be deleted.".to_string());
        }
        if bundled::is_personal_source(source) {
            return Err("Personal skill source is app-managed and cannot be deleted.".to_string());
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

fn slugify_skill_name(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    slug
}

fn skill_body_without_frontmatter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("---") else {
        return content;
    };
    let Some(rest) = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
    else {
        return content;
    };

    let mut offset = content.len() - rest.len();
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim();
        offset += line.len();
        if trimmed == "---" {
            return content[offset..].trim_start_matches(['\n', '\r']);
        }
    }

    content
}

fn build_forked_skill_content(
    name: &str,
    description: &str,
    forked_from: &str,
    forked_at: &str,
    body: &str,
) -> String {
    let yaml_name = serde_json::to_string(name).unwrap_or_else(|_| "\"Forked Skill\"".to_string());
    let yaml_description =
        serde_json::to_string(description).unwrap_or_else(|_| "\"\"".to_string());
    let yaml_forked_from =
        serde_json::to_string(forked_from).unwrap_or_else(|_| "\"\"".to_string());
    let yaml_forked_at = serde_json::to_string(forked_at).unwrap_or_else(|_| "\"\"".to_string());

    format!(
        "---\nname: {}\ndescription: {}\nforked_from: {}\nforked_at: {}\n---\n{}",
        yaml_name, yaml_description, yaml_forked_from, yaml_forked_at, body
    )
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

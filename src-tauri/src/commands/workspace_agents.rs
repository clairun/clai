//! Workspace-scoped agent CRUD commands.
//!
//! Agents live inside `<workspace>/.clai/config.json`. The command payloads
//! intentionally preserve the previous SQLite-backed wire shape.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::config::{
    workspace_config, AppConfig, ExecutionCapabilityConfig, WorkspaceAgent, WorkspaceConfig,
};
use crate::AppState;

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgentCreateRequest {
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub selected_skill_ids: Vec<String>,
    #[serde(default)]
    pub selected_mcp_server_ids: Vec<String>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional explicit id; if absent, a fresh UUID is generated.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgentUpdateRequest {
    pub workspace_id: String,
    pub agent_id: String,
    pub name: String,
    pub description: String,
    pub selected_skill_ids: Vec<String>,
    pub selected_mcp_server_ids: Vec<String>,
    pub provider_connection_ids: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgentEnabledRequest {
    pub workspace_id: String,
    pub agent_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgentDetail {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub description: String,
    pub selected_skill_ids: Vec<String>,
    pub selected_mcp_server_ids: Vec<String>,
    pub provider_connection_ids: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

fn default_true() -> bool {
    true
}

/// Returns the default execution-capability shape that a brand-new agent
/// ships with (host `$HOME` read-only by default). The UI calls this when
/// opening the "Add agent" form so the user can see the granted defaults
/// — and, importantly, remove them before saving — instead of having the
/// backend silently inject them on create.
#[tauri::command]
pub async fn workspace_agent_default_execution() -> Result<ExecutionCapabilityConfig, String> {
    Ok(workspace_config::default_agent_execution())
}

#[tauri::command]
pub async fn workspace_get_agent(
    workspace_id: String,
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<Option<WorkspaceAgentDetail>, String> {
    let (_root, config) = load_workspace_config(state.inner(), &workspace_id)?;
    let app_config = app_config(state.inner())?;
    Ok(config
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .map(|agent| detail_from_agent(&app_config, &config, agent)))
}

#[tauri::command]
pub async fn workspace_create_agent(
    request: WorkspaceAgentCreateRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceAgentDetail, String> {
    let app_config = app_config(state.inner())?;
    let (root, mut config) = load_workspace_config(state.inner(), &request.workspace_id)?;
    let id = request
        .id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if config.agents.iter().any(|agent| agent.id == id) {
        return Err(format!("Workspace agent already exists: {}", id));
    }

    // The host `$HOME` RO default is pre-populated by the UI via
    // `workspace_agent_default_execution` so the user can see and remove it
    // before saving. Trust the request's execution verbatim — if the user
    // cleared all path grants on purpose, we honor that.
    let execution = request.execution;

    let now = now_millis();
    let agent = WorkspaceAgent {
        id: id.clone(),
        name: request.name,
        description: request.description,
        enabled: request.enabled,
        selected_skills: workspace_config::skill_ids_to_refs(
            &app_config,
            &request.selected_skill_ids,
        ),
        selected_mcp_servers: workspace_config::mcp_ids_to_refs(
            &app_config,
            &request.selected_mcp_server_ids,
        ),
        provider_connection_ids: request.provider_connection_ids,
        execution,
        created_at: now,
        updated_at: now,
    };
    config.updated_at = now;
    config.agents.push(agent);
    save_workspace_config(state.inner(), &root, &config)?;

    let saved = config
        .agents
        .iter()
        .find(|agent| agent.id == id)
        .ok_or_else(|| "Workspace agent disappeared between write and read-back".to_string())?;
    Ok(detail_from_agent(&app_config, &config, saved))
}

#[tauri::command]
pub async fn workspace_update_agent(
    request: WorkspaceAgentUpdateRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceAgentDetail, String> {
    let app_config = app_config(state.inner())?;
    let (root, mut config) = load_workspace_config(state.inner(), &request.workspace_id)?;
    let now = now_millis();
    let Some(agent) = config
        .agents
        .iter_mut()
        .find(|agent| agent.id == request.agent_id)
    else {
        return Err(format!("Workspace agent not found: {}", request.agent_id));
    };

    agent.name = request.name;
    agent.description = request.description;
    agent.selected_skills =
        workspace_config::skill_ids_to_refs(&app_config, &request.selected_skill_ids);
    agent.selected_mcp_servers =
        workspace_config::mcp_ids_to_refs(&app_config, &request.selected_mcp_server_ids);
    agent.provider_connection_ids = request.provider_connection_ids;
    agent.execution = request.execution;
    agent.enabled = request.enabled;
    agent.updated_at = now;
    config.updated_at = now;
    save_workspace_config(state.inner(), &root, &config)?;

    let saved = config
        .agents
        .iter()
        .find(|agent| agent.id == request.agent_id)
        .ok_or_else(|| {
            format!(
                "Workspace agent not found after update: {}",
                request.agent_id
            )
        })?;
    Ok(detail_from_agent(&app_config, &config, saved))
}

#[tauri::command]
pub async fn workspace_delete_agent(
    workspace_id: String,
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (root, mut config) = load_workspace_config(state.inner(), &workspace_id)?;
    if config.default_agent_id == agent_id {
        return Err(
            "Cannot delete the workspace's manager agent. Designate a different manager first."
                .to_string(),
        );
    }

    let before = config.agents.len();
    config.agents.retain(|agent| agent.id != agent_id);
    if config.agents.len() == before {
        return Err(format!("Workspace agent not found: {}", agent_id));
    }

    config.updated_at = now_millis();
    save_workspace_config(state.inner(), &root, &config)?;
    Ok(())
}

#[tauri::command]
pub async fn workspace_set_agent_enabled(
    request: WorkspaceAgentEnabledRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceAgentDetail, String> {
    let app_config = app_config(state.inner())?;
    let (root, mut config) = load_workspace_config(state.inner(), &request.workspace_id)?;
    let now = now_millis();
    let Some(agent) = config
        .agents
        .iter_mut()
        .find(|agent| agent.id == request.agent_id)
    else {
        return Err(format!("Workspace agent not found: {}", request.agent_id));
    };
    agent.enabled = request.enabled;
    agent.updated_at = now;
    config.updated_at = now;
    save_workspace_config(state.inner(), &root, &config)?;

    let saved = config
        .agents
        .iter()
        .find(|agent| agent.id == request.agent_id)
        .ok_or_else(|| {
            format!(
                "Workspace agent not found after toggle: {}",
                request.agent_id
            )
        })?;
    Ok(detail_from_agent(&app_config, &config, saved))
}

fn load_workspace_config(
    state: &AppState,
    workspace_id: &str,
) -> Result<(PathBuf, WorkspaceConfig), String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    Ok((root, config))
}

fn save_workspace_config(
    state: &AppState,
    root: &std::path::Path,
    config: &WorkspaceConfig,
) -> Result<(), String> {
    workspace_config::save(root, config).map_err(|e| e.to_string())?;
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .insert_config(root.to_path_buf(), config);
    Ok(())
}

fn app_config(state: &AppState) -> Result<AppConfig, String> {
    Ok(state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get())
}

pub(crate) fn detail_from_agent(
    app_config: &AppConfig,
    workspace: &WorkspaceConfig,
    agent: &WorkspaceAgent,
) -> WorkspaceAgentDetail {
    WorkspaceAgentDetail {
        id: agent.id.clone(),
        workspace_id: workspace.id.clone(),
        name: agent.name.clone(),
        description: agent.description.clone(),
        selected_skill_ids: workspace_config::refs_to_skill_ids(app_config, &agent.selected_skills),
        selected_mcp_server_ids: workspace_config::refs_to_mcp_ids(
            app_config,
            &agent.selected_mcp_servers,
        ),
        provider_connection_ids: agent.provider_connection_ids.clone(),
        execution: agent.execution.clone(),
        enabled: agent.enabled,
        is_default: workspace.default_agent_id == agent.id,
        created_at: agent.created_at,
        updated_at: agent.updated_at,
    }
}

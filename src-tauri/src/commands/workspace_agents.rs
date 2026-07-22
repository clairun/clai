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
    let id = request
        .id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

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
        selected_mcp_servers: workspace_config::mcp_ids_to_refs(&request.selected_mcp_server_ids),
        provider_connection_ids: request.provider_connection_ids,
        execution,
        created_at: now,
        updated_at: now,
    };
    let ((), config) = update_workspace_config(state.inner(), &request.workspace_id, |config| {
        if config.agents.iter().any(|agent| agent.id == id) {
            return Err(format!("Workspace agent already exists: {}", id));
        }
        config.updated_at = now;
        config.agents.push(agent);
        Ok(())
    })?;

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
    let now = now_millis();
    let agent_id = request.agent_id.clone();
    let workspace_id = request.workspace_id.clone();
    let ((), config) = update_workspace_config(state.inner(), &workspace_id, |config| {
        let is_default_agent = config.default_agent_id == request.agent_id;
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
            workspace_config::mcp_ids_to_refs(&request.selected_mcp_server_ids);
        let selected_ids: Vec<String> = agent
            .selected_mcp_servers
            .iter()
            .map(|mcp_ref| mcp_ref.id.clone())
            .collect();
        agent.provider_connection_ids = request.provider_connection_ids;
        agent.execution = request.execution;
        agent.enabled = request.enabled;
        agent.updated_at = now;
        if is_default_agent {
            // Re-selecting a server for the workspace manager re-enables it:
            // drop it from the workspace-level disabled list so the config
            // keeps the enabled/disabled lists disjoint.
            config
                .disabled_mcp_servers
                .retain(|mcp_ref| !selected_ids.contains(&mcp_ref.id));
        }
        config.updated_at = now;
        Ok(())
    })?;

    let saved = config
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .ok_or_else(|| format!("Workspace agent not found after update: {}", agent_id))?;
    Ok(detail_from_agent(&app_config, &config, saved))
}

#[tauri::command]
pub async fn workspace_delete_agent(
    workspace_id: String,
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    update_workspace_config(state.inner(), &workspace_id, |config| {
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
        Ok(())
    })?;
    Ok(())
}

#[tauri::command]
pub async fn workspace_set_agent_enabled(
    request: WorkspaceAgentEnabledRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceAgentDetail, String> {
    let app_config = app_config(state.inner())?;
    let now = now_millis();
    let ((), config) = update_workspace_config(state.inner(), &request.workspace_id, |config| {
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
        Ok(())
    })?;

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

/// Atomic read-modify-write + index refresh — all config writers must use
/// this instead of a bare load→save pair, which races the runner's
/// run-completion persist (see `workspace_config::update`).
fn update_workspace_config<R>(
    state: &AppState,
    workspace_id: &str,
    mutate: impl FnOnce(&mut WorkspaceConfig) -> Result<R, String>,
) -> Result<(R, WorkspaceConfig), String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let (value, config) = workspace_config::update(&root, mutate)?;
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .insert_config(root, &config);
    Ok((value, config))
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
        selected_mcp_server_ids: workspace_config::refs_to_mcp_ids(&agent.selected_mcp_servers),
        provider_connection_ids: agent.provider_connection_ids.clone(),
        execution: agent.execution.clone(),
        enabled: agent.enabled,
        is_default: workspace.default_agent_id == agent.id,
        created_at: agent.created_at,
        updated_at: agent.updated_at,
    }
}

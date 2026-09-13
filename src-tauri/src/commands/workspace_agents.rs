//! Workspace-scoped agent commands.
//!
//! A workspace owns exactly one agent outright: its Main, stored in
//! `<workspace>/.clai/config.json` and editable here. Teammates are app-level
//! definitions ([`crate::commands::global_agents`]) made callable by an
//! assignment; the only parts of them a workspace may change are the local
//! overlays, which is why the update path below refuses an assignment id
//! instead of writing a copy of shared behavior into the workspace file.

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
    let (config, roster) = state.resolve_workspace_roster(&workspace_id)?;
    let app_config = app_config(state.inner())?;
    Ok(roster
        .iter()
        .find(|resolved| resolved.agent.id == agent_id)
        .map(|resolved| detail_from_agent(&app_config, &config, &resolved.agent)))
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
    let ((), config) = state.update_workspace_config(&request.workspace_id, |config| {
        // Creating an agent means setting up this workspace's Main. Teammates
        // are assigned from the library, never created here, so a workspace
        // that already has a Main has nothing left to create.
        if let Some(existing) = config.main_agent.as_ref() {
            return Err(format!(
                "This workspace already has a Main agent ({}). Assign teammates from the agent library instead.",
                existing.id
            ));
        }
        config.updated_at = now;
        config.main_agent = Some(agent);
        Ok(())
    })?;

    let saved = config
        .main_agent
        .as_ref()
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
    let ((), config) = state.update_workspace_config(&workspace_id, |config| {
        if config.assignment(&request.agent_id).is_some() {
            return Err(
                "This teammate's behavior is shared. Edit it in the agent library, or change its local context and access in the workspace team settings."
                    .to_string(),
            );
        }
        let Some(agent) = config
            .main_agent
            .as_mut()
            .filter(|agent| agent.id == request.agent_id)
        else {
            return Err(format!("Workspace agent not found: {}", request.agent_id));
        };

        agent.name = request.name;
        agent.description = request.description;
        agent.selected_skills =
            workspace_config::skill_ids_to_refs(&app_config, &request.selected_skill_ids);
        // Settings edits attachment only; the context-bar `disabled` flag is
        // preserved for servers that stay attached (merge_mcp_selection).
        agent.selected_mcp_servers = workspace_config::merge_mcp_selection(
            &agent.selected_mcp_servers,
            &request.selected_mcp_server_ids,
        );
        agent.provider_connection_ids = request.provider_connection_ids;
        agent.execution = request.execution;
        agent.enabled = request.enabled;
        agent.updated_at = now;
        config.updated_at = now;
        Ok(())
    })?;

    let saved = config
        .main_agent
        .as_ref()
        .filter(|agent| agent.id == agent_id)
        .ok_or_else(|| format!("Workspace agent not found after update: {}", agent_id))?;
    Ok(detail_from_agent(&app_config, &config, saved))
}

#[tauri::command]
pub async fn workspace_delete_agent(
    workspace_id: String,
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.update_workspace_config(&workspace_id, |config| {
        if config.main_agent_id() == agent_id {
            return Err(
                "Cannot delete the workspace's Main agent. Every workspace has exactly one."
                    .to_string(),
            );
        }

        // Unassigning drops the local identity and its local overlays. The
        // shared definition — and every other workspace using it — is
        // untouched.
        let before = config.assignments.len();
        config
            .assignments
            .retain(|assignment| assignment.id != agent_id);
        if config.assignments.len() == before {
            return Err(format!(
                "Workspace agent assignment not found: {}",
                agent_id
            ));
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
    let workspace_id = request.workspace_id.clone();
    let agent_id = request.agent_id.clone();
    state.update_workspace_config(&workspace_id, |config| {
        // Enabling is local for both kinds: an assignment carries its own
        // switch precisely so one workspace can park a teammate without
        // disabling it everywhere else.
        if let Some(assignment) = config
            .assignments
            .iter_mut()
            .find(|assignment| assignment.id == agent_id)
        {
            assignment.enabled = request.enabled;
            assignment.updated_at = now;
            config.updated_at = now;
            return Ok(());
        }
        let Some(agent) = config
            .main_agent
            .as_mut()
            .filter(|agent| agent.id == agent_id)
        else {
            return Err(format!("Workspace agent not found: {}", agent_id));
        };
        agent.enabled = request.enabled;
        agent.updated_at = now;
        config.updated_at = now;
        Ok(())
    })?;

    let (config, roster) = state.resolve_workspace_roster(&workspace_id)?;
    let saved = roster
        .iter()
        .find(|resolved| resolved.agent.id == agent_id)
        .ok_or_else(|| format!("Workspace agent not found after toggle: {}", agent_id))?;
    Ok(detail_from_agent(&app_config, &config, &saved.agent))
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
        is_default: workspace.main_agent_id() == agent.id,
        created_at: agent.created_at,
        updated_at: agent.updated_at,
    }
}

//! Agent management Tauri commands.
//!
//! These commands handle CRUD operations for user-defined automations.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::assistant::repository as assistant_repository;
use crate::config::{AgentConfig, ExecutionCapabilityConfig, ExposedAgentTool};
use crate::db::DbPool;
use crate::AppState;

// =============================================================================
// Request/Response Types
// =============================================================================

/// Request to create a new agent.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAgentRequest {
    pub name: String,
    pub description: String,
    #[serde(default = "default_true")]
    pub schedule_enabled: bool,
    pub interval_minutes: u32,
    #[serde(default)]
    pub selected_mcp_server_ids: Vec<String>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,
    #[serde(default)]
    pub selected_skill_ids: Vec<String>,
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,
    #[serde(default)]
    pub exposed_tools: Vec<ExposedAgentTool>,
}

/// Request to update an existing agent.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAgentRequest {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default = "default_true")]
    pub schedule_enabled: bool,
    pub interval_minutes: u32,
    #[serde(default)]
    pub selected_mcp_server_ids: Vec<String>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,
    #[serde(default)]
    pub selected_skill_ids: Vec<String>,
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,
    #[serde(default)]
    pub exposed_tools: Vec<ExposedAgentTool>,
}

/// Request to enable/disable an agent globally.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAgentEnabledRequest {
    pub id: String,
    pub enabled: bool,
}

/// Response for agent list operations.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentResponse {
    pub id: String,
    pub name: String,
    pub description: String,
    pub schedule_enabled: bool,
    pub interval_minutes: u32,
    pub enabled: bool,
    pub selected_mcp_server_ids: Vec<String>,
    pub provider_connection_ids: Vec<String>,
    pub selected_skill_ids: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub exposed_tools: Vec<ExposedAgentTool>,
    pub created_at: String,
    pub updated_at: String,
    pub is_default: bool,
}

impl From<AgentConfig> for AgentResponse {
    fn from(agent: AgentConfig) -> Self {
        let is_default = agent.is_default();
        Self {
            id: agent.id,
            name: agent.name,
            description: agent.description,
            schedule_enabled: agent.schedule_enabled,
            interval_minutes: agent.interval_minutes,
            enabled: agent.enabled,
            selected_mcp_server_ids: agent.selected_mcp_server_ids,
            provider_connection_ids: agent.provider_connection_ids,
            selected_skill_ids: agent.selected_skill_ids,
            execution: agent.execution,
            exposed_tools: agent.exposed_tools,
            created_at: agent.created_at,
            updated_at: agent.updated_at,
            is_default,
        }
    }
}

// =============================================================================
// CRUD Commands
// =============================================================================

/// Gets all agents.
#[tauri::command]
pub fn get_agents(state: State<'_, AppState>) -> Result<Vec<AgentResponse>, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;

    let agents = config_manager
        .get_agents()
        .into_iter()
        .map(AgentResponse::from)
        .collect();

    Ok(agents)
}

/// Gets a single agent by ID.
#[tauri::command]
pub fn get_agent(id: String, state: State<'_, AppState>) -> Result<Option<AgentResponse>, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;

    let agent = config_manager.get_agent(&id).map(AgentResponse::from);

    Ok(agent)
}

/// Creates a new agent.
///
/// Returns the created agent with its generated UUID.
#[tauri::command]
pub async fn create_agent(
    request: CreateAgentRequest,
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<AgentResponse, String> {
    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        validate_mcp_server_ids(
            &config_manager,
            &request.selected_mcp_server_ids,
            "create agent",
        )?;
    }
    validate_provider_connection_ids(pool.inner(), &request.provider_connection_ids).await?;

    let mut agent = AgentConfig::new(request.name, request.description, request.interval_minutes);
    agent.schedule_enabled = request.schedule_enabled;
    agent.selected_mcp_server_ids = request.selected_mcp_server_ids;
    agent.provider_connection_ids = request.provider_connection_ids;
    agent.selected_skill_ids = request.selected_skill_ids;
    agent.execution = request.execution;
    agent.exposed_tools = request.exposed_tools;
    agent.validate()?;

    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        config_manager
            .add_agent(agent.clone())
            .map_err(|e| format!("Failed to create agent: {}", e))?;
    }

    sync_agent_scheduler(&state, &agent).await;

    Ok(AgentResponse::from(agent))
}

/// Updates an existing agent.
#[tauri::command]
pub async fn update_agent(
    request: UpdateAgentRequest,
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<AgentResponse, String> {
    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        if config_manager.get_agent(&request.id).is_none() {
            return Err(format!("Agent not found: {}", request.id));
        }

        validate_mcp_server_ids(
            &config_manager,
            &request.selected_mcp_server_ids,
            "update agent",
        )?;
    }
    validate_provider_connection_ids(pool.inner(), &request.provider_connection_ids).await?;

    let agent = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        let mut candidate = config_manager
            .get_agent(&request.id)
            .ok_or_else(|| format!("Agent not found: {}", request.id))?;
        candidate.name = request.name.clone();
        candidate.description = request.description.clone();
        candidate.schedule_enabled = request.schedule_enabled;
        candidate.interval_minutes = request.interval_minutes;
        candidate.selected_mcp_server_ids = request.selected_mcp_server_ids.clone();
        candidate.provider_connection_ids = request.provider_connection_ids.clone();
        candidate.selected_skill_ids = request.selected_skill_ids.clone();
        candidate.execution = request.execution.clone();
        candidate.exposed_tools = request.exposed_tools.clone();
        candidate.validate()?;

        config_manager
            .update_agent(&request.id, |agent| {
                *agent = candidate.clone();
            })
            .map_err(|e| format!("Failed to update agent: {}", e))?;

        config_manager
            .get_agent(&request.id)
            .ok_or_else(|| "Agent not found after update".to_string())?
    };
    sync_agent_scheduler(&state, &agent).await;

    Ok(AgentResponse::from(agent))
}

#[tauri::command]
pub async fn set_agent_enabled(
    request: SetAgentEnabledRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<AgentResponse, String> {
    if request.enabled {
        let pool = app.try_state::<DbPool>().ok_or_else(|| {
            "Assistant database is not ready yet. Try again in a moment.".to_string()
        })?;
        let agent = {
            let config_manager = state
                .config_manager
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?;
            config_manager
                .get_agent(&request.id)
                .ok_or_else(|| format!("Agent not found: {}", request.id))?
        };
        validate_agent_provider_connections(pool.inner(), &agent).await?;
    }

    let agent = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        if config_manager.get_agent(&request.id).is_none() {
            return Err(format!("Agent not found: {}", request.id));
        }

        config_manager
            .set_agent_enabled(&request.id, request.enabled)
            .map_err(|e| format!("Failed to update agent enabled state: {}", e))?;

        config_manager
            .get_agent(&request.id)
            .ok_or_else(|| "Agent not found after update".to_string())?
    };

    sync_agent_scheduler(&state, &agent).await;

    Ok(AgentResponse::from(agent))
}

/// Deletes an agent.
///
/// Also removes any scheduler instances for this agent.
#[tauri::command]
pub async fn delete_agent(id: String, state: State<'_, AppState>) -> Result<(), String> {
    // Scope the config_manager lock to avoid holding it across await
    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        let removed = config_manager
            .remove_agent(&id)
            .map_err(|e| format!("Failed to delete agent: {}", e))?;

        if !removed {
            return Err(format!("Agent not found: {}", id));
        }
    }

    // Remove scheduler instances for this agent
    let mut scheduler = state.scheduler.lock().await;
    scheduler.remove_instances_for_agent(&id);

    Ok(())
}

async fn validate_agent_provider_connections(
    pool: &DbPool,
    agent: &AgentConfig,
) -> Result<(), String> {
    if agent.provider_connection_ids.is_empty() {
        return Err("Select at least one provider connection for this agent.".to_string());
    }

    let connections = assistant_repository::list_provider_connections(pool).await?;
    let has_enabled = agent.provider_connection_ids.iter().any(|id| {
        connections
            .iter()
            .any(|connection| connection.id == *id && connection.enabled)
    });

    if !has_enabled {
        return Err("This agent has no enabled provider connections.".to_string());
    }

    Ok(())
}

async fn validate_provider_connection_ids(
    pool: &DbPool,
    connection_ids: &[String],
) -> Result<(), String> {
    let connections = assistant_repository::list_provider_connections(pool).await?;
    for connection_id in connection_ids {
        if !connections
            .iter()
            .any(|connection| connection.id == *connection_id)
        {
            return Err(format!("Unknown provider connection: {}", connection_id));
        }
    }
    Ok(())
}

fn build_agent_definition(agent: &AgentConfig) -> crate::agents::AgentDefinition {
    crate::agents::AgentDefinition::new(
        &agent.id,
        &agent.name,
        (agent.interval_minutes as u64) * 60 * 1000,
    )
    .with_description(&agent.description)
    .with_tools(agent.required_tools())
}

async fn sync_agent_scheduler(state: &State<'_, AppState>, agent: &AgentConfig) {
    let mut scheduler = state.scheduler.lock().await;
    scheduler.remove_instances_for_agent(&agent.id);
    if agent.schedule_enabled {
        scheduler.register_definition(build_agent_definition(agent));
    }

    if !agent.enabled || !agent.schedule_enabled {
        return;
    }

    scheduler.create_instance(&agent.id, "", "");
}

fn default_true() -> bool {
    true
}

fn validate_mcp_server_ids(
    config_manager: &crate::config::ConfigManager,
    server_ids: &[String],
    action: &str,
) -> Result<(), String> {
    let missing: Vec<String> = server_ids
        .iter()
        .filter(|server_id| config_manager.get_mcp_server(server_id).is_none())
        .cloned()
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    Err(format!(
        "Cannot {}: unknown MCP server IDs: {}",
        action,
        missing.join(", ")
    ))
}

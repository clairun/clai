//! Workspace-scoped agent CRUD commands.
//!
//! Agents are now local to a workspace (`workspace_agents` table carries the
//! full configuration inline). These commands replace the legacy global
//! `commands::agents` CRUD that operated on `ClaiConfig.agents`.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::config::{AgentConfig, ExecutionCapabilityConfig, ExposedAgentTool};
use crate::db::DbPool;

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
    #[serde(default)]
    pub exposed_tools: Vec<ExposedAgentTool>,
    #[serde(default)]
    pub schedule_enabled: bool,
    #[serde(default)]
    pub interval_minutes: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional explicit id; if absent, a fresh UUID is generated.
    /// Used by the "fork from bundled template" UI path to seed the row.
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
    pub exposed_tools: Vec<ExposedAgentTool>,
    pub schedule_enabled: bool,
    pub interval_minutes: u32,
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
    pub exposed_tools: Vec<ExposedAgentTool>,
    pub schedule_enabled: bool,
    pub interval_minutes: u32,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

fn default_true() -> bool {
    true
}

#[tauri::command]
pub async fn workspace_get_agent(
    workspace_id: String,
    agent_id: String,
    pool: State<'_, DbPool>,
) -> Result<Option<WorkspaceAgentDetail>, String> {
    let detail = load_detail(pool.inner(), &agent_id).await?;
    if let Some(d) = &detail {
        if d.workspace_id != workspace_id {
            return Err("Workspace agent does not belong to this workspace.".to_string());
        }
    }
    Ok(detail)
}

#[tauri::command]
pub async fn workspace_create_agent(
    request: WorkspaceAgentCreateRequest,
    pool: State<'_, DbPool>,
) -> Result<WorkspaceAgentDetail, String> {
    validate_agent_fields(
        request.schedule_enabled,
        request.interval_minutes,
        &request.exposed_tools,
    )?;

    let id = request
        .id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let now = now_millis();
    let selected_skill_ids = serde_json::to_string(&request.selected_skill_ids)
        .map_err(|e| format!("Failed to encode selected_skill_ids: {}", e))?;
    let selected_mcp_server_ids = serde_json::to_string(&request.selected_mcp_server_ids)
        .map_err(|e| format!("Failed to encode selected_mcp_server_ids: {}", e))?;
    let provider_connection_ids = serde_json::to_string(&request.provider_connection_ids)
        .map_err(|e| format!("Failed to encode provider_connection_ids: {}", e))?;
    let execution = serde_json::to_string(&request.execution)
        .map_err(|e| format!("Failed to encode execution: {}", e))?;
    let exposed_tools = serde_json::to_string(&request.exposed_tools)
        .map_err(|e| format!("Failed to encode exposed_tools: {}", e))?;

    sqlx::query(
        r#"
        INSERT INTO workspace_agents (
            id, workspace_id, agent_definition_id, display_name, role, enabled,
            name, description, selected_skill_ids, selected_mcp_server_ids,
            provider_connection_ids, execution, exposed_tools,
            schedule_enabled, interval_minutes,
            created_at, updated_at
        )
        VALUES (?, ?, ?, NULL, 'member', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&id)
    .bind(&request.workspace_id)
    .bind(&id) // legacy agent_definition_id placeholder = self (column will be dropped in Phase 1.7)
    .bind(i64::from(request.enabled))
    .bind(&request.name)
    .bind(&request.description)
    .bind(&selected_skill_ids)
    .bind(&selected_mcp_server_ids)
    .bind(&provider_connection_ids)
    .bind(&execution)
    .bind(&exposed_tools)
    .bind(i64::from(request.schedule_enabled))
    .bind(i64::from(request.interval_minutes))
    .bind(now)
    .bind(now)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to create workspace agent: {}", e))?;

    load_detail(pool.inner(), &id)
        .await?
        .ok_or_else(|| "Workspace agent disappeared between INSERT and read-back".to_string())
}

#[tauri::command]
pub async fn workspace_update_agent(
    request: WorkspaceAgentUpdateRequest,
    pool: State<'_, DbPool>,
) -> Result<WorkspaceAgentDetail, String> {
    validate_agent_fields(
        request.schedule_enabled,
        request.interval_minutes,
        &request.exposed_tools,
    )?;

    let existing: Option<String> =
        sqlx::query_scalar("SELECT workspace_id FROM workspace_agents WHERE id = ? LIMIT 1")
            .bind(&request.agent_id)
            .fetch_optional(pool.inner())
            .await
            .map_err(|e| format!("Failed to look up workspace agent: {}", e))?;
    match existing {
        None => return Err(format!("Workspace agent not found: {}", request.agent_id)),
        Some(ws) if ws != request.workspace_id => {
            return Err("Workspace agent does not belong to this workspace.".to_string());
        }
        _ => {}
    }

    let selected_skill_ids = serde_json::to_string(&request.selected_skill_ids)
        .map_err(|e| format!("Failed to encode selected_skill_ids: {}", e))?;
    let selected_mcp_server_ids = serde_json::to_string(&request.selected_mcp_server_ids)
        .map_err(|e| format!("Failed to encode selected_mcp_server_ids: {}", e))?;
    let provider_connection_ids = serde_json::to_string(&request.provider_connection_ids)
        .map_err(|e| format!("Failed to encode provider_connection_ids: {}", e))?;
    let execution = serde_json::to_string(&request.execution)
        .map_err(|e| format!("Failed to encode execution: {}", e))?;
    let exposed_tools = serde_json::to_string(&request.exposed_tools)
        .map_err(|e| format!("Failed to encode exposed_tools: {}", e))?;

    sqlx::query(
        r#"
        UPDATE workspace_agents
        SET name = ?,
            description = ?,
            selected_skill_ids = ?,
            selected_mcp_server_ids = ?,
            provider_connection_ids = ?,
            execution = ?,
            exposed_tools = ?,
            schedule_enabled = ?,
            interval_minutes = ?,
            enabled = ?,
            updated_at = ?
        WHERE id = ?
        "#,
    )
    .bind(&request.name)
    .bind(&request.description)
    .bind(&selected_skill_ids)
    .bind(&selected_mcp_server_ids)
    .bind(&provider_connection_ids)
    .bind(&execution)
    .bind(&exposed_tools)
    .bind(i64::from(request.schedule_enabled))
    .bind(i64::from(request.interval_minutes))
    .bind(i64::from(request.enabled))
    .bind(now_millis())
    .bind(&request.agent_id)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to update workspace agent: {}", e))?;

    load_detail(pool.inner(), &request.agent_id)
        .await?
        .ok_or_else(|| {
            format!(
                "Workspace agent not found after update: {}",
                request.agent_id
            )
        })
}

#[tauri::command]
pub async fn workspace_delete_agent(
    workspace_id: String,
    agent_id: String,
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT wa.workspace_id, w.default_workspace_agent_id
        FROM workspace_agents wa
        LEFT JOIN workspaces w ON w.id = wa.workspace_id
        WHERE wa.id = ?
        LIMIT 1
        "#,
    )
    .bind(&agent_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| format!("Failed to look up workspace agent: {}", e))?;

    let Some((row_workspace, default_id)) = row else {
        return Err(format!("Workspace agent not found: {}", agent_id));
    };
    if row_workspace != workspace_id {
        return Err("Workspace agent does not belong to this workspace.".to_string());
    }
    if default_id.as_deref() == Some(agent_id.as_str()) {
        return Err(
            "Cannot delete the workspace's manager agent. Designate a different manager first."
                .to_string(),
        );
    }

    sqlx::query("DELETE FROM workspace_agents WHERE id = ?")
        .bind(&agent_id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to delete workspace agent: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn workspace_set_agent_enabled(
    request: WorkspaceAgentEnabledRequest,
    pool: State<'_, DbPool>,
) -> Result<WorkspaceAgentDetail, String> {
    let row_workspace: Option<String> =
        sqlx::query_scalar("SELECT workspace_id FROM workspace_agents WHERE id = ? LIMIT 1")
            .bind(&request.agent_id)
            .fetch_optional(pool.inner())
            .await
            .map_err(|e| format!("Failed to look up workspace agent: {}", e))?;
    match row_workspace {
        None => return Err(format!("Workspace agent not found: {}", request.agent_id)),
        Some(ws) if ws != request.workspace_id => {
            return Err("Workspace agent does not belong to this workspace.".to_string());
        }
        _ => {}
    }

    sqlx::query("UPDATE workspace_agents SET enabled = ?, updated_at = ? WHERE id = ?")
        .bind(i64::from(request.enabled))
        .bind(now_millis())
        .bind(&request.agent_id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to set workspace agent enabled: {}", e))?;

    load_detail(pool.inner(), &request.agent_id)
        .await?
        .ok_or_else(|| {
            format!(
                "Workspace agent not found after toggle: {}",
                request.agent_id
            )
        })
}

fn validate_agent_fields(
    schedule_enabled: bool,
    interval_minutes: u32,
    exposed_tools: &[ExposedAgentTool],
) -> Result<(), String> {
    if schedule_enabled && interval_minutes == 0 {
        return Err("Scheduled agents must have an interval of at least 1 minute.".to_string());
    }
    for tool in exposed_tools {
        if tool.name.trim().is_empty() {
            return Err("Exposed tool names cannot be empty.".to_string());
        }
        if tool.description.trim().is_empty() {
            return Err(format!(
                "Exposed tool '{}' must have a description.",
                tool.name
            ));
        }
        if !tool.input_schema.is_object() {
            return Err(format!(
                "Exposed tool '{}' must define an object JSON Schema for input_schema.",
                tool.name
            ));
        }
        if !tool.output_schema.is_object() {
            return Err(format!(
                "Exposed tool '{}' must define an object JSON Schema for output_schema.",
                tool.name
            ));
        }
    }
    Ok(())
}

async fn load_detail(pool: &DbPool, id: &str) -> Result<Option<WorkspaceAgentDetail>, String> {
    let row = sqlx::query(
        r#"
        SELECT wa.id, wa.workspace_id, wa.name, wa.description,
               wa.selected_skill_ids, wa.selected_mcp_server_ids,
               wa.provider_connection_ids, wa.execution, wa.exposed_tools,
               wa.schedule_enabled, wa.interval_minutes, wa.enabled,
               wa.created_at, wa.updated_at,
               w.default_workspace_agent_id
        FROM workspace_agents wa
        LEFT JOIN workspaces w ON w.id = wa.workspace_id
        WHERE wa.id = ?
        LIMIT 1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load workspace agent: {}", e))?;

    let Some(row) = row else {
        return Ok(None);
    };
    use sqlx::Row;
    let selected_skill_ids: String = row.try_get("selected_skill_ids").unwrap_or_default();
    let selected_mcp_server_ids: String =
        row.try_get("selected_mcp_server_ids").unwrap_or_default();
    let provider_connection_ids: String =
        row.try_get("provider_connection_ids").unwrap_or_default();
    let execution: String = row.try_get("execution").unwrap_or_default();
    let exposed_tools: String = row.try_get("exposed_tools").unwrap_or_default();
    let default_id: Option<String> = row.try_get("default_workspace_agent_id").ok();
    let row_id: String = row.try_get("id").unwrap_or_default();
    let is_default = default_id.as_deref() == Some(row_id.as_str());

    Ok(Some(WorkspaceAgentDetail {
        id: row_id,
        workspace_id: row.try_get("workspace_id").unwrap_or_default(),
        name: row.try_get("name").unwrap_or_default(),
        description: row.try_get("description").unwrap_or_default(),
        selected_skill_ids: serde_json::from_str(&selected_skill_ids).unwrap_or_default(),
        selected_mcp_server_ids: serde_json::from_str(&selected_mcp_server_ids).unwrap_or_default(),
        provider_connection_ids: serde_json::from_str(&provider_connection_ids).unwrap_or_default(),
        execution: serde_json::from_str::<ExecutionCapabilityConfig>(&execution)
            .unwrap_or_default(),
        exposed_tools: serde_json::from_str::<Vec<ExposedAgentTool>>(&exposed_tools)
            .unwrap_or_default(),
        schedule_enabled: row.try_get::<i64, _>("schedule_enabled").unwrap_or(0) != 0,
        interval_minutes: row
            .try_get::<i64, _>("interval_minutes")
            .unwrap_or(0)
            .max(0) as u32,
        enabled: row.try_get::<i64, _>("enabled").unwrap_or(0) != 0,
        is_default,
        created_at: row.try_get("created_at").unwrap_or(0),
        updated_at: row.try_get("updated_at").unwrap_or(0),
    }))
}

// Suppress unused warning for AgentConfig until callers materialize.
#[allow(dead_code)]
fn _keep_agent_config_in_scope(_agent: AgentConfig) {}

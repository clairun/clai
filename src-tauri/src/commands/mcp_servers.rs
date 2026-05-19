use serde::{Deserialize, Serialize};
use tauri::State;

use crate::assistant::auth::McpSecretStorage;
use crate::config::{McpServerAuth, McpServerConfig, McpServerIntegrationType, McpServerTransport};
use crate::db::DbPool;
use crate::AppState;

/// Removes the given MCP server id from every workspace_agents row's
/// `selected_mcp_server_ids` JSON array.
async fn sweep_workspace_agent_mcp_ids(pool: &DbPool, server_id: &str) -> Result<(), String> {
    let like_pattern = format!("%{}%", server_id);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, selected_mcp_server_ids FROM workspace_agents WHERE selected_mcp_server_ids LIKE ?",
    )
    .bind(&like_pattern)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to scan workspace_agents for MCP references: {}", e))?;

    for (id, mcp_ids_json) in rows {
        let mcp_ids: Vec<String> = serde_json::from_str(&mcp_ids_json).unwrap_or_default();
        let filtered: Vec<String> = mcp_ids.into_iter().filter(|x| x != server_id).collect();
        let encoded = serde_json::to_string(&filtered).unwrap_or_else(|_| "[]".to_string());
        if encoded == mcp_ids_json {
            continue;
        }
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE workspace_agents SET selected_mcp_server_ids = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&encoded)
        .bind(now)
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| {
            format!(
                "Failed to sweep MCP references on workspace agent {}: {}",
                id, e
            )
        })?;
    }

    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMcpServerRequest {
    pub name: String,
    pub enabled: bool,
    pub transport: McpServerTransport,
    #[serde(default)]
    pub integration_type: McpServerIntegrationType,
    #[serde(default)]
    pub auth: McpServerAuthRequest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMcpServerRequest {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: McpServerTransport,
    #[serde(default)]
    pub integration_type: McpServerIntegrationType,
    #[serde(default)]
    pub auth: McpServerAuthRequest,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerAuthRequest {
    #[default]
    None,
    BearerToken {
        #[serde(default)]
        token: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerAuthResponse {
    None,
    BearerToken { has_secret: bool },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerResponse {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: McpServerTransport,
    pub integration_type: McpServerIntegrationType,
    pub auth: McpServerAuthResponse,
    pub created_at: String,
    pub updated_at: String,
}

impl McpServerResponse {
    fn from_config(server: McpServerConfig) -> Self {
        Self {
            id: server.id,
            name: server.name,
            enabled: server.enabled,
            transport: server.transport,
            integration_type: server.integration_type,
            auth: match server.auth {
                McpServerAuth::None => McpServerAuthResponse::None,
                McpServerAuth::BearerToken { secret_ref } => McpServerAuthResponse::BearerToken {
                    has_secret: McpSecretStorage::get_secret(&secret_ref)
                        .ok()
                        .flatten()
                        .map(|secret| !secret.trim().is_empty())
                        .unwrap_or(false),
                },
            },
            created_at: server.created_at,
            updated_at: server.updated_at,
        }
    }
}

#[tauri::command]
pub fn get_mcp_servers(state: State<'_, AppState>) -> Result<Vec<McpServerResponse>, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;

    Ok(config_manager
        .get_mcp_servers()
        .into_iter()
        .map(McpServerResponse::from_config)
        .collect())
}

#[tauri::command]
pub fn get_mcp_server(
    id: String,
    state: State<'_, AppState>,
) -> Result<Option<McpServerResponse>, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;

    Ok(config_manager
        .get_mcp_server(&id)
        .map(McpServerResponse::from_config))
}

#[tauri::command]
pub async fn create_mcp_server(
    request: CreateMcpServerRequest,
    state: State<'_, AppState>,
) -> Result<McpServerResponse, String> {
    let server = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        let mut server = McpServerConfig::new(request.name, request.transport);
        server.enabled = request.enabled;
        server.integration_type = request.integration_type;
        server.auth = build_auth_for_new_server(&server.id, &request.auth)?;
        config_manager
            .add_mcp_server(server.clone())
            .map_err(|e| format!("Failed to create MCP server: {}", e))?;
        server
    };

    sync_mcp_client_manager(&state).await;

    Ok(McpServerResponse::from_config(server))
}

#[tauri::command]
pub async fn update_mcp_server(
    request: UpdateMcpServerRequest,
    state: State<'_, AppState>,
) -> Result<McpServerResponse, String> {
    let server = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        if config_manager.get_mcp_server(&request.id).is_none() {
            return Err(format!("MCP server not found: {}", request.id));
        }

        let existing = config_manager
            .get_mcp_server(&request.id)
            .ok_or_else(|| format!("MCP server not found: {}", request.id))?;
        let next_auth = build_auth_for_existing_server(&existing, &request.auth)?;

        config_manager
            .update_mcp_server(&request.id, |server| {
                server.name = request.name.clone();
                server.enabled = request.enabled;
                server.transport = request.transport.clone();
                server.integration_type = request.integration_type.clone();
                server.auth = next_auth.clone();
            })
            .map_err(|e| format!("Failed to update MCP server: {}", e))?;

        config_manager
            .get_mcp_server(&request.id)
            .ok_or_else(|| "MCP server not found after update".to_string())?
    };

    sync_mcp_client_manager(&state).await;

    Ok(McpServerResponse::from_config(server))
}

#[tauri::command]
pub async fn delete_mcp_server(
    id: String,
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        if let Some(server) = config_manager.get_mcp_server(&id) {
            clear_auth_secret(&server.auth)?;
        }

        let removed = config_manager
            .remove_mcp_server(&id)
            .map_err(|e| format!("Failed to delete MCP server: {}", e))?;

        if !removed {
            return Err(format!("MCP server not found: {}", id));
        }
    }

    // Workspace-local sweep: drop any reference to this server from every
    // workspace_agents row's selected_mcp_server_ids JSON array.
    sweep_workspace_agent_mcp_ids(pool.inner(), &id).await?;

    sync_mcp_client_manager(&state).await;

    Ok(())
}

async fn sync_mcp_client_manager(state: &State<'_, AppState>) {
    let config = match state.config_manager.lock() {
        Ok(config_manager) => config_manager.get(),
        Err(error) => {
            tracing::error!(error = %error, "Failed to lock config manager for MCP sync");
            return;
        }
    };

    let mut manager = state.mcp_client_manager.lock().await;
    manager.sync_from_config(&config);
}

fn build_auth_for_new_server(
    id: &str,
    auth: &McpServerAuthRequest,
) -> Result<McpServerAuth, String> {
    match auth {
        McpServerAuthRequest::None => Ok(McpServerAuth::None),
        McpServerAuthRequest::BearerToken { token } => {
            let token = token
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "Bearer token is required for bearer_token auth".to_string())?;

            let secret_ref = format!("mcp-server::{}::bearer", id);
            McpSecretStorage::set_secret(&secret_ref, token)
                .map_err(|e| format!("Failed to store MCP server credential: {}", e))?;

            Ok(McpServerAuth::BearerToken { secret_ref })
        }
    }
}

fn build_auth_for_existing_server(
    existing: &McpServerConfig,
    auth: &McpServerAuthRequest,
) -> Result<McpServerAuth, String> {
    match auth {
        McpServerAuthRequest::None => {
            clear_auth_secret(&existing.auth)?;
            Ok(McpServerAuth::None)
        }
        McpServerAuthRequest::BearerToken { token } => {
            let secret_ref = match &existing.auth {
                McpServerAuth::BearerToken { secret_ref } => secret_ref.clone(),
                McpServerAuth::None => format!("mcp-server::{}::bearer", existing.id),
            };

            if let Some(token) = token
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                McpSecretStorage::set_secret(&secret_ref, token)
                    .map_err(|e| format!("Failed to store MCP server credential: {}", e))?;
            } else if matches!(existing.auth, McpServerAuth::None) {
                return Err("Bearer token is required when enabling bearer_token auth".to_string());
            }

            Ok(McpServerAuth::BearerToken { secret_ref })
        }
    }
}

fn clear_auth_secret(auth: &McpServerAuth) -> Result<(), String> {
    match auth {
        McpServerAuth::None => Ok(()),
        McpServerAuth::BearerToken { secret_ref } => McpSecretStorage::clear_secret(secret_ref)
            .map_err(|e| format!("Failed to clear MCP server credential: {}", e)),
    }
}

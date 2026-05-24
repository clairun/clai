use serde::{Deserialize, Serialize};
use tauri::State;

use crate::assistant::auth::McpSecretStorage;
use crate::config::{McpServerAuth, McpServerConfig, McpServerIntegrationType, McpServerTransport};
use crate::AppState;

/// Rewrites every workspace agent's `selected_mcp_servers` so its
/// `McpRef.name` entries point at the *current* AppConfig server names.
/// Used after rename — workspace configs store MCP references by name
/// (so they remain portable across machines, where ids differ), which
/// means renaming a server in AppConfig silently de-references all
/// existing selections until they're rewritten. Pass the new app config
/// so this works inside the same critical section that performed the
/// rename.
fn sweep_workspace_agent_mcp_renames(
    state: &AppState,
    app_config: &crate::config::AppConfig,
) -> Result<(), String> {
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
            // Resolve each existing ref to an id (lookup by name with
            // fallback to name-as-id), then convert back to a ref using
            // the current config. Any McpRef whose name was renamed
            // gets refreshed; entries that resolved through the
            // name-as-id fallback are dropped (they were already
            // pointing at nothing).
            let ids = crate::config::workspace_config::refs_to_mcp_ids(
                app_config,
                &agent.selected_mcp_servers,
            );
            let resolved: Vec<String> = ids
                .into_iter()
                .filter(|id| app_config.mcp_servers.iter().any(|s| s.id == *id))
                .collect();
            let new_refs = crate::config::workspace_config::mcp_ids_to_refs(app_config, &resolved);
            if new_refs != agent.selected_mcp_servers {
                agent.selected_mcp_servers = new_refs;
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

/// Removes the given MCP server id from every workspace_agents row's
/// `selected_mcp_server_ids` JSON array.
fn sweep_workspace_agent_mcp_ids(state: &AppState, server_id: &str) -> Result<(), String> {
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
            let ids = crate::config::workspace_config::refs_to_mcp_ids(
                &app_config,
                &agent.selected_mcp_servers,
            );
            if ids.iter().any(|id| id == server_id) {
                let filtered: Vec<String> = ids.into_iter().filter(|id| id != server_id).collect();
                agent.selected_mcp_servers =
                    crate::config::workspace_config::mcp_ids_to_refs(&app_config, &filtered);
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
    let (server, name_changed, app_config_after) = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        let existing = config_manager
            .get_mcp_server(&request.id)
            .ok_or_else(|| format!("MCP server not found: {}", request.id))?;
        let next_auth = build_auth_for_existing_server(&existing, &request.auth)?;
        let name_changed = existing.name != request.name;

        config_manager
            .update_mcp_server(&request.id, |server| {
                server.name = request.name.clone();
                server.enabled = request.enabled;
                server.transport = request.transport.clone();
                server.integration_type = request.integration_type.clone();
                server.auth = next_auth.clone();
            })
            .map_err(|e| format!("Failed to update MCP server: {}", e))?;

        let server = config_manager
            .get_mcp_server(&request.id)
            .ok_or_else(|| "MCP server not found after update".to_string())?;
        // Capture the post-update AppConfig snapshot so the sweep below
        // (which runs after the config_manager lock is released) sees
        // the new name when re-resolving workspace `McpRef`s.
        let app_config_after = config_manager.get();
        (server, name_changed, app_config_after)
    };

    // Workspace configs store MCP refs by name (portable across machines
    // — see [`workspace_config::McpRef`]). Renames in AppConfig would
    // otherwise leave every workspace agent's selection pointing at a
    // stale name that fails to resolve. Rewrite the refs to the current
    // name now so selections stay live.
    if name_changed {
        sweep_workspace_agent_mcp_renames(state.inner(), &app_config_after)?;
    }

    sync_mcp_client_manager(&state).await;

    Ok(McpServerResponse::from_config(server))
}

#[tauri::command]
pub async fn delete_mcp_server(id: String, state: State<'_, AppState>) -> Result<(), String> {
    // Sweep before removing the server from AppConfig so name-based workspace
    // refs still resolve to this id.
    sweep_workspace_agent_mcp_ids(state.inner(), &id)?;

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

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::assistant::auth::McpSecretStorage;
use crate::config::{McpServerAuth, McpServerConfig, McpServerTransport};
use crate::mcp::oauth;
use crate::AppState;

fn default_true() -> bool {
    true
}

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
        // Atomic RMW (see workspace_config::update); unchanged configs are
        // rewritten with identical content, which the atomic save makes
        // harmless — sweeps only run on rare rename/delete actions.
        let (changed, config) =
            crate::config::workspace_config::update(&locator.root_path, |config| {
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
                    let new_refs =
                        crate::config::workspace_config::mcp_ids_to_refs(app_config, &resolved);
                    if new_refs != agent.selected_mcp_servers {
                        agent.selected_mcp_servers = new_refs;
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
        // Atomic RMW (see workspace_config::update); unchanged configs are
        // rewritten with identical content, which the atomic save makes
        // harmless — sweeps only run on rare rename/delete actions.
        let (changed, config) =
            crate::config::workspace_config::update(&locator.root_path, |config| {
                let mut changed = false;
                let now = chrono::Utc::now().timestamp_millis();
                for agent in &mut config.agents {
                    let ids = crate::config::workspace_config::refs_to_mcp_ids(
                        &app_config,
                        &agent.selected_mcp_servers,
                    );
                    if ids.iter().any(|id| id == server_id) {
                        let filtered: Vec<String> =
                            ids.into_iter().filter(|id| id != server_id).collect();
                        agent.selected_mcp_servers =
                            crate::config::workspace_config::mcp_ids_to_refs(
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
pub struct CreateMcpServerRequest {
    pub name: String,
    pub enabled: bool,
    pub transport: McpServerTransport,
    #[serde(default)]
    pub auth: McpServerAuthRequest,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct UpdateMcpServerRequest {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: McpServerTransport,
    #[serde(default)]
    pub auth: McpServerAuthRequest,
}

#[derive(Debug, Clone, Deserialize, Default, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum McpServerAuthRequest {
    #[default]
    None,
    BearerToken {
        #[serde(default)]
        token: Option<String>,
    },
    #[serde(rename = "oauth")]
    OAuth {
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        client_id: Option<String>,
        #[serde(default)]
        client_secret: Option<String>,
        #[serde(default)]
        client_metadata_url: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum McpServerAuthResponse {
    None,
    BearerToken {
        has_secret: bool,
    },
    #[serde(rename = "oauth")]
    OAuth {
        connected: bool,
        needs_login: bool,
        authorization_server_issuer: Option<String>,
        scopes: Vec<String>,
        client_id_configured: bool,
        client_secret_configured: bool,
        client_metadata_url: Option<String>,
        last_error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct McpServerResponse {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: McpServerTransport,
    pub auth: McpServerAuthResponse,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct StartMcpOAuthLoginRequest {
    #[serde(default)]
    pub server_id: Option<String>,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub url: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub client_metadata_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct McpOAuthStartResponse {
    pub login_id: String,
    pub server_id: String,
    pub authorization_url: String,
    pub expires_at: String,
}

impl McpServerResponse {
    fn from_config(server: McpServerConfig) -> Self {
        let server_id = server.id.clone();
        Self {
            id: server.id,
            name: server.name,
            enabled: server.enabled,
            transport: server.transport,
            auth: match server.auth {
                McpServerAuth::None => McpServerAuthResponse::None,
                McpServerAuth::BearerToken { secret_ref } => McpServerAuthResponse::BearerToken {
                    has_secret: McpSecretStorage::get_secret(&secret_ref)
                        .ok()
                        .flatten()
                        .map(|secret| !secret.trim().is_empty())
                        .unwrap_or(false),
                },
                McpServerAuth::OAuth {
                    credential_ref,
                    authorization_server_issuer,
                    client_id,
                    client_metadata_url,
                    scopes,
                } => {
                    let connected = oauth::has_stored_oauth_credentials(&credential_ref);
                    McpServerAuthResponse::OAuth {
                        connected,
                        needs_login: !connected,
                        authorization_server_issuer,
                        scopes,
                        client_id_configured: client_id.is_some(),
                        client_secret_configured: oauth::has_stored_oauth_client_secret(&server_id),
                        client_metadata_url,
                        last_error: None,
                    }
                }
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
pub fn get_mcp_server_catalog() -> Vec<oauth::McpCatalogEntry> {
    oauth::catalog_entries()
}

#[tauri::command]
pub async fn start_mcp_oauth_login(
    request: StartMcpOAuthLoginRequest,
    state: State<'_, AppState>,
) -> Result<McpOAuthStartResponse, String> {
    let name = request.name.trim().to_string();
    if name.is_empty() {
        return Err("MCP server name is required".to_string());
    }
    let url = validate_http_url(&request.url)?;
    if let Some(server_id) = request.server_id.as_deref() {
        let exists = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?
            .get_mcp_server(server_id)
            .is_some();
        if !exists {
            return Err(format!("MCP server not found: {}", server_id));
        }
    }

    let server_id = request
        .server_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let login_id = uuid::Uuid::new_v4().to_string();
    let scopes = sanitize_scopes(&request.scopes);
    let manual_client_id = non_empty_opt(request.client_id.as_deref()).map(str::to_string);
    let manual_client_secret = non_empty_opt(request.client_secret.as_deref()).map(str::to_string);
    let client_metadata_url = if manual_client_id.is_some() {
        non_empty_opt(request.client_metadata_url.as_deref()).map(str::to_string)
    } else {
        non_empty_opt(request.client_metadata_url.as_deref())
            .map(str::to_string)
            .or_else(|| Some(oauth::CLAI_CLIENT_METADATA_URL.to_string()))
    };

    let callback_listener = oauth::start_callback_listener().await?;
    let credential_ref = oauth::oauth_credential_ref(&server_id);
    let (authorization_session, expected_issuer) = match oauth::build_authorization_session(
        &url,
        credential_ref,
        &scopes,
        &callback_listener.redirect_uri,
        manual_client_id.as_deref(),
        manual_client_secret.as_deref(),
        client_metadata_url.as_deref(),
    )
    .await
    {
        Ok(session) => session,
        Err(error) => {
            callback_listener.cancellation_token.cancel();
            return Err(error);
        }
    };

    let authorization_url = authorization_session.get_authorization_url().to_string();
    let expires_at = (chrono::Utc::now()
        + chrono::Duration::seconds(oauth::OAUTH_LOGIN_TIMEOUT_SECS as i64))
    .to_rfc3339();
    let draft = oauth::PendingMcpOAuthDraft {
        existing_server_id: request.server_id,
        server_id: server_id.clone(),
        name,
        enabled: request.enabled,
        url,
        scopes,
        client_metadata_url,
        manual_client_secret,
    };
    let pending = oauth::PendingMcpOAuthSession {
        login_id: login_id.clone(),
        draft,
        expected_issuer,
        authorization_session,
        callback_listener,
    };
    state.pending_mcp_oauth.lock().await.insert(pending);

    Ok(McpOAuthStartResponse {
        login_id,
        server_id,
        authorization_url,
        expires_at,
    })
}

#[tauri::command]
pub async fn finish_mcp_oauth_login(
    login_id: String,
    state: State<'_, AppState>,
) -> Result<McpServerResponse, String> {
    let pending = state
        .pending_mcp_oauth
        .lock()
        .await
        .remove(&login_id)
        .ok_or_else(|| format!("OAuth login session not found: {}", login_id))?;

    let callback = match tokio::time::timeout(
        std::time::Duration::from_secs(oauth::OAUTH_LOGIN_TIMEOUT_SECS),
        pending.callback_listener.receiver,
    )
    .await
    {
        Ok(Ok(result)) => result?,
        Ok(Err(_)) => return Err("OAuth callback listener closed before completing".to_string()),
        Err(_) => {
            pending.callback_listener.cancellation_token.cancel();
            return Err("OAuth login timed out".to_string());
        }
    };
    pending.callback_listener.cancellation_token.cancel();

    if !oauth::callback_issuer_matches(pending.expected_issuer.as_deref(), callback.iss.as_deref())
    {
        return Err("OAuth callback issuer did not match authorization server issuer".to_string());
    }

    pending
        .authorization_session
        .handle_callback(&callback.code, &callback.state)
        .await
        .map_err(|error| format!("OAuth token exchange failed: {}", error))?;
    let (client_id, _) = pending
        .authorization_session
        .get_credentials()
        .await
        .map_err(|error| format!("Failed to read OAuth credentials after login: {}", error))?;

    if let Some(client_secret) = pending.draft.manual_client_secret.as_deref() {
        McpSecretStorage::set_secret(
            &oauth::oauth_client_secret_ref(&pending.draft.server_id),
            client_secret,
        )
        .map_err(|e| format!("Failed to store MCP OAuth client secret: {}", e))?;
    }

    let auth = McpServerAuth::OAuth {
        credential_ref: oauth::oauth_credential_ref(&pending.draft.server_id),
        authorization_server_issuer: pending.expected_issuer.clone(),
        client_id: Some(client_id),
        client_metadata_url: pending.draft.client_metadata_url.clone(),
        scopes: pending.draft.scopes.clone(),
    };

    let (server, name_changed, app_config_after) = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        if let Some(existing_id) = pending.draft.existing_server_id.as_deref() {
            let existing = config_manager
                .get_mcp_server(existing_id)
                .ok_or_else(|| format!("MCP server not found: {}", existing_id))?;
            let name_changed = existing.name != pending.draft.name;
            clear_auth_secret_if_replaced(&existing.id, &existing.auth, &auth)?;
            config_manager
                .update_mcp_server(existing_id, |server| {
                    server.name = pending.draft.name.clone();
                    server.enabled = pending.draft.enabled;
                    server.transport = McpServerTransport::Http {
                        url: pending.draft.url.clone(),
                    };
                    server.auth = auth.clone();
                })
                .map_err(|e| format!("Failed to update MCP server: {}", e))?;
            let server = config_manager
                .get_mcp_server(existing_id)
                .ok_or_else(|| "MCP server not found after OAuth update".to_string())?;
            let app_config_after = config_manager.get();
            (server, name_changed, app_config_after)
        } else {
            let now = chrono::Utc::now().to_rfc3339();
            let server = McpServerConfig {
                id: pending.draft.server_id.clone(),
                name: pending.draft.name.clone(),
                enabled: pending.draft.enabled,
                transport: McpServerTransport::Http {
                    url: pending.draft.url.clone(),
                },
                auth,
                created_at: now.clone(),
                updated_at: now,
            };
            config_manager
                .add_mcp_server(server.clone())
                .map_err(|e| format!("Failed to create MCP server: {}", e))?;
            let app_config_after = config_manager.get();
            (server, false, app_config_after)
        }
    };

    if name_changed {
        sweep_workspace_agent_mcp_renames(state.inner(), &app_config_after)?;
    }

    sync_mcp_client_manager(&state).await;
    Ok(McpServerResponse::from_config(server))
}

#[tauri::command]
pub async fn cancel_mcp_oauth_login(
    login_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.pending_mcp_oauth.lock().await.cancel(&login_id);
    Ok(())
}

#[tauri::command]
pub async fn disconnect_mcp_oauth(
    id: String,
    state: State<'_, AppState>,
) -> Result<McpServerResponse, String> {
    let server = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let existing = config_manager
            .get_mcp_server(&id)
            .ok_or_else(|| format!("MCP server not found: {}", id))?;
        if !matches!(existing.auth, McpServerAuth::OAuth { .. }) {
            return Err(format!("MCP server is not configured for OAuth: {}", id));
        }
        clear_auth_secret(&existing.id, &existing.auth)?;
        config_manager
            .update_mcp_server(&id, |server| {
                server.auth = McpServerAuth::None;
            })
            .map_err(|e| format!("Failed to disconnect MCP OAuth server: {}", e))?;
        config_manager
            .get_mcp_server(&id)
            .ok_or_else(|| "MCP server not found after disconnect".to_string())?
    };

    sync_mcp_client_manager(&state).await;
    Ok(McpServerResponse::from_config(server))
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
            clear_auth_secret(&server.id, &server.auth)?;
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
        McpServerAuthRequest::OAuth {
            scopes,
            client_id,
            client_secret,
            client_metadata_url,
        } => {
            if let Some(client_secret) = non_empty_opt(client_secret.as_deref()) {
                McpSecretStorage::set_secret(&oauth::oauth_client_secret_ref(id), client_secret)
                    .map_err(|e| format!("Failed to store MCP OAuth client secret: {}", e))?;
            }

            Ok(McpServerAuth::OAuth {
                credential_ref: oauth::oauth_credential_ref(id),
                authorization_server_issuer: None,
                client_id: non_empty_opt(client_id.as_deref()).map(str::to_string),
                client_metadata_url: non_empty_opt(client_metadata_url.as_deref())
                    .map(str::to_string)
                    .or_else(|| Some(oauth::CLAI_CLIENT_METADATA_URL.to_string())),
                scopes: sanitize_scopes(scopes),
            })
        }
    }
}

fn build_auth_for_existing_server(
    existing: &McpServerConfig,
    auth: &McpServerAuthRequest,
) -> Result<McpServerAuth, String> {
    match auth {
        McpServerAuthRequest::None => {
            clear_auth_secret(&existing.id, &existing.auth)?;
            Ok(McpServerAuth::None)
        }
        McpServerAuthRequest::BearerToken { token } => {
            let secret_ref = match &existing.auth {
                McpServerAuth::BearerToken { secret_ref } => secret_ref.clone(),
                _ => {
                    clear_auth_secret(&existing.id, &existing.auth)?;
                    format!("mcp-server::{}::bearer", existing.id)
                }
            };

            if let Some(token) = token
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                McpSecretStorage::set_secret(&secret_ref, token)
                    .map_err(|e| format!("Failed to store MCP server credential: {}", e))?;
            } else if !matches!(existing.auth, McpServerAuth::BearerToken { .. }) {
                return Err("Bearer token is required when enabling bearer_token auth".to_string());
            }

            Ok(McpServerAuth::BearerToken { secret_ref })
        }
        McpServerAuthRequest::OAuth {
            scopes,
            client_id,
            client_secret,
            client_metadata_url,
        } => {
            if !matches!(existing.auth, McpServerAuth::OAuth { .. }) {
                clear_auth_secret(&existing.id, &existing.auth)?;
            }

            if let Some(client_secret) = non_empty_opt(client_secret.as_deref()) {
                McpSecretStorage::set_secret(
                    &oauth::oauth_client_secret_ref(&existing.id),
                    client_secret,
                )
                .map_err(|e| format!("Failed to store MCP OAuth client secret: {}", e))?;
            }

            let (
                credential_ref,
                authorization_server_issuer,
                existing_client_id,
                existing_metadata_url,
            ) = match &existing.auth {
                McpServerAuth::OAuth {
                    credential_ref,
                    authorization_server_issuer,
                    client_id,
                    client_metadata_url,
                    ..
                } => (
                    credential_ref.clone(),
                    authorization_server_issuer.clone(),
                    client_id.clone(),
                    client_metadata_url.clone(),
                ),
                _ => (oauth::oauth_credential_ref(&existing.id), None, None, None),
            };

            Ok(McpServerAuth::OAuth {
                credential_ref,
                authorization_server_issuer,
                client_id: non_empty_opt(client_id.as_deref())
                    .map(str::to_string)
                    .or(existing_client_id),
                client_metadata_url: non_empty_opt(client_metadata_url.as_deref())
                    .map(str::to_string)
                    .or(existing_metadata_url)
                    .or_else(|| Some(oauth::CLAI_CLIENT_METADATA_URL.to_string())),
                scopes: sanitize_scopes(scopes),
            })
        }
    }
}

fn clear_auth_secret(server_id: &str, auth: &McpServerAuth) -> Result<(), String> {
    match auth {
        McpServerAuth::None => Ok(()),
        McpServerAuth::BearerToken { secret_ref } => McpSecretStorage::clear_secret(secret_ref)
            .map_err(|e| format!("Failed to clear MCP server credential: {}", e)),
        McpServerAuth::OAuth { credential_ref, .. } => {
            oauth::clear_oauth_secrets(server_id, credential_ref)
        }
    }
}

fn clear_auth_secret_if_replaced(
    server_id: &str,
    old_auth: &McpServerAuth,
    new_auth: &McpServerAuth,
) -> Result<(), String> {
    match (old_auth, new_auth) {
        (
            McpServerAuth::OAuth {
                credential_ref: old_ref,
                ..
            },
            McpServerAuth::OAuth {
                credential_ref: new_ref,
                ..
            },
        ) if old_ref == new_ref => Ok(()),
        _ => clear_auth_secret(server_id, old_auth),
    }
}

fn non_empty_opt(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn sanitize_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .map(|scope| scope.trim())
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate_http_url(raw_url: &str) -> Result<String, String> {
    let url = raw_url.trim();
    if url.is_empty() {
        return Err("HTTP MCP server URL is required".to_string());
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("Invalid MCP server URL: {}", e))?;
    match parsed.scheme() {
        "http" | "https" => Ok(url.to_string()),
        scheme => Err(format!(
            "MCP OAuth requires an HTTP or HTTPS URL, got scheme `{}`",
            scheme
        )),
    }
}

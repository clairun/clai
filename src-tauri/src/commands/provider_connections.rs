use serde::{Deserialize, Serialize};
use tauri::State;

use crate::assistant::auth::ProviderSecretStorage;
use crate::assistant::providers;
use crate::assistant::repository;
use crate::assistant::repository::{
    CreateProviderConnectionParams, UpdateProviderConnectionParams,
};
use crate::assistant::types::{AuthMode, ModelInfo, ProviderConnection, ProviderDescriptor};
use crate::db::DbPool;
use crate::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProviderConnectionRequest {
    pub name: String,
    pub provider_id: String,
    pub api_key: String,
    #[serde(default)]
    pub base_url: Option<String>,
    pub model_id: String,
    #[serde(default)]
    pub account_label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderConnectionRequest {
    pub id: String,
    pub name: String,
    pub provider_id: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    pub model_id: String,
    #[serde(default)]
    pub account_label: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[tauri::command]
pub async fn provider_connection_list_available() -> Result<Vec<ProviderDescriptor>, String> {
    Ok(providers::supported_providers())
}

#[tauri::command]
pub async fn provider_connection_create(
    request: CreateProviderConnectionRequest,
    pool: State<'_, DbPool>,
) -> Result<ProviderConnection, String> {
    let descriptor = providers::get_provider_descriptor(&request.provider_id)
        .ok_or_else(|| format!("Unsupported provider: {}", request.provider_id))?;

    if !descriptor
        .supported_auth_modes
        .contains(&AuthMode::DeveloperApiKey)
    {
        return Err(format!(
            "Provider '{}' does not support developer API keys",
            request.provider_id
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let secret_ref = format!("provider-connection::{}", id);
    ProviderSecretStorage::set_secret(&secret_ref, request.api_key.trim())
        .map_err(|e| format!("Failed to store provider credential: {}", e))?;

    repository::create_provider_connection(
        pool.inner(),
        CreateProviderConnectionParams {
            id,
            name: request.name.trim().to_string(),
            provider_id: request.provider_id,
            auth_mode: AuthMode::DeveloperApiKey,
            base_url: request
                .base_url
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            secret_ref,
            model_id: request.model_id.trim().to_string(),
            account_label: request
                .account_label
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            enabled: true,
        },
    )
    .await
}

#[tauri::command]
pub async fn provider_connection_update(
    request: UpdateProviderConnectionRequest,
    pool: State<'_, DbPool>,
) -> Result<ProviderConnection, String> {
    let existing = repository::get_provider_connection(pool.inner(), &request.id)
        .await?
        .ok_or_else(|| format!("Provider connection not found: {}", request.id))?;

    let descriptor = providers::get_provider_descriptor(&request.provider_id)
        .ok_or_else(|| format!("Unsupported provider: {}", request.provider_id))?;

    if !descriptor
        .supported_auth_modes
        .contains(&AuthMode::DeveloperApiKey)
    {
        return Err(format!(
            "Provider '{}' does not support developer API keys",
            request.provider_id
        ));
    }

    if let Some(api_key) = request
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        ProviderSecretStorage::set_secret(&existing.secret_ref, api_key)
            .map_err(|e| format!("Failed to store provider credential: {}", e))?;
    }

    repository::update_provider_connection(
        pool.inner(),
        UpdateProviderConnectionParams {
            id: request.id,
            name: request.name.trim().to_string(),
            provider_id: request.provider_id,
            auth_mode: AuthMode::DeveloperApiKey,
            base_url: request
                .base_url
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            secret_ref: existing.secret_ref,
            model_id: request.model_id.trim().to_string(),
            account_label: request
                .account_label
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            enabled: request.enabled,
        },
    )
    .await
}

#[tauri::command]
pub async fn provider_connection_delete(
    id: String,
    pool: State<'_, DbPool>,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    // Refuse delete if any workspace_agent still references this connection.
    // We scan the JSON array `provider_connection_ids` for the id substring,
    // then confirm with a per-row parse.
    let like_pattern = format!("%{}%", id);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, provider_connection_ids FROM workspace_agents WHERE provider_connection_ids LIKE ?",
    )
    .bind(&like_pattern)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| format!("Failed to scan workspace_agents for provider use: {}", e))?;

    let dependents: Vec<String> = rows
        .into_iter()
        .filter_map(|(name, ids_json)| {
            let ids: Vec<String> = serde_json::from_str(&ids_json).unwrap_or_default();
            if ids.iter().any(|value| value == &id) {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    if !dependents.is_empty() {
        return Err(format!(
            "Cannot delete provider connection while used by {} agent(s)",
            dependents.len()
        ));
    }

    let _ = state; // config_manager no longer consulted; agents live in DB.

    if let Some(connection) = repository::get_provider_connection(pool.inner(), &id).await? {
        ProviderSecretStorage::clear_secret(&connection.secret_ref)
            .map_err(|e| format!("Failed to clear provider credential: {}", e))?;
    }

    repository::delete_provider_connection(pool.inner(), &id).await
}

#[tauri::command]
pub async fn provider_connection_get(
    id: String,
    pool: State<'_, DbPool>,
) -> Result<Option<ProviderConnection>, String> {
    repository::get_provider_connection(pool.inner(), &id).await
}

#[tauri::command]
pub async fn provider_connection_list(
    pool: State<'_, DbPool>,
) -> Result<Vec<ProviderConnection>, String> {
    repository::list_provider_connections(pool.inner()).await
}

#[tauri::command]
pub async fn provider_connection_list_models(
    id: String,
    pool: State<'_, DbPool>,
) -> Result<Vec<ModelInfo>, String> {
    let connection = repository::get_provider_connection(pool.inner(), &id)
        .await?
        .ok_or_else(|| format!("Provider connection not found: {}", id))?;

    let adapter = providers::resolve_adapter(&connection.provider_id).map_err(|e| e.to_string())?;
    adapter
        .list_models(&connection)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn provider_connection_test(
    id: String,
    pool: State<'_, DbPool>,
) -> Result<TestResult, String> {
    let connection = repository::get_provider_connection(pool.inner(), &id)
        .await?
        .ok_or_else(|| format!("Provider connection not found: {}", id))?;

    tracing::info!(
        connection_id = %connection.id,
        connection_name = %connection.name,
        provider_id = %connection.provider_id,
        model_id = %connection.model_id,
        base_url = ?connection.base_url,
        auth_mode = ?connection.auth_mode,
        "Testing provider connection"
    );

    let adapter = providers::resolve_adapter(&connection.provider_id).map_err(|e| {
        tracing::error!(
            provider_id = %connection.provider_id,
            error = %e,
            "Failed to resolve provider adapter"
        );
        e.to_string()
    })?;

    // Try list_models first; if that fails (some providers don't implement it),
    // fall back to a minimal completion request to verify auth and connectivity.
    match adapter.list_models(&connection).await {
        Ok(models) => {
            tracing::info!(
                connection_id = %connection.id,
                model_count = models.len(),
                "Provider connection test succeeded (via list_models)"
            );
            return Ok(TestResult {
                success: true,
                error: None,
            });
        }
        Err(error) => {
            tracing::warn!(
                connection_id = %connection.id,
                error = %error,
                "list_models failed, falling back to completion test"
            );
        }
    }

    // Fallback: send a minimal completion request
    use crate::assistant::types::{
        CompletionRequest, ContentPart, MessageRole, ProviderEvent, ProviderInputMessage,
    };
    use futures::StreamExt;

    let request = CompletionRequest {
        run_id: format!("test:{}", connection.id),
        session_id: format!("test-session:{}", connection.id),
        model_id: connection.model_id.clone(),
        messages: vec![ProviderInputMessage {
            role: MessageRole::User,
            content: vec![ContentPart::Text {
                text: "hi".to_string(),
            }],
        }],
        tools: vec![],
        temperature: Some(0.0),
        max_output_tokens: Some(1),
    };

    match adapter.stream_completion(&connection, request).await {
        Ok(mut stream) => {
            let mut got_response = false;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ProviderEvent::TextDelta { .. }) | Ok(ProviderEvent::MessageComplete) => {
                        got_response = true;
                        break;
                    }
                    Ok(ProviderEvent::ProviderError { message }) => {
                        tracing::error!(
                            connection_id = %connection.id,
                            error = %message,
                            "Completion test returned provider error"
                        );
                        return Ok(TestResult {
                            success: false,
                            error: Some(message),
                        });
                    }
                    Err(e) => {
                        tracing::error!(
                            connection_id = %connection.id,
                            error = %e,
                            "Completion test stream error"
                        );
                        return Ok(TestResult {
                            success: false,
                            error: Some(e.to_string()),
                        });
                    }
                    _ => continue,
                }
            }
            if got_response {
                tracing::info!(
                    connection_id = %connection.id,
                    "Provider connection test succeeded (via completion)"
                );
                Ok(TestResult {
                    success: true,
                    error: None,
                })
            } else {
                Ok(TestResult {
                    success: false,
                    error: Some("No response received from provider".to_string()),
                })
            }
        }
        Err(error) => {
            tracing::error!(
                connection_id = %connection.id,
                connection_name = %connection.name,
                provider_id = %connection.provider_id,
                base_url = ?connection.base_url,
                error = %error,
                "Provider connection test failed"
            );
            Ok(TestResult {
                success: false,
                error: Some(error.to_string()),
            })
        }
    }
}

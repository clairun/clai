use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::process::Command;

use crate::assistant::auth::ProviderSecretStorage;
use crate::assistant::providers::{self, cli};
use crate::assistant::types::{AuthMode, ModelInfo, ProviderConnection, ProviderDescriptor};
use crate::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProviderConnectionRequest {
    pub name: String,
    pub provider_id: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_mode: Option<AuthMode>,
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
    pub auth_mode: Option<AuthMode>,
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
    state: State<'_, AppState>,
) -> Result<ProviderConnection, String> {
    let descriptor = providers::get_provider_descriptor(&request.provider_id)
        .ok_or_else(|| format!("Unsupported provider: {}", request.provider_id))?;

    if !descriptor.supported_auth_modes.contains(
        &request
            .auth_mode
            .clone()
            .unwrap_or(AuthMode::DeveloperApiKey),
    ) {
        return Err(format!(
            "Provider '{}' does not support the requested auth mode",
            request.provider_id
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let auth_mode = request.auth_mode.unwrap_or(AuthMode::DeveloperApiKey);
    let secret_ref = format!("provider-connection::{}", id);
    if auth_mode == AuthMode::DeveloperApiKey {
        let api_key = request
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "API key is required for developer API key connections".to_string())?;
        ProviderSecretStorage::set_secret(&secret_ref, api_key)
            .map_err(|e| format!("Failed to store provider credential: {}", e))?;
    }

    let now = chrono::Utc::now().timestamp_millis();
    let connection = ProviderConnection {
        id,
        name: request.name.trim().to_string(),
        provider_id: request.provider_id,
        auth_mode,
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
        created_at: now,
        updated_at: now,
    };
    state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .add_provider_connection(connection.clone())
        .map_err(|e| format!("Failed to create provider connection: {}", e))?;
    Ok(connection)
}

#[tauri::command]
pub async fn provider_connection_update(
    request: UpdateProviderConnectionRequest,
    state: State<'_, AppState>,
) -> Result<ProviderConnection, String> {
    let existing = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connection(&request.id)
        .ok_or_else(|| format!("Provider connection not found: {}", request.id))?;

    let descriptor = providers::get_provider_descriptor(&request.provider_id)
        .ok_or_else(|| format!("Unsupported provider: {}", request.provider_id))?;

    if !descriptor.supported_auth_modes.contains(
        &request
            .auth_mode
            .clone()
            .unwrap_or(existing.auth_mode.clone()),
    ) {
        return Err(format!(
            "Provider '{}' does not support the requested auth mode",
            request.provider_id
        ));
    }

    let auth_mode = request.auth_mode.unwrap_or(existing.auth_mode.clone());
    if let Some(api_key) = request
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if auth_mode == AuthMode::DeveloperApiKey {
            ProviderSecretStorage::set_secret(&existing.secret_ref, api_key)
                .map_err(|e| format!("Failed to store provider credential: {}", e))?;
        }
    }

    let updated = ProviderConnection {
        id: request.id,
        name: request.name.trim().to_string(),
        provider_id: request.provider_id,
        auth_mode,
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
        created_at: existing.created_at,
        updated_at: chrono::Utc::now().timestamp_millis(),
    };
    state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .update_provider_connection(updated.clone())
        .map_err(|e| format!("Failed to update provider connection: {}", e))?;
    Ok(updated)
}

#[tauri::command]
pub async fn provider_connection_delete(
    id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();
    let mut dependents = Vec::new();
    for locator in locators {
        let Ok(config) = crate::config::workspace_config::load(&locator.root_path) else {
            continue;
        };
        for agent in config.agents {
            if agent
                .provider_connection_ids
                .iter()
                .any(|value| value == &id)
            {
                dependents.push(agent.name);
            }
        }
    }

    if !dependents.is_empty() {
        return Err(format!(
            "Cannot delete provider connection while used by {} agent(s)",
            dependents.len()
        ));
    }

    let connection = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connection(&id);
    if let Some(connection) = connection {
        ProviderSecretStorage::clear_secret(&connection.secret_ref)
            .map_err(|e| format!("Failed to clear provider credential: {}", e))?;
    }

    state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .remove_provider_connection(&id)
        .map_err(|e| format!("Failed to delete provider connection: {}", e))
}

#[tauri::command]
pub async fn provider_connection_get(
    id: String,
    state: State<'_, AppState>,
) -> Result<Option<ProviderConnection>, String> {
    Ok(state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connection(&id))
}

#[tauri::command]
pub async fn provider_connection_list(
    state: State<'_, AppState>,
) -> Result<Vec<ProviderConnection>, String> {
    Ok(state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connections())
}

#[tauri::command]
pub async fn provider_descriptor_models(provider_id: String) -> Result<Vec<ModelInfo>, String> {
    cli::models_for_provider(&provider_id)
        .ok_or_else(|| format!("No static model list for provider '{}'", provider_id))
}

#[tauri::command]
pub async fn provider_connection_list_models(
    id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ModelInfo>, String> {
    let connection = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connection(&id)
        .ok_or_else(|| format!("Provider connection not found: {}", id))?;

    if let Some(models) = cli::models_for_provider(&connection.provider_id) {
        return Ok(models);
    }

    let adapter = providers::resolve_adapter(&connection.provider_id).map_err(|e| e.to_string())?;
    adapter
        .list_models(&connection)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn provider_connection_test(
    id: String,
    state: State<'_, AppState>,
) -> Result<TestResult, String> {
    let connection = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connection(&id)
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

    if providers::is_cli_provider(&connection.provider_id) {
        return test_cli_provider_connection(&connection).await;
    }

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

async fn test_cli_provider_connection(
    connection: &ProviderConnection,
) -> Result<TestResult, String> {
    let command = connection
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| cli::command_for_provider(&connection.provider_id).map(str::to_string))
        .ok_or_else(|| format!("Unsupported CLI provider: {}", connection.provider_id))?;

    let output = if connection.provider_id == cli::CLAUDE_CODE_PROVIDER_ID {
        Command::new(&command)
            .args(["auth", "status"])
            .output()
            .await
            .map_err(|e| format!("Failed to run `{}`: {}", command, e))?
    } else {
        Command::new(&command)
            .arg("--version")
            .output()
            .await
            .map_err(|e| format!("Failed to run `{}`: {}", command, e))?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(TestResult {
            success: false,
            error: Some(if stderr.is_empty() { stdout } else { stderr }),
        });
    }

    if connection.provider_id == cli::CLAUDE_CODE_PROVIDER_ID {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| format!("Claude auth status returned invalid JSON: {}", e))?;
        if !value
            .get("loggedIn")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(TestResult {
                success: false,
                error: Some("Claude Code is installed but not logged in".to_string()),
            });
        }
    }

    Ok(TestResult {
        success: true,
        error: None,
    })
}

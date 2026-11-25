use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use crate::assistant::auth::env_vars::validate_env_vars;
use crate::assistant::auth::ProviderSecretStorage;
use crate::assistant::providers::catalog::ProviderCatalogEntry;
use crate::assistant::providers::{self, catalog, cli};
use crate::assistant::types::{AuthMode, ModelInfo, ProviderConnection, ProviderDescriptor};
use crate::config::{ConfigManager, McpEnvVar};
use crate::AppState;

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct CreateProviderConnectionRequest {
    pub name: String,
    pub protocol_id: String,
    /// Brand/catalog id. Defaults to `protocol_id` when omitted.
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_mode: Option<AuthMode>,
    #[serde(default)]
    pub base_url: Option<String>,
    pub model_id: String,
    #[serde(default)]
    pub account_label: Option<String>,
    /// Env vars for a CLI-backed connection's process. A secret entry sends
    /// its value once; it is stored in the vault and never returned.
    #[serde(default)]
    #[ts(as = "Option<Vec<McpEnvVar>>", optional)]
    pub env: Vec<McpEnvVar>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct UpdateProviderConnectionRequest {
    pub id: String,
    pub name: String,
    pub protocol_id: String,
    /// Brand/catalog id. Defaults to `protocol_id` when omitted.
    #[serde(default)]
    pub provider_id: Option<String>,
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
    /// Replaces the connection's env vars; omitted keeps them. A secret
    /// entry sent without a value keeps its stored secret.
    #[serde(default)]
    #[ts(optional)]
    pub env: Option<Vec<McpEnvVar>>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct ProbeModelsRequest {
    pub protocol_id: String,
    /// Existing connection to probe with. When present and no replacement
    /// `api_key` is supplied, the probe reuses the connection's stored secret.
    #[serde(default)]
    pub connection_id: Option<String>,
    /// Brand/catalog id, if probing a catalog preset. Needed so brand-scoped
    /// quirks (OpenRouter headers, MiniMax's dedicated models endpoint) apply.
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct TestResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[tauri::command]
pub async fn provider_connection_list_available() -> Result<Vec<ProviderDescriptor>, String> {
    Ok(providers::supported_providers())
}

/// The bundled predefined-provider catalog (presets with endpoint + logo +
/// curated models). Frontend uses it to offer a pick-a-provider flow.
#[tauri::command]
pub async fn provider_catalog_list() -> Result<Vec<ProviderCatalogEntry>, String> {
    Ok(catalog::catalog_entries())
}

/// Probe a provider's live model list at creation time — before a connection
/// is saved. Builds an ephemeral connection (transient secret, cleared after
/// the call) and reuses the adapter's `list_models`. On failure the caller
/// falls back to the catalog entry's `curated_models`.
#[tauri::command]
pub async fn provider_catalog_probe_models(
    request: ProbeModelsRequest,
    state: State<'_, AppState>,
) -> Result<Vec<ModelInfo>, String> {
    let probe_id = uuid::Uuid::new_v4().to_string();
    probe_models(&state.config_manager, request, probe_id).await
}

async fn probe_models(
    config_manager: &Mutex<ConfigManager>,
    request: ProbeModelsRequest,
    probe_id: String,
) -> Result<Vec<ModelInfo>, String> {
    // CLI providers have static lists — no live probe needed.
    if let Some(models) = cli::models_for_provider(&request.protocol_id) {
        return Ok(models);
    }

    let connection = probe_connection(config_manager, &request, probe_id)?;
    let typed_key = request.typed_key();
    if let Some(key) = typed_key {
        ProviderSecretStorage::set_secret(&connection.secret_ref, key)
            .map_err(|e| format!("Failed to store probe credential: {}", e))?;
    }

    let adapter = providers::resolve_connection_adapter(&connection).map_err(|e| e.to_string());
    let result = match adapter {
        Ok(adapter) => adapter
            .list_models(&connection)
            .await
            .map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };

    if typed_key.is_some() {
        let _ = ProviderSecretStorage::clear_secret(&connection.secret_ref);
    }
    result
}

impl ProbeModelsRequest {
    fn typed_key(&self) -> Option<&str> {
        self.api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }
}

/// The connection a probe lists models with: the request's fields over the
/// saved connection's, if any. A typed key gets its own transient secret.
fn probe_connection(
    config_manager: &Mutex<ConfigManager>,
    request: &ProbeModelsRequest,
    probe_id: String,
) -> Result<ProviderConnection, String> {
    let existing = match request
        .connection_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(connection_id) => Some(
            config_manager
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?
                .get_provider_connection(connection_id)
                .ok_or_else(|| format!("Provider connection not found: {}", connection_id))?,
        ),
        None => None,
    };

    let brand_id = request
        .provider_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| existing.as_ref().map(|conn| conn.provider_id.clone()))
        .unwrap_or_else(|| request.protocol_id.clone());
    let base_url =
        trimmed(request.base_url.clone()).or_else(|| existing.as_ref()?.base_url.clone());
    check_brand_protocol(&brand_id, &request.protocol_id)?;
    check_base_url(&brand_id, base_url.as_deref(), existing.as_ref())?;

    let probe_secret_ref = format!("provider-probe::{probe_id}");
    let name = "probe".to_string();
    let protocol_id = request.protocol_id.clone();
    // CLI connections return before this point (static model lists), and
    // API adapters spawn no process, so a probe carries no env.
    let connection = match existing {
        Some(existing) => ProviderConnection {
            name,
            protocol_id,
            provider_id: brand_id,
            base_url,
            secret_ref: if request.typed_key().is_some() {
                probe_secret_ref
            } else {
                existing.secret_ref
            },
            enabled: true,
            env: Vec::new(),
            ..existing
        },
        None => ProviderConnection {
            id: probe_id,
            name,
            protocol_id,
            provider_id: brand_id,
            auth_mode: AuthMode::DeveloperApiKey,
            base_url,
            secret_ref: probe_secret_ref,
            model_id: String::new(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
            env: Vec::new(),
        },
    };
    Ok(connection)
}

#[tauri::command]
pub async fn provider_connection_create(
    request: CreateProviderConnectionRequest,
    state: State<'_, AppState>,
) -> Result<ProviderConnection, String> {
    let id = uuid::Uuid::new_v4().to_string();
    create_connection(&state.config_manager, request, id)
}

fn create_connection(
    config_manager: &Mutex<ConfigManager>,
    request: CreateProviderConnectionRequest,
    id: String,
) -> Result<ProviderConnection, String> {
    let descriptor = providers::get_provider_descriptor(&request.protocol_id)
        .ok_or_else(|| format!("Unsupported provider: {}", request.protocol_id))?;

    if !descriptor.supported_auth_modes.contains(
        &request
            .auth_mode
            .clone()
            .unwrap_or(AuthMode::DeveloperApiKey),
    ) {
        return Err(format!(
            "Provider '{}' does not support the requested auth mode",
            request.protocol_id
        ));
    }

    let auth_mode = request.auth_mode.unwrap_or(AuthMode::DeveloperApiKey);
    let secret_ref = format!("provider-connection::{}", id);
    let brand_id = request
        .provider_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| request.protocol_id.clone());
    let base_url = trimmed(request.base_url);
    check_brand_protocol(&brand_id, &request.protocol_id)?;
    check_base_url(&brand_id, base_url.as_deref(), None)?;
    // Keyless self-hosted catalog entries (ollama / LM Studio / vLLM) may be
    // created without an API key; store one only when provided.
    let requires_key = catalog::requires_api_key(&brand_id);
    if auth_mode == AuthMode::DeveloperApiKey {
        let api_key = request
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match api_key {
            Some(key) => ProviderSecretStorage::set_secret(&secret_ref, key)
                .map_err(|e| format!("Failed to store provider credential: {}", e))?,
            None if requires_key => return Err("API key is required for this provider".to_string()),
            None => { /* keyless: no secret stored */ }
        }
    }

    let env = store_connection_env(&id, &request.protocol_id, request.env, &[])?;
    let now = chrono::Utc::now().timestamp_millis();
    let connection = ProviderConnection {
        id,
        name: request.name.trim().to_string(),
        protocol_id: request.protocol_id,
        provider_id: brand_id,
        auth_mode,
        base_url,
        secret_ref,
        model_id: request.model_id.trim().to_string(),
        account_label: request
            .account_label
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        enabled: true,
        created_at: now,
        updated_at: now,
        env,
    };
    config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .add_provider_connection(connection.clone())
        .map_err(|e| format!("Failed to create provider connection: {}", e))?;
    Ok(connection)
}

const BASE_URL_REQUIRED: &str = "A base URL is required for this provider";

/// A catalog brand without a default base (the custom-* entries) needs one:
/// without it, its key would go to the protocol's default host. A connection
/// saved without one before this rule keeps working under its brand.
fn check_base_url(
    brand_id: &str,
    base_url: Option<&str>,
    existing: Option<&ProviderConnection>,
) -> Result<(), String> {
    let requires_base_url =
        catalog::get_entry(brand_id).is_some_and(|entry| entry.default_base_url.is_none());
    let saved_without_base =
        existing.is_some_and(|conn| conn.base_url.is_none() && conn.provider_id == brand_id);
    if base_url.is_none() && requires_base_url && !saved_without_base {
        return Err(BASE_URL_REQUIRED.to_string());
    }
    Ok(())
}

/// A catalog brand speaks one protocol: under another, its key would go to
/// the brand's host in a wire format it does not serve, or to another host.
fn check_brand_protocol(brand_id: &str, protocol_id: &str) -> Result<(), String> {
    match catalog::get_entry(brand_id) {
        Some(entry) if entry.protocol_id != protocol_id => Err(format!(
            "Provider '{brand_id}' uses the '{}' protocol, not '{protocol_id}'",
            entry.protocol_id
        )),
        _ => Ok(()),
    }
}

fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Validates a connection's env vars and moves secret values into the vault,
/// returning the entries to persist. Only CLI connections take env: API
/// adapters spawn no process, so env there would be stored and never used.
fn store_connection_env(
    connection_id: &str,
    protocol_id: &str,
    env: Vec<McpEnvVar>,
    previous: &[McpEnvVar],
) -> Result<Vec<McpEnvVar>, String> {
    if env.is_empty() {
        return Ok(env);
    }
    if !providers::is_cli_provider(protocol_id) {
        return Err(format!(
            "Environment variables are supported only for CLI providers, not '{protocol_id}'"
        ));
    }
    validate_env_vars(&env)?;
    cli::connection_env_secrets(connection_id).store(env, previous)
}

#[tauri::command]
pub async fn provider_connection_update(
    request: UpdateProviderConnectionRequest,
    state: State<'_, AppState>,
) -> Result<ProviderConnection, String> {
    update_connection(&state.config_manager, request)
}

fn update_connection(
    config_manager: &Mutex<ConfigManager>,
    request: UpdateProviderConnectionRequest,
) -> Result<ProviderConnection, String> {
    let existing = config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connection(&request.id)
        .ok_or_else(|| format!("Provider connection not found: {}", request.id))?;
    // Sessions, models and env belong to the connection's CLI or API.
    if request.protocol_id != existing.protocol_id {
        return Err("A connection's provider cannot be changed; add a new connection".to_string());
    }

    let descriptor = providers::get_provider_descriptor(&request.protocol_id)
        .ok_or_else(|| format!("Unsupported provider: {}", request.protocol_id))?;

    if !descriptor.supported_auth_modes.contains(
        &request
            .auth_mode
            .clone()
            .unwrap_or(existing.auth_mode.clone()),
    ) {
        return Err(format!(
            "Provider '{}' does not support the requested auth mode",
            request.protocol_id
        ));
    }

    let brand_id = request
        .provider_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if existing.provider_id.is_empty() {
                request.protocol_id.clone()
            } else {
                existing.provider_id.clone()
            }
        });
    // A row hand-edited to a brand of another protocol keeps saving as is.
    if brand_id != existing.provider_id {
        check_brand_protocol(&brand_id, &request.protocol_id)?;
    }
    let base_url = trimmed(request.base_url);
    check_base_url(&brand_id, base_url.as_deref(), Some(&existing))?;

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

    let env = store_connection_env(
        &request.id,
        &request.protocol_id,
        request.env.unwrap_or_else(|| existing.env.clone()),
        &existing.env,
    )?;
    let updated = ProviderConnection {
        id: request.id,
        name: request.name.trim().to_string(),
        protocol_id: request.protocol_id,
        provider_id: brand_id,
        auth_mode,
        base_url,
        secret_ref: existing.secret_ref,
        model_id: request.model_id.trim().to_string(),
        account_label: request
            .account_label
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        enabled: request.enabled,
        created_at: existing.created_at,
        updated_at: chrono::Utc::now().timestamp_millis(),
        env,
    };
    config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .update_provider_connection(updated.clone())
        .map_err(|e| format!("Failed to update provider connection: {}", e))?;
    cli::connection_env_secrets(&updated.id).clear_orphaned(&existing.env, &updated.env);
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
        if let Some(agent) = config.main_agent.filter(|agent| {
            agent
                .provider_connection_ids
                .iter()
                .any(|value| value == &id)
        }) {
            dependents.push(agent.name);
        }
    }
    // Shared teammates depend on connections too, and they are not reachable
    // from any single workspace.
    let definitions = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get()
        .agent_definitions;
    for definition in definitions {
        if definition
            .behavior
            .provider_connection_ids
            .iter()
            .any(|value| value == &id)
        {
            dependents.push(definition.behavior.name);
        }
    }

    if !dependents.is_empty() {
        return Err(format!(
            "Cannot delete provider connection while used by {} agent(s)",
            dependents.len()
        ));
    }

    remove_connection(&state.config_manager, &id)
}

/// Removes the connection from config, then its secrets from the vault, so a
/// failed removal leaves a connection that still authenticates.
fn remove_connection(config_manager: &Mutex<ConfigManager>, id: &str) -> Result<bool, String> {
    let (connection, removed) = {
        let config_manager = config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let connection = config_manager.get_provider_connection(id);
        let removed = config_manager
            .remove_provider_connection(id)
            .map_err(|e| format!("Failed to delete provider connection: {}", e))?;
        (connection, removed)
    };
    if let Some(connection) = connection {
        if let Err(error) = ProviderSecretStorage::clear_secret(&connection.secret_ref) {
            tracing::warn!(
                %error,
                connection_id = %connection.id,
                "Failed to clear a deleted provider connection's credential"
            );
        }
        cli::connection_env_secrets(&connection.id).clear(&connection.env);
    }
    Ok(removed)
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

/// Last-4 identification hint for ONE connection's stored API key (design
/// D12, mirroring the MCP secret hints). Looked up lazily when the user
/// opens a connection for editing — never for the whole list, so opening
/// Settings does not fan out a keyring/DBus read per connection.
#[tauri::command]
pub async fn provider_connection_secret_hint(
    id: String,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    // Copy the connection out, then read the vault OUTSIDE the config lock
    // (keyring reads can be slow; never hold the lock across them).
    let connection = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connection(&id);
    let Some(connection) = connection else {
        return Ok(None);
    };
    if connection.auth_mode == AuthMode::SubscriptionLogin {
        return Ok(None); // CLI login — no stored API key.
    }
    Ok(ProviderSecretStorage::get_secret(&connection.secret_ref)
        .ok()
        .flatten()
        .as_deref()
        .and_then(crate::assistant::auth::secret_hint))
}

#[tauri::command]
pub async fn provider_descriptor_models(provider_id: String) -> Result<Vec<ModelInfo>, String> {
    cli::models_for_provider(&provider_id)
        .ok_or_else(|| format!("No static model list for provider '{}'", provider_id))
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

    test_api_provider_connection(&connection).await
}

#[allow(
    clippy::cognitive_complexity,
    reason = "lint debt: cognitive complexity 65 against a budget of 25"
)]
#[expect(
    clippy::too_many_lines,
    reason = "lint debt: 148 lines against a 100-line budget; split it, do not raise the budget"
)]
async fn test_api_provider_connection(
    connection: &ProviderConnection,
) -> Result<TestResult, String> {
    tracing::info!(
        connection_id = %connection.id,
        connection_name = %connection.name,
        protocol_id = %connection.protocol_id,
        model_id = %connection.model_id,
        base_url = ?connection.base_url,
        auth_mode = ?connection.auth_mode,
        "Testing provider connection"
    );

    if providers::is_cli_provider(&connection.protocol_id) {
        return test_cli_provider_connection(connection).await;
    }

    let adapter = providers::resolve_connection_adapter(connection).map_err(|e| {
        tracing::error!(
            protocol_id = %connection.protocol_id,
            error = %e,
            "Failed to resolve provider adapter"
        );
        e.to_string()
    })?;

    if let Err(error) = adapter.validate_connection(connection).await {
        return Ok(TestResult {
            success: false,
            error: Some(format!("{}: {error}", connection.name)),
        });
    }

    // Try a live model-list call first; if that fails (some providers don't
    // implement it), fall back to a minimal completion request to verify auth
    // and connectivity. Catalog entries with no live model endpoint skip this
    // so the test cannot pass solely from a curated static list.
    let has_live_models_endpoint = catalog::entry_for_connection(connection)
        .map(|entry| {
            !matches!(
                entry.models_endpoint_style,
                catalog::ModelsEndpointStyle::None
            )
        })
        .unwrap_or(true);
    if has_live_models_endpoint {
        match adapter.list_models(connection).await {
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
    } else {
        tracing::info!(
            connection_id = %connection.id,
            provider_id = %connection.provider_id,
            "Skipping model-list test for provider without a live models endpoint"
        );
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
        images: Default::default(),
    };

    match adapter.stream_completion(connection, request).await {
        Ok(mut stream) => {
            let mut got_response = false;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ProviderEvent::TextDelta { .. }) | Ok(ProviderEvent::MessageComplete) => {
                        got_response = true;
                        break;
                    }
                    Ok(ProviderEvent::ProviderError { failure }) => {
                        let message = failure.to_string();
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
                protocol_id = %connection.protocol_id,
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
    let args: &[&str] = match connection.protocol_id.as_str() {
        cli::CLAUDE_CODE_PROVIDER_ID => &["auth", "status"],
        cli::CODEX_PROVIDER_ID => &["login", "status"],
        _ => &["--version"],
    };
    // The same command builder the runs use (binary resolution, host hop,
    // the connection's env), so a passing test reflects the account and
    // binary a run will use.
    let (mut command, binary) = match cli::cli_command(connection, &[], None) {
        Ok(built) => built,
        Err(error) => {
            return Ok(TestResult {
                success: false,
                error: Some(error),
            })
        }
    };
    let output = command
        .args(args)
        .output()
        .await
        .map_err(|e| format!("Failed to run `{binary}`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(TestResult {
            success: false,
            error: Some(if stderr.is_empty() { stdout } else { stderr }),
        });
    }

    if connection.protocol_id == cli::CLAUDE_CODE_PROVIDER_ID {
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
    } else if connection.protocol_id == cli::CODEX_PROVIDER_ID {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.contains("Logged in") {
            return Ok(TestResult {
                success: false,
                error: Some("Codex CLI is installed but not logged in".to_string()),
            });
        }
    }

    Ok(TestResult {
        success: true,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::auth::ProviderSecretStorage;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const VALID_KEY: &str = "sk-or-valid-regression-key";

    /// A local OpenRouter stand-in reproducing the endpoint behavior behind
    /// issue #242: `GET /v1/models` answers 200 WITHOUT credentials, while
    /// the key endpoint is authenticated. Testing the command against this
    /// server proves a missing or wrong key can no longer produce a false
    /// success from the model listing.
    async fn fake_openrouter() -> (String, Arc<AtomicUsize>) {
        let models_hits = Arc::new(AtomicUsize::new(0));
        let hits = models_hits.clone();
        let router = Router::new()
            .route(
                "/v1/key",
                get(|headers: HeaderMap| async move {
                    let authorized = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|value| value == format!("Bearer {VALID_KEY}"));
                    if authorized {
                        StatusCode::OK
                    } else {
                        StatusCode::UNAUTHORIZED
                    }
                }),
            )
            .route(
                "/v1/models",
                get(move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({"data": [{"id": "openrouter/auto"}]}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{addr}"), models_hits)
    }

    fn openrouter_connection(id: &str, base_url: String) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            name: "OpenRouter (regression)".into(),
            protocol_id: "openai".into(),
            provider_id: "openrouter".into(),
            auth_mode: AuthMode::DeveloperApiKey,
            base_url: Some(base_url),
            secret_ref: format!("provider-connection::{id}"),
            model_id: "openrouter/auto".into(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
            env: Vec::new(),
        }
    }

    fn env_var(key: &str, value: &str, secret: bool) -> McpEnvVar {
        McpEnvVar {
            key: key.to_string(),
            value: Some(value.to_string()),
            secret_ref: None,
            secret,
        }
    }

    fn cli_connection(id: &str, protocol_id: &str, env: Vec<McpEnvVar>) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            name: "CLI".into(),
            protocol_id: protocol_id.into(),
            provider_id: protocol_id.into(),
            auth_mode: AuthMode::SubscriptionLogin,
            base_url: None,
            secret_ref: format!("provider-connection::{id}"),
            model_id: "default".into(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
            env,
        }
    }

    /// A secret env value goes to the vault; the saved connection, which is
    /// both what config persists and what the commands return, holds only
    /// its reference.
    #[test]
    fn saved_connection_env_keeps_secret_values_out_of_config_and_responses() {
        let id = "env-save-secret";
        let env = store_connection_env(
            id,
            cli::CLAUDE_CODE_PROVIDER_ID,
            vec![
                env_var("CLAUDE_CONFIG_DIR", "claude-work", false),
                env_var("ANTHROPIC_API_KEY", "sk-secret-value", true),
            ],
            &[],
        )
        .expect("valid env saves");
        let connection = cli_connection(id, cli::CLAUDE_CODE_PROVIDER_ID, env);

        let json = serde_json::to_string(&connection).unwrap();
        assert!(!json.contains("sk-secret-value"), "{json}");
        assert!(json.contains("claude-work"), "{json}");
        let secret_ref = format!("provider-connection::{id}::env::ANTHROPIC_API_KEY");
        assert_eq!(
            ProviderSecretStorage::get_secret(&secret_ref)
                .unwrap()
                .as_deref(),
            Some("sk-secret-value")
        );
        ProviderSecretStorage::clear_secret(&secret_ref).unwrap();
    }

    #[test]
    fn saving_env_rejects_api_connections_and_invalid_keys() {
        let error = store_connection_env("env-api", "openai", vec![env_var("A", "1", false)], &[])
            .expect_err("API connections spawn no process");
        assert!(error.contains("only for CLI providers"), "{error}");
        assert_eq!(
            store_connection_env("env-api", "openai", Vec::new(), &[]),
            Ok(Vec::new())
        );

        let error = store_connection_env(
            "env-dup",
            cli::CODEX_PROVIDER_ID,
            vec![env_var("A", "1", false), env_var("A", "2", false)],
            &[],
        )
        .expect_err("duplicate keys");
        assert!(error.contains("more than once"), "{error}");
    }

    fn config_with(
        connection: &ProviderConnection,
        config_path: std::path::PathBuf,
    ) -> Mutex<ConfigManager> {
        let config = crate::AppConfig {
            provider_connections: vec![connection.clone()],
            ..Default::default()
        };
        Mutex::new(ConfigManager::new_for_tests(config, config_path))
    }

    fn stored(secret_ref: &str) -> Option<String> {
        ProviderSecretStorage::get_secret(secret_ref).unwrap()
    }

    fn update_request(
        connection: &ProviderConnection,
        env: Option<Vec<McpEnvVar>>,
    ) -> UpdateProviderConnectionRequest {
        UpdateProviderConnectionRequest {
            id: connection.id.clone(),
            name: connection.name.clone(),
            protocol_id: connection.protocol_id.clone(),
            provider_id: None,
            api_key: None,
            auth_mode: None,
            base_url: None,
            model_id: connection.model_id.clone(),
            account_label: None,
            enabled: true,
            env,
        }
    }

    /// An update that omits env keeps it; one that replaces it removes the
    /// dropped secrets from the vault.
    #[test]
    fn update_keeps_omitted_env_and_clears_dropped_secrets() {
        let id = "env-update";
        let env = store_connection_env(
            id,
            cli::CLAUDE_CODE_PROVIDER_ID,
            vec![
                env_var("CLAUDE_CONFIG_DIR", "work", false),
                env_var("TOKEN", "secret-value", true),
            ],
            &[],
        )
        .unwrap();
        let connection = cli_connection(id, cli::CLAUDE_CODE_PROVIDER_ID, env.clone());
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(&connection, dir.path().join("config.json"));
        let request = |env| update_request(&connection, env);
        let token_ref = format!("provider-connection::{id}::env::TOKEN");

        let kept = update_connection(&config, request(None)).unwrap();
        assert_eq!(kept.env, env);
        assert_eq!(stored(&token_ref).as_deref(), Some("secret-value"));

        let personal = vec![env_var("CLAUDE_CONFIG_DIR", "personal", false)];
        let replaced = update_connection(&config, request(Some(personal.clone()))).unwrap();
        assert_eq!(replaced.env, personal);
        assert_eq!(stored(&token_ref), None);
        let saved = config.lock().unwrap().get_provider_connection(id).unwrap();
        assert_eq!(saved.env, personal);
    }

    /// The editor sends a stored secret back as `{key, secret: true}` with no
    /// value. Under the key it was stored with, that keeps the stored value;
    /// under any other key there is nothing to keep, so the update fails and
    /// changes nothing.
    #[test]
    fn update_with_a_valueless_secret_keeps_only_a_stored_key() {
        let id = "env-update-valueless";
        let env = store_connection_env(
            id,
            cli::CLAUDE_CODE_PROVIDER_ID,
            vec![env_var("TOKEN", "secret-value", true)],
            &[],
        )
        .unwrap();
        let connection = cli_connection(id, cli::CLAUDE_CODE_PROVIDER_ID, env.clone());
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(&connection, dir.path().join("config.json"));
        let request = |key: &str| {
            let var = serde_json::json!({ "key": key, "secret": true });
            update_request(
                &connection,
                Some(vec![serde_json::from_value(var).unwrap()]),
            )
        };
        let token_ref = format!("provider-connection::{id}::env::TOKEN");

        let error = update_connection(&config, request("NEW")).expect_err("nothing stored");
        assert!(error.contains("`NEW`"), "{error}");
        let saved = config.lock().unwrap().get_provider_connection(id).unwrap();
        assert_eq!(saved.env, env);
        assert_eq!(stored(&token_ref).as_deref(), Some("secret-value"));

        let kept = update_connection(&config, request("TOKEN")).unwrap();
        assert_eq!(kept.env, env);
        assert_eq!(stored(&token_ref).as_deref(), Some("secret-value"));
    }

    /// Update clears dropped secrets only once config no longer references
    /// them: a failed save leaves a connection whose secrets still resolve.
    #[test]
    fn update_clears_dropped_secrets_only_after_saving() {
        let id = "env-update-unsaved";
        let env = store_connection_env(
            id,
            cli::CLAUDE_CODE_PROVIDER_ID,
            vec![env_var("TOKEN", "secret-value", true)],
            &[],
        )
        .unwrap();
        let connection = cli_connection(id, cli::CLAUDE_CODE_PROVIDER_ID, env);
        let token_ref = format!("provider-connection::{id}::env::TOKEN");
        let dir = tempfile::tempdir().unwrap();

        // Config cannot be saved where its directory does not exist.
        let unsaveable = config_with(&connection, dir.path().join("missing").join("config.json"));
        let dropped = update_request(&connection, Some(Vec::new()));
        assert!(update_connection(&unsaveable, dropped).is_err());
        assert_eq!(stored(&token_ref).as_deref(), Some("secret-value"));
        ProviderSecretStorage::clear_secret(&token_ref).unwrap();
    }

    /// A connection keeps its provider: its CLI session, models and env
    /// mean nothing to another one.
    #[test]
    fn update_rejects_a_provider_change() {
        let connection = cli_connection(
            "env-update-protocol",
            cli::CLAUDE_CODE_PROVIDER_ID,
            Vec::new(),
        );
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(&connection, dir.path().join("config.json"));
        let mut request = update_request(&connection, None);
        request.protocol_id = cli::CODEX_PROVIDER_ID.to_string();

        let error = update_connection(&config, request).expect_err("provider change");
        assert!(error.contains("cannot be changed"), "{error}");
        let saved = config
            .lock()
            .unwrap()
            .get_provider_connection(&connection.id)
            .unwrap();
        assert_eq!(saved.protocol_id, cli::CLAUDE_CODE_PROVIDER_ID);
    }

    fn create_request(
        protocol_id: &str,
        provider_id: &str,
        base_url: Option<&str>,
    ) -> CreateProviderConnectionRequest {
        CreateProviderConnectionRequest {
            name: "Custom".into(),
            protocol_id: protocol_id.into(),
            provider_id: Some(provider_id.into()),
            api_key: Some("sk-custom".into()),
            auth_mode: None,
            base_url: base_url.map(Into::into),
            model_id: "m".into(),
            account_label: None,
            env: Vec::new(),
        }
    }

    fn empty_config(dir: &tempfile::TempDir) -> Mutex<ConfigManager> {
        Mutex::new(ConfigManager::new_for_tests(
            crate::AppConfig::default(),
            dir.path().join("config.json"),
        ))
    }

    /// A custom brand has no catalog base, so without its own URL its key
    /// would go to the protocol's default host.
    #[test]
    fn create_rejects_a_custom_brand_without_a_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let config = empty_config(&dir);
        let cases = [
            ("anthropic", "custom-anthropic", None),
            ("anthropic", "custom-anthropic", Some("  ")),
            ("openai", "custom-openai", None),
        ];
        for (protocol_id, brand, base_url) in cases {
            let id = "custom-create-rejected";
            let error = create_connection(
                &config,
                create_request(protocol_id, brand, base_url),
                id.into(),
            )
            .expect_err("custom brand without a base URL");
            assert_eq!(error, BASE_URL_REQUIRED);
            assert_eq!(stored(&format!("provider-connection::{id}")), None);
        }
        assert!(config.lock().unwrap().get_provider_connections().is_empty());

        let created = create_connection(
            &config,
            create_request("anthropic", "minimax", None),
            "minimax-create".into(),
        )
        .unwrap();
        assert_eq!(created.base_url, None, "the catalog base applies");
        let created = create_connection(
            &config,
            create_request(
                "anthropic",
                "custom-anthropic",
                Some(" https://llm.example.com "),
            ),
            "custom-create".into(),
        )
        .unwrap();
        assert_eq!(created.base_url.as_deref(), Some("https://llm.example.com"));
    }

    fn custom_connection(id: &str, base_url: Option<&str>) -> ProviderConnection {
        let mut connection = openrouter_connection(id, String::new());
        connection.protocol_id = "anthropic".into();
        connection.provider_id = "custom-anthropic".into();
        connection.base_url = base_url.map(Into::into);
        connection
    }

    #[test]
    fn update_rejects_clearing_a_custom_base_url() {
        let connection = custom_connection("custom-clear", Some("https://llm.example.com"));
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(&connection, dir.path().join("config.json"));
        let mut request = update_request(&connection, None);
        request.api_key = Some("sk-new".into());

        let error = update_connection(&config, request).expect_err("base URL cleared");
        assert_eq!(error, BASE_URL_REQUIRED);
        let saved = config
            .lock()
            .unwrap()
            .get_provider_connection(&connection.id)
            .unwrap();
        assert_eq!(saved.base_url, connection.base_url);
        assert_eq!(stored(&connection.secret_ref), None, "key not written");
    }

    /// Connections saved without a base before the rule keep saving.
    #[test]
    fn update_keeps_a_custom_connection_saved_without_a_base_url() {
        let connection = custom_connection("custom-legacy", None);
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(&connection, dir.path().join("config.json"));
        let mut request = update_request(&connection, None);
        request.name = "Renamed".into();

        let updated = update_connection(&config, request).expect("legacy connection updates");
        assert_eq!(updated.name, "Renamed");
        assert_eq!(updated.base_url, None);
    }

    /// The legacy exemption is for the brand the connection was saved as:
    /// a URL-less MiniMax connection turned custom would send its key to
    /// Anthropic.
    #[test]
    fn update_rejects_switching_a_url_less_connection_to_a_custom_brand() {
        let mut connection = custom_connection("minimax-to-custom", None);
        connection.provider_id = "minimax".into();
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(&connection, dir.path().join("config.json"));
        let mut request = update_request(&connection, None);
        request.provider_id = Some("custom-anthropic".into());
        request.api_key = Some("sk-new".into());

        let error = update_connection(&config, request).expect_err("custom brand without a URL");
        assert_eq!(error, BASE_URL_REQUIRED);
        assert_eq!(stored(&connection.secret_ref), None, "key not written");
    }

    /// Listing models for a custom brand without a URL would send the typed
    /// key to the protocol's default host.
    #[tokio::test]
    async fn probe_rejects_a_custom_brand_without_a_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let config = empty_config(&dir);
        let request = ProbeModelsRequest {
            protocol_id: "anthropic".into(),
            connection_id: None,
            provider_id: Some("custom-anthropic".into()),
            base_url: None,
            api_key: Some("sk-typed".into()),
        };

        let error = probe_models(&config, request, "custom-probe".into())
            .await
            .expect_err("custom brand without a URL");
        assert_eq!(error, BASE_URL_REQUIRED);
        assert_eq!(
            stored("provider-probe::custom-probe"),
            None,
            "key not written"
        );
    }

    fn probe_request(connection_id: &str, api_key: Option<&str>) -> ProbeModelsRequest {
        ProbeModelsRequest {
            protocol_id: "anthropic".into(),
            connection_id: Some(connection_id.into()),
            provider_id: None,
            base_url: None,
            api_key: api_key.map(Into::into),
        }
    }

    /// "Load models" on a saved connection probes with its stored base URL
    /// and key; a legacy custom connection saved without a URL still probes.
    #[test]
    fn probe_uses_the_saved_base_url_and_key() {
        let dir = tempfile::tempdir().unwrap();
        for base_url in [Some("https://llm.example.com"), None] {
            let saved = custom_connection("custom-probe-saved", base_url);
            let config = config_with(&saved, dir.path().join("config.json"));

            let probe = probe_connection(&config, &probe_request(&saved.id, None), "p".into())
                .expect("saved connection probes");
            assert_eq!(probe.base_url, saved.base_url);
            assert_eq!(probe.secret_ref, saved.secret_ref);

            let probe = probe_connection(
                &config,
                &probe_request(&saved.id, Some("sk-typed")),
                "p".into(),
            )
            .unwrap();
            assert_eq!(
                probe.secret_ref, "provider-probe::p",
                "a typed key is transient"
            );
        }
    }

    /// A brand under another protocol would send its key elsewhere: MiniMax
    /// over the OpenAI protocol resolves to MiniMax's Anthropic base.
    #[tokio::test]
    async fn create_and_probe_reject_a_brand_of_another_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let config = empty_config(&dir);
        let error = create_connection(
            &config,
            create_request("openai", "minimax", None),
            "minimax-over-openai".into(),
        )
        .expect_err("brand of another protocol");
        assert!(error.contains("'anthropic' protocol"), "{error}");
        assert_eq!(stored("provider-connection::minimax-over-openai"), None);
        assert!(config.lock().unwrap().get_provider_connections().is_empty());

        let request = ProbeModelsRequest {
            protocol_id: "openai".into(),
            connection_id: None,
            provider_id: Some("minimax".into()),
            base_url: None,
            api_key: Some("sk-typed".into()),
        };
        let error = probe_models(&config, request, "minimax-probe".into())
            .await
            .expect_err("brand of another protocol");
        assert!(error.contains("'anthropic' protocol"), "{error}");
        assert_eq!(stored("provider-probe::minimax-probe"), None);
    }

    /// Switching a saved connection to a brand of another protocol would
    /// send its key to that brand's host; a row already saved that way (by
    /// hand) keeps saving under its brand.
    #[test]
    fn update_rejects_switching_to_a_brand_of_another_protocol() {
        let connection = openrouter_connection("openai-to-minimax", String::new());
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(&connection, dir.path().join("config.json"));
        let mut request = update_request(&connection, None);
        request.provider_id = Some("minimax".into());
        request.api_key = Some("sk-new".into());

        let error = update_connection(&config, request).expect_err("brand of another protocol");
        assert!(error.contains("'anthropic' protocol"), "{error}");
        assert_eq!(stored(&connection.secret_ref), None, "key not written");
        let saved = config
            .lock()
            .unwrap()
            .get_provider_connection(&connection.id)
            .unwrap();
        assert_eq!(saved.provider_id, "openrouter");

        let mut hand_edited = connection.clone();
        hand_edited.id = "hand-edited-minimax".into();
        hand_edited.provider_id = "minimax".into();
        let config = config_with(&hand_edited, dir.path().join("config.json"));
        let mut request = update_request(&hand_edited, None);
        request.provider_id = Some("minimax".into());
        request.name = "Renamed".into();
        let updated = update_connection(&config, request).expect("unchanged brand saves");
        assert_eq!(updated.name, "Renamed");
        assert_eq!(updated.provider_id, "minimax");
    }

    /// A typed key lives only for the probe, failed or not.
    #[tokio::test]
    async fn probe_clears_the_typed_key_after_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = empty_config(&dir);
        let request = ProbeModelsRequest {
            protocol_id: "no-such-protocol".into(),
            connection_id: None,
            provider_id: None,
            base_url: None,
            api_key: Some("sk-typed".into()),
        };

        probe_models(&config, request, "unresolved-probe".into())
            .await
            .expect_err("no adapter for the protocol");
        assert_eq!(stored("provider-probe::unresolved-probe"), None);
    }

    /// Delete clears the connection's secrets only once config no longer
    /// holds it: a failed removal leaves a connection that still works.
    #[test]
    fn delete_clears_secrets_only_after_removing_the_connection() {
        let id = "env-delete";
        let env = store_connection_env(
            id,
            cli::CLAUDE_CODE_PROVIDER_ID,
            vec![env_var("TOKEN", "secret-value", true)],
            &[],
        )
        .unwrap();
        let connection = cli_connection(id, cli::CLAUDE_CODE_PROVIDER_ID, env);
        ProviderSecretStorage::set_secret(&connection.secret_ref, "api-key").unwrap();
        let token_ref = format!("provider-connection::{id}::env::TOKEN");
        let dir = tempfile::tempdir().unwrap();

        // Config cannot be saved where its directory does not exist.
        let unsaveable = config_with(&connection, dir.path().join("missing").join("config.json"));
        assert!(remove_connection(&unsaveable, id).is_err());
        assert_eq!(stored(&token_ref).as_deref(), Some("secret-value"));
        assert_eq!(stored(&connection.secret_ref).as_deref(), Some("api-key"));

        let config = config_with(&connection, dir.path().join("config.json"));
        assert_eq!(remove_connection(&config, id), Ok(true));
        assert_eq!(stored(&token_ref), None);
        assert_eq!(stored(&connection.secret_ref), None);
    }

    /// A secret sent without a value needs one stored under its key; a
    /// renamed secret has none, and saving says so instead of dropping it.
    #[test]
    fn saving_env_rejects_a_secret_with_nothing_stored_and_nul_values() {
        let mut renamed = env_var("RENAMED", "", true);
        renamed.value = None;
        let error = store_connection_env("env-renamed", cli::CODEX_PROVIDER_ID, vec![renamed], &[])
            .expect_err("nothing stored under the new key");
        assert!(error.contains("`RENAMED`"), "{error}");

        let error = store_connection_env(
            "env-nul",
            cli::CODEX_PROVIDER_ID,
            vec![env_var("DIR", "a\0B=1", false)],
            &[],
        )
        .expect_err("NUL would split the Flatpak env block");
        assert!(error.contains("NUL"), "{error}");
    }

    /// A secret the vault no longer holds fails the connection test, naming
    /// the key, rather than testing the CLI without it.
    #[tokio::test]
    async fn cli_connection_test_fails_when_a_secret_is_missing() {
        let mut missing = env_var("ANTHROPIC_API_KEY", "", true);
        missing.value = None;
        missing.secret_ref =
            Some("provider-connection::env-test-missing::env::ANTHROPIC_API_KEY".into());
        let connection = cli_connection(
            "env-test-missing",
            cli::CLAUDE_CODE_PROVIDER_ID,
            vec![missing],
        );

        let result = run_connection_test(connection).await;

        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(error.contains("`ANTHROPIC_API_KEY`"), "{error}");
    }

    /// The connection test runs the CLI with the connection's env, so it
    /// reports on the account a run will use. The stand-in `claude` reports
    /// "logged in" only when it sees the connection's config dir.
    #[cfg(unix)]
    #[tokio::test]
    async fn cli_connection_test_runs_with_the_connection_env() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("claude-work");
        let script = dir.path().join("claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$CLAUDE_CONFIG_DIR\" = '{}' ]; then echo '{{\"loggedIn\":true}}'; \
                 else echo '{{\"loggedIn\":false}}'; fi\n",
                config_dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let run = |env: Vec<McpEnvVar>| {
            let mut connection = cli_connection("env-test-path", cli::CLAUDE_CODE_PROVIDER_ID, env);
            connection.base_url = Some(script.to_string_lossy().into_owned());
            async move {
                // A freshly written script can be briefly busy (ETXTBSY) while
                // another test's fork still holds the write descriptor.
                for _ in 0..20 {
                    match test_api_provider_connection(&connection).await {
                        Err(error) if error.contains("Text file busy") => {
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                        result => return result.expect("the stand-in CLI runs"),
                    }
                }
                panic!("the stand-in CLI stayed busy");
            }
        };

        let without_env = run(Vec::new()).await;
        assert!(!without_env.success, "{:?}", without_env.error);

        let config_dir = config_dir.to_string_lossy().into_owned();
        let with_env = run(vec![env_var("CLAUDE_CONFIG_DIR", &config_dir, false)]).await;
        assert!(with_env.success, "{:?}", with_env.error);
    }

    async fn run_connection_test(connection: ProviderConnection) -> TestResult {
        test_api_provider_connection(&connection)
            .await
            .expect("connection test returns a result")
    }

    /// Regression test for #242: the OpenRouter key guard must reject a
    /// stored-but-invalid key BEFORE the model listing runs. Without the
    /// guard this test fails: the unauthenticated models endpoint answers
    /// 200 and the command reports success.
    #[tokio::test]
    async fn openrouter_test_fails_on_an_invalid_key_without_listing_models() {
        let (base_url, models_hits) = fake_openrouter().await;
        let connection = openrouter_connection("test-openrouter-invalid-key", base_url);
        ProviderSecretStorage::set_secret(&connection.secret_ref, "sk-or-invalid-key")
            .expect("stage invalid key");

        let result = run_connection_test(connection).await;

        assert!(
            !result.success,
            "an invalid OpenRouter key must fail the connection test"
        );
        let error = result.error.expect("failure carries an error message");
        assert!(
            error.starts_with(
                "OpenRouter (regression): provider request failed: \
                 HTTP 401: The provider rejected the API key."
            ),
            "unexpected error: {error}"
        );
        assert_eq!(
            models_hits.load(Ordering::SeqCst),
            0,
            "the unauthenticated model listing must not be reached"
        );
    }

    #[tokio::test]
    async fn openrouter_test_fails_on_a_missing_key_without_listing_models() {
        let (base_url, models_hits) = fake_openrouter().await;
        let connection = openrouter_connection("test-openrouter-missing-key", base_url);
        // No secret staged for this connection's secret_ref.

        let result = run_connection_test(connection).await;

        assert!(
            !result.success,
            "a missing OpenRouter key must fail the connection test"
        );
        assert_eq!(
            models_hits.load(Ordering::SeqCst),
            0,
            "the unauthenticated model listing must not be reached"
        );
    }

    #[tokio::test]
    async fn openrouter_test_succeeds_with_a_valid_key_via_the_model_listing() {
        let (base_url, models_hits) = fake_openrouter().await;
        let connection = openrouter_connection("test-openrouter-valid-key", base_url);
        ProviderSecretStorage::set_secret(&connection.secret_ref, VALID_KEY)
            .expect("stage valid key");

        let result = run_connection_test(connection).await;

        assert!(
            result.success,
            "a valid OpenRouter key must pass: {:?}",
            result.error
        );
        assert_eq!(
            models_hits.load(Ordering::SeqCst),
            1,
            "a valid key passes through exactly one model listing"
        );
    }
}

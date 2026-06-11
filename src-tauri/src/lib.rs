//! CLAI backend runtime.
//!
//! This is the Rust backend for the Tauri-based desktop application.
//! It handles:
//! - HTTP communication with provider services
//! - Exposing functionality to the JavaScript frontend
//! - Scheduled automations with MCP tool integration
//!
//! # Automation Architecture
//!
//! When an automation is enabled, the app runs it through the assistant engine:
//! 1. The scheduler picks the next ready automation
//! 2. The runner prepares a dedicated workspace tab and background session
//! 3. The assistant runtime executes a scheduled turn with built-in and MCP tools
//! 4. The scheduler records completion and schedules the next run
//!
//! # Rust Learning: Module System
//!
//! Rust organizes code into modules. This file (`lib.rs`) is the crate root
//! for library code. We declare submodules with `mod`, making their contents
//! available under `crate::module_name`.
//!
//! ```ignore
//! mod commands;  // Declares src/commands/mod.rs as a module
//!                // Contents accessible as crate::commands::*
//! ```

// Declare our modules
mod agents;
mod assistant;
mod commands;
mod config;
mod db;
mod mcp;
mod paths;
mod providers;
mod system_apps;
mod workspace_index;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use agents::SharedScheduler;
use tauri::Manager;
use tokio::sync::Mutex as AsyncMutex;

#[doc(hidden)]
pub use commands::workspace::workspace_agent_runtime_description;
#[doc(hidden)]
pub use config::{workspace_config, AppConfig, ConfigManager, SkillSourceConfig, WorkspaceConfig};
#[doc(hidden)]
pub use workspace_index::WorkspaceIndex;

/// Re-exports for integration tests. Not part of the public API —
/// the `#[doc(hidden)]` keeps them out of generated docs, and the
/// module's name signals intent at the use site.
#[doc(hidden)]
pub mod runtime {
    pub use crate::assistant::runtime::{cancel_run, register_run, unregister_run};
}

/// Shared application state accessible from all commands.
///
/// This struct holds all the shared resources that commands need access to.
/// It's registered with Tauri via `.manage()` and injected into commands
/// via `State<'_, AppState>`.
pub struct AppState {
    /// Configuration manager for providers, automations, and MCP servers.
    pub config_manager: Mutex<ConfigManager>,
    /// External MCP client registry.
    pub mcp_client_manager: AsyncMutex<mcp::client::McpClientManager>,
    /// In-memory MCP OAuth login sessions awaiting browser callback/finish.
    pub pending_mcp_oauth: AsyncMutex<mcp::oauth::McpOAuthSessionRegistry>,
    /// Agent scheduler (manages agent instances)
    pub scheduler: SharedScheduler,
    /// In-flight shell-permission approval requests awaiting user decision.
    pub pending_approvals: commands::permissions::PendingApprovals,
    /// In-flight filesystem path-grant requests awaiting user decision.
    pub pending_path_grants: commands::path_grants::PendingPathGrants,
    /// File-backed workspace discovery index.
    pub workspace_index: Arc<RwLock<WorkspaceIndex>>,
}

impl AppState {
    /// Construct application state for integration tests that exercise helper
    /// code without starting a Tauri app.
    #[doc(hidden)]
    pub fn new_for_tests(
        config_manager: ConfigManager,
        workspace_index: WorkspaceIndex,
    ) -> Result<Self, String> {
        let initial_config = config_manager.get();
        let mut mcp_client_manager = mcp::client::McpClientManager::new();
        mcp_client_manager.sync_from_config(&initial_config);

        Ok(Self {
            config_manager: Mutex::new(config_manager),
            mcp_client_manager: AsyncMutex::new(mcp_client_manager),
            pending_mcp_oauth: AsyncMutex::new(mcp::oauth::McpOAuthSessionRegistry::new()),
            scheduler: agents::create_shared_scheduler(),
            pending_approvals: commands::permissions::PendingApprovals::new(),
            pending_path_grants: commands::path_grants::PendingPathGrants::new(),
            workspace_index: Arc::new(RwLock::new(workspace_index)),
        })
    }

    pub fn workspace_root(&self, workspace_id: &str) -> Option<PathBuf> {
        self.workspace_index.read().ok()?.root(workspace_id)
    }

    pub fn workspace_create_target(&self, requested: Option<&Path>) -> Result<PathBuf, String> {
        let config = self
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?
            .get();
        let dirs = config.expanded_workspace_dirs();
        let base = match requested {
            Some(path) => {
                let expanded = paths::expand_tilde(path);
                if !dirs.iter().any(|dir| dir == &expanded) {
                    return Err(format!(
                        "Workspace directory is not configured: {}",
                        expanded.display()
                    ));
                }
                expanded
            }
            None if dirs.len() == 1 => dirs[0].clone(),
            None => {
                return Err("Workspace directory must be selected.".to_string());
            }
        };
        Ok(base)
    }

    pub async fn workspace_db(&self, workspace_id: &str) -> Result<db::DbPool, String> {
        if let Some(pool) = self
            .workspace_index
            .read()
            .map_err(|e| format!("Workspace index lock error: {}", e))?
            .pool(workspace_id)
        {
            return Ok(pool);
        }

        let root = self
            .workspace_root(workspace_id)
            .ok_or_else(|| format!("Workspace {} not found", workspace_id))?;
        let pool = db::init_workspace_db(&root).await?;
        self.workspace_index
            .write()
            .map_err(|e| format!("Workspace index lock error: {}", e))?
            .attach_pool(workspace_id.to_string(), pool.clone());
        Ok(pool)
    }
}

/// Entry point for the Tauri application.
///
/// # Rust Learning: Conditional Compilation
///
/// `#[cfg_attr(mobile, tauri::mobile_entry_point)]` applies the
/// `mobile_entry_point` attribute only when compiling for mobile.
/// This is how Rust handles platform-specific code.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize tracing (structured logging)
    // In development, this outputs to stderr with colors
    // RUST_LOG env var can control log levels (e.g., RUST_LOG=debug)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("CLAI starting up...");

    // Install the process-wide rustls CryptoProvider before any TLS handshake.
    // rmcp's HTTP MCP transport builds a reqwest 0.13 (rustls 0.23) client, and
    // rustls 0.23 panics with "No provider set" if no default provider is
    // installed — which crashed the worker the first time an HTTPS MCP server
    // (e.g. Netdata Cloud) was contacted, hanging tool discovery. `ring` is the
    // backend already in our dependency tree. Idempotent: ignore the Err that
    // signals a provider was already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Clear temp directory from previous runs (MCP configs, etc)
    agents::clear_tmp_dir();

    // Initialize config manager (loads config from disk or creates default)
    let config_manager = ConfigManager::new().expect(
        "Failed to initialize configuration manager. \
         Check that the config directory is accessible.",
    );

    // First-run convenience: if the user has no provider connections yet,
    // auto-detect supported CLI providers and register a connection for each
    // one found, so the app is usable without a manual trip to Settings. The
    // model is left empty so the CLI uses its own configured default. Runs
    // before the default-workspace bootstrap so that first workspace picks the
    // connection up.
    if config_manager.get().provider_connections.is_empty() {
        for provider_id in providers::detect_supported_cli_providers() {
            let now = chrono::Utc::now().timestamp_millis();
            let id = uuid::Uuid::new_v4().to_string();
            let name = crate::assistant::providers::registry::get_provider_descriptor(provider_id)
                .map(|d| d.display_name)
                .unwrap_or_else(|| provider_id.to_string());
            let connection = crate::assistant::types::ProviderConnection {
                secret_ref: format!("provider-connection::{}", id),
                id,
                name,
                provider_id: provider_id.to_string(),
                auth_mode: crate::assistant::types::AuthMode::SubscriptionLogin,
                base_url: None,
                model_id: String::new(),
                account_label: None,
                enabled: true,
                created_at: now,
                updated_at: now,
            };
            match config_manager.add_provider_connection(connection) {
                Ok(()) => tracing::info!(
                    provider_id,
                    "Auto-detected supported provider CLI; registered a connection"
                ),
                Err(e) => tracing::warn!(
                    provider_id,
                    error = %e,
                    "Failed to register auto-detected provider connection"
                ),
            }
        }
    }

    let initial_config = config_manager.get();
    let workspace_index = WorkspaceIndex::scan(&initial_config);

    // Initialize agent scheduler
    let scheduler = agents::create_shared_scheduler();

    // Register default automation definitions and restore enabled instances
    agents::init::initialize_scheduler(&scheduler, &config_manager);

    // Clone scheduler before moving into state (needed for runner and for
    // post-DB population from workspace_agents).
    let runner_scheduler = scheduler.clone();
    let post_db_scheduler = scheduler.clone();

    // Create the shared application state
    let state = AppState {
        config_manager: Mutex::new(config_manager),
        mcp_client_manager: AsyncMutex::new({
            let mut manager = mcp::client::McpClientManager::new();
            manager.sync_from_config(&initial_config);
            manager
        }),
        pending_mcp_oauth: AsyncMutex::new(mcp::oauth::McpOAuthSessionRegistry::new()),
        scheduler,
        pending_approvals: commands::permissions::PendingApprovals::new(),
        pending_path_grants: commands::path_grants::PendingPathGrants::new(),
        workspace_index: Arc::new(RwLock::new(workspace_index)),
    };

    // Build and run the Tauri application
    tauri::Builder::default()
        // Register Tauri plugins
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Make our state available to all commands
        // Commands can access it via State<'_, AppState>
        .manage(state)
        // Setup hook - runs after app is built, gives us AppHandle
        .setup(move |app| {
            // Keep the app-managed skills repository current without making
            // startup depend on network availability.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    if let Err(e) = commands::skills::sync_default_skill_source(state.inner()).await
                    {
                        tracing::warn!("Failed to sync default skill source: {}", e);
                    }
                }
            });

            // Open per-workspace DBs and populate the scheduler.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    if let Err(e) = initialize_workspace_storage(&state).await {
                        tracing::warn!("Failed to initialize workspace storage: {}", e);
                    }
                    agents::init::populate_scheduler_from_workspace_agents(
                        &post_db_scheduler,
                        state.inner(),
                    )
                    .await;
                }
            });

            // Start the agent runner background task
            let app_handle = app.handle().clone();
            agents::start_agent_runner(app_handle, runner_scheduler);
            Ok(())
        })
        // Register our custom commands
        // These become available to JS via invoke()
        .invoke_handler(tauri::generate_handler![
            // App metadata
            commands::app_info::app_version_detail,
            // Assistant runtime commands
            commands::assistant::assistant_create_session,
            commands::assistant::assistant_get_session,
            commands::assistant::assistant_list_sessions,
            commands::assistant::assistant_delete_session,
            commands::assistant::assistant_load_session_messages,
            commands::assistant::assistant_load_session_messages_page,
            commands::assistant::assistant_list_runs,
            commands::assistant::assistant_list_tool_calls,
            commands::assistant::assistant_send_message,
            commands::assistant::assistant_compact_session,
            commands::assistant::assistant_delete_queued_message,
            commands::assistant::assistant_edit_queued_message,
            commands::assistant::assistant_submit_user_input,
            commands::assistant::assistant_retry_run,
            commands::assistant::assistant_cancel_run,
            // Bundled agent templates (read-only).
            commands::agent_templates::agent_templates_list,
            // MCP server commands
            commands::mcp_servers::get_mcp_servers,
            commands::mcp_servers::get_mcp_server,
            commands::mcp_servers::get_mcp_server_catalog,
            commands::mcp_servers::create_mcp_server,
            commands::mcp_servers::update_mcp_server,
            commands::mcp_servers::delete_mcp_server,
            commands::mcp_servers::start_mcp_oauth_login,
            commands::mcp_servers::finish_mcp_oauth_login,
            commands::mcp_servers::cancel_mcp_oauth_login,
            commands::mcp_servers::disconnect_mcp_oauth,
            // Provider commands
            commands::provider::get_ai_provider,
            commands::provider::set_ai_provider,
            commands::provider::clear_ai_provider,
            commands::provider::get_available_ai_providers,
            commands::provider::validate_ai_provider,
            commands::provider::get_provider_models,
            // Provider connection commands for app-owned assistant runtime
            commands::provider_connections::provider_connection_list_available,
            commands::provider_connections::provider_connection_create,
            commands::provider_connections::provider_connection_update,
            commands::provider_connections::provider_connection_delete,
            commands::provider_connections::provider_connection_get,
            commands::provider_connections::provider_connection_list,
            commands::provider_connections::provider_connection_list_models,
            commands::provider_connections::provider_descriptor_models,
            commands::provider_connections::provider_connection_test,
            // Skill catalog commands
            commands::skills::skill_sources_list,
            commands::skills::skills_list,
            commands::skills::skills_catalog,
            commands::skills::skill_source_add,
            commands::skills::skill_source_refresh,
            commands::skills::skill_source_set_enabled,
            commands::skills::skill_source_delete,
            commands::system_apps::system_apps_detect,
            commands::system_apps::get_system_apps_settings,
            commands::system_apps::set_system_apps_settings,
            commands::system_apps::open_workspace_path,
            commands::system_apps::workspace_import_files,
            commands::workspace::workspace_get_snapshot,
            commands::workspace::workspace_get_or_create_session,
            commands::workspace::workspace_read_file,
            commands::workspace::workspace_read_file_base64,
            commands::workspace::workspace_list_dir,
            commands::workspace::workspace_search_artifacts,
            commands::workspace::workspace_write_file,
            commands::workspace::workspace_download_file,
            commands::workspace::workspace_update_session_mcp,
            commands::workspace::workspace_set_provider,
            commands::workspace::workspace_list_agents,
            commands::workspace::workspace_set_default_agent,
            commands::workspace::workspace_acknowledge_task,
            commands::workspace::workspace_create,
            commands::workspace::workspace_clone_config,
            commands::workspace::workspace_list,
            commands::workspace::workspace_run_now,
            commands::workspace::workspace_set_schedule,
            commands::workspace::workspace_set_schedule_paused,
            commands::workspace::workspace_preview_schedule,
            commands::workspace::workspace_host_timezone,
            commands::workspace::workspace_delete,
            commands::workspace::workspace_set_title,
            commands::workspace::workspace_mark_opened,
            commands::workspace_agents::workspace_get_agent,
            commands::workspace_agents::workspace_create_agent,
            commands::workspace_agents::workspace_update_agent,
            commands::workspace_agents::workspace_delete_agent,
            commands::workspace_agents::workspace_set_agent_enabled,
            commands::workspace_agents::workspace_agent_default_execution,
            commands::permissions::submit_permission_decision,
            commands::permissions::list_pending_permission_requests,
            commands::permissions::list_pending_permission_counts,
            commands::path_grants::submit_path_grant_decision,
            commands::path_grants::list_pending_path_grant_requests,
            commands::path_grants::list_pending_path_grant_counts,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

async fn initialize_workspace_storage(state: &tauri::State<'_, AppState>) -> Result<(), String> {
    {
        let config = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?
            .get();
        for dir in config.expanded_workspace_dirs() {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create workspace dir {}: {}", dir.display(), e))?;
        }
    }

    let is_empty = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .by_id
        .is_empty();

    if is_empty {
        let id = uuid::Uuid::new_v4().to_string();
        let manager_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        let base = state.workspace_create_target(None)?;
        let root = base.join(&id);
        let mut config = config::workspace_config::WorkspaceConfig::new(
            id.clone(),
            "Default".to_string(),
            now,
            manager_id,
        );
        // Attach any existing (incl. just-auto-detected) provider so the very
        // first workspace is usable out of the box.
        let connections = state
            .config_manager
            .lock()
            .map_err(|e| format!("Config lock error: {}", e))?
            .get_provider_connections();
        config.attach_default_provider(&connections, now);
        config::workspace_config::save(&root, &config).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(root.join(".clai").join("memory").join("journal"))
            .map_err(|e| format!("Failed to prepare default workspace: {}", e))?;
        let pool = db::init_workspace_db(&root).await?;
        let mut index = state
            .workspace_index
            .write()
            .map_err(|e| format!("Workspace index lock error: {}", e))?;
        index.insert_config(root, &config);
        index.attach_pool(id, pool);
        return Ok(());
    }

    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();

    const MAX_WORKSPACE_DB_INIT_CONCURRENCY: usize = 8;
    let mut pending = tokio::task::JoinSet::new();
    for locator in locators {
        pending.spawn(async move {
            let id = locator.id;
            let root_path = locator.root_path;
            let result = db::init_workspace_db(&root_path).await;
            (id, root_path, result)
        });

        if pending.len() >= MAX_WORKSPACE_DB_INIT_CONCURRENCY {
            let Some(joined) = pending.join_next().await else {
                continue;
            };
            attach_workspace_db_result(state, joined)?;
        }
    }

    while let Some(joined) = pending.join_next().await {
        attach_workspace_db_result(state, joined)?;
    }
    Ok(())
}

fn attach_workspace_db_result(
    state: &tauri::State<'_, AppState>,
    joined: Result<
        (String, std::path::PathBuf, Result<db::DbPool, String>),
        tokio::task::JoinError,
    >,
) -> Result<(), String> {
    let (id, root_path, result) =
        joined.map_err(|e| format!("Workspace DB initialization task failed: {}", e))?;
    match result {
        Ok(pool) => state
            .workspace_index
            .write()
            .map_err(|e| format!("Workspace index lock error: {}", e))?
            .attach_pool(id, pool),
        Err(error) => state
            .workspace_index
            .write()
            .map_err(|e| format!("Workspace index lock error: {}", e))?
            .record_failure(
                root_path,
                workspace_index::LoadFailureReason::DbCorrupt(error),
            ),
    }
    Ok(())
}

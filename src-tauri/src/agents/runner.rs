//! Agent Runner - Background task that executes agents.
//!
//! This module provides the background loop that:
//! 1. Periodically checks the scheduler for ready agents
//! 2. Executes automations through the assistant runtime
//! 3. Handles completion and errors
//!
//! # Starting the Runner
//!
//! The runner is started during app initialization via `start_agent_runner()`.
//! It runs continuously in the background, checking for work every 5 seconds.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                      AGENT RUNNER                                │
//! │                                                                  │
//! │  loop {                                                          │
//! │      sleep(CHECK_INTERVAL)                                       │
//! │           │                                                      │
//! │           ▼                                                      │
//! │      scheduler.next_ready()                                      │
//! │           │                                                      │
//! │           ▼ (if Some)                                            │
//! │      get definition + instance                                   │
//! │           │                                                      │
//! │           ▼                                                      │
//! │      engine::run_session_turn(...)                               │
//! │           │                                                      │
//! │           ▼                                                      │
//! │      scheduler.complete_agent(success)                           │
//! │  }                                                               │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use std::time::Duration;

use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::agents::designer;
use crate::agents::{SchedulerState, SharedScheduler};
use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository::{self, CreateRunParams, CreateSessionParams};
use crate::assistant::runtime;
use crate::assistant::types::{
    AssistantRun, ContentPart, MessageRole, ProviderConnection, RunStatus, RunTrigger,
    SessionContext, SessionKind,
};
use crate::config::agent_instructions_with_skills;
use crate::db::DbPool;
use crate::AppState;

// =============================================================================
// Configuration
// =============================================================================

/// How often to check for ready agents (in seconds).
const CHECK_INTERVAL_SECS: u64 = 5;

// =============================================================================
// Runner
// =============================================================================

/// Starts the agent runner background task.
///
/// This spawns a tokio task that runs indefinitely, periodically checking
/// for agents that need to run and executing them.
///
/// # Arguments
///
/// * `app_handle` - Tauri app handle for accessing state and emitting events
/// * `scheduler` - The shared agent scheduler
///
/// # Returns
///
/// A handle to the spawned task (can be used to abort if needed).
pub fn start_agent_runner(app_handle: AppHandle, scheduler: SharedScheduler) {
    tracing::info!(
        check_interval_secs = CHECK_INTERVAL_SECS,
        "Starting agent runner"
    );

    // Use Tauri's async runtime to spawn the background task
    tauri::async_runtime::spawn(async move {
        tracing::info!("Agent runner background task started");
        loop {
            // Sleep first to avoid running immediately on startup
            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;

            tracing::debug!("Checking for ready agents...");

            // Check for and run agents
            if let Err(e) = run_next_agent(&app_handle, &scheduler).await {
                tracing::error!(error = %e, "Agent runner error");
            }
        }
    });
}

/// Checks for and runs the next ready agent.
///
/// This is called periodically by the runner loop. It:
/// 1. Checks if the scheduler is paused
/// 2. Gets the next ready agent (if any)
/// 3. Executes the agent
/// 4. Marks it complete
async fn run_next_agent(
    app_handle: &AppHandle,
    scheduler: &SharedScheduler,
) -> Result<(), RunnerError> {
    // Get app state
    let state = app_handle.state::<AppState>();

    let pool = match app_handle.try_state::<DbPool>() {
        Some(pool) => pool.inner().clone(),
        None => {
            tracing::debug!("Assistant database not ready yet, skipping agent check");
            return Ok(());
        }
    };

    // Check for a ready agent
    let instance_id = {
        let mut sched = scheduler.lock().await;

        // Log scheduler state
        let instance_count = sched.instance_count();
        let state = sched.state();
        let running_agent = sched.running_instance();
        tracing::debug!(
            instance_count,
            scheduler_state = ?state,
            running_agent = ?running_agent,
            "Scheduler status"
        );

        // Check if scheduler is paused
        if matches!(state, SchedulerState::Paused { .. }) {
            tracing::debug!("Scheduler is paused");
            return Ok(());
        }

        sched.next_ready()
    };

    let instance_id = match instance_id {
        Some(id) => id,
        None => {
            tracing::debug!("No agents ready");
            return Ok(());
        }
    };

    tracing::info!(instance_id = %instance_id, "Running agent");

    // Get the instance details
    let (agent_id, space_id, room_id) = {
        let sched = scheduler.lock().await;
        let instance = sched
            .get_instance(&instance_id)
            .ok_or_else(|| RunnerError::InstanceNotFound(instance_id.clone()))?;

        (
            instance.agent_id.clone(),
            instance.space_id.clone(),
            instance.room_id.clone(),
        )
    };

    tracing::debug!(
        agent_id = %agent_id,
        space_id = %space_id,
        room_id = %room_id,
        "Got agent instance"
    );

    // Get the agent config from ConfigManager
    let agent_config = {
        let config = state.config_manager.lock().unwrap();
        config
            .get_agent(&agent_id)
            .ok_or_else(|| RunnerError::AgentNotFound(agent_id.clone()))?
    };

    // Check if the automation is still enabled.
    if !agent_config.enabled {
        tracing::info!(
            agent_id = %agent_id,
            space_id = %space_id,
            room_id = %room_id,
            "Automation is no longer enabled, removing instance"
        );

        // Remove the instance from scheduler
        {
            let mut sched = scheduler.lock().await;
            sched.remove_instance(&instance_id);
        }

        return Ok(());
    }

    tracing::debug!(agent_name = %agent_config.name, "Got agent config");

    let connections = resolve_agent_connections(&pool, &agent_config).await?;

    let existing_session = find_background_session(
        &pool,
        &agent_config,
        if space_id.is_empty() {
            None
        } else {
            Some(space_id.as_str())
        },
        if room_id.is_empty() {
            None
        } else {
            Some(room_id.as_str())
        },
    )
    .await?;
    let preferred_tab_id = existing_session
        .as_ref()
        .and_then(|session| session.tab_id.clone());

    {
        let mut sched = scheduler.lock().await;
        if let Some(instance) = sched.get_instance_mut(&instance_id) {
            instance.tab_id = preferred_tab_id.clone();
        }
    }

    let session = ensure_background_session(
        app_handle,
        &pool,
        &agent_config,
        &space_id,
        &room_id,
        preferred_tab_id.as_deref(),
    )
    .await?;

    let result =
        run_scheduled_agent_with_fallback(app_handle, &pool, &instance_id, &session, &connections)
            .await;

    // Mark agent complete
    let success = match &result {
        Ok(()) => {
            tracing::info!(instance_id = %instance_id, "Agent completed successfully");
            true
        }
        Err(e) => {
            tracing::error!(instance_id = %instance_id, error = %e, "Agent execution error");
            false
        }
    };

    // Run workspace designer after a successful agent run.
    // This generates/evolves .clai/workspace.json for the workspace UI.
    // Fire-and-forget: failures are logged but do not affect the run outcome.
    if success {
        let message_count = repository::list_messages(&pool, &session.id)
            .await
            .map(|msgs| msgs.len())
            .unwrap_or(0);
        let artifact_count = 0_usize; // Counted from filesystem by the designer itself
        let designer_connection = connections.first().cloned();
        if let Some(conn) = designer_connection {
            let agent_id = agent_config.id.clone();
            let agent_name = agent_config.name.clone();
            let agent_desc = agent_config.description.clone();
            let sess_id = session.id.clone();
            tauri::async_runtime::spawn(async move {
                designer::design_workspace(
                    &agent_id,
                    &agent_name,
                    &agent_desc,
                    &conn,
                    &sess_id,
                    message_count,
                    artifact_count,
                )
                .await;
            });
        }
    }

    // Update scheduler with interval from config
    let interval_ms = (agent_config.interval_minutes as u64) * 60 * 1000;
    {
        let mut sched = scheduler.lock().await;
        sched.complete_agent(&instance_id, success, interval_ms);

        // Log next run time
        if let Some(instance) = sched.get_instance(&instance_id) {
            let seconds_until_next = instance.seconds_until_next_run();
            tracing::info!(
                instance_id = %instance_id,
                seconds_until_next,
                minutes_until_next = seconds_until_next / 60,
                "Scheduled next agent run"
            );
        }
    }

    Ok(())
}

// =============================================================================
// Errors
// =============================================================================

/// Errors that can occur in the agent runner.
#[derive(Debug)]
pub enum RunnerError {
    /// Agent instance not found.
    InstanceNotFound(String),
    /// Agent config not found in ConfigManager.
    AgentNotFound(String),
    /// Failed to persist or load assistant runtime data.
    AssistantPersistence(String),
    /// Failed to create or restore an assistant session.
    AssistantSession(String),
    /// No usable provider connections are configured for the agent.
    NoProviderConfigured(String),
    /// Tab was not persisted within the expected timeout.
    #[cfg(test)]
    TabPersistenceFailed(String),
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::InstanceNotFound(id) => write!(f, "Agent instance not found: {}", id),
            RunnerError::AgentNotFound(id) => write!(f, "Agent config not found: {}", id),
            RunnerError::AssistantPersistence(msg) => {
                write!(f, "Assistant persistence failed: {}", msg)
            }
            RunnerError::AssistantSession(msg) => write!(f, "Assistant session failed: {}", msg),
            RunnerError::NoProviderConfigured(msg) => write!(f, "No provider configured: {}", msg),
            #[cfg(test)]
            RunnerError::TabPersistenceFailed(msg) => {
                write!(f, "Tab persistence timed out: {}", msg)
            }
        }
    }
}

impl std::error::Error for RunnerError {}

/// Poll the database until a tab row with the given `tab_id` exists,
/// or return `TabPersistenceFailed` after `timeout` elapses.
#[cfg(test)]
async fn wait_for_persisted_tab(
    pool: &sqlx::SqlitePool,
    tab_id: &str,
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
) -> Result<(), RunnerError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let exists: bool = sqlx::query_scalar::<_, i64>("SELECT 1 FROM tabs WHERE id = ? LIMIT 1")
            .bind(tab_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| RunnerError::TabPersistenceFailed(e.to_string()))?
            .is_some();
        if exists {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(RunnerError::TabPersistenceFailed(format!(
                "Tab {} was not persisted within {:?}",
                tab_id, timeout
            )));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn ensure_background_session(
    app_handle: &AppHandle,
    pool: &DbPool,
    agent_config: &crate::config::AgentConfig,
    space_id: &str,
    room_id: &str,
    tab_id: Option<&str>,
) -> Result<crate::assistant::types::AssistantSession, RunnerError> {
    let session_space_id = if space_id.is_empty() {
        None
    } else {
        Some(space_id.to_string())
    };
    let session_room_id = if room_id.is_empty() {
        None
    } else {
        Some(room_id.to_string())
    };
    let state = app_handle.state::<AppState>();
    let config = state
        .config_manager
        .lock()
        .ok()
        .map(|manager| manager.get());
    let automation_description = match config {
        Some(config) => agent_instructions_with_skills(&config, agent_config),
        None => agent_config.description.clone(),
    };
    let desired_context = SessionContext {
        space_id: session_space_id.clone(),
        room_id: session_room_id.clone(),
        workspace_id: Some(agent_config.id.clone()),
        tab_id: tab_id.map(str::to_string),
        tool_scopes: agent_config
            .required_tools()
            .into_iter()
            .map(str::to_string)
            .collect(),
        mcp_server_ids: agent_config.selected_mcp_server_ids.clone(),
        execution: agent_config.execution.clone(),
        netdata_conversation_id: None,
        automation_id: Some(agent_config.id.clone()),
        agent_workspace_id: Some(agent_config.id.clone()),
        automation_name: Some(agent_config.name.clone()),
        automation_description: Some(automation_description),
        inter_agent_call: None,
        workspace_agents: Vec::new(),
    };

    let existing = find_background_session(
        pool,
        agent_config,
        session_space_id.as_deref(),
        session_room_id.as_deref(),
    )
    .await?;

    if let Some(session) = existing {
        let session = if session.tab_id.as_deref() != tab_id
            || session.title.as_deref() != Some(agent_config.name.as_str())
            || session.context != desired_context
        {
            let mut updated = session;
            updated.tab_id = tab_id.map(str::to_string);
            updated.title = Some(agent_config.name.clone());
            updated.context = desired_context.clone();
            updated.updated_at = chrono::Utc::now().timestamp_millis();
            repository::update_session(pool, &updated)
                .await
                .map_err(RunnerError::AssistantPersistence)?
        } else {
            session
        };

        emit_event(
            app_handle,
            &session,
            None,
            AssistantUiEvent::SessionCreated {
                session: Box::new(session.clone()),
            },
        )
        .map_err(RunnerError::AssistantSession)?;

        return Ok(session);
    }

    let session = repository::create_session(
        pool,
        CreateSessionParams {
            tab_id: tab_id.map(str::to_string),
            kind: SessionKind::BackgroundJob,
            title: Some(agent_config.name.clone()),
            context: desired_context,
        },
    )
    .await
    .map_err(RunnerError::AssistantSession)?;

    emit_event(
        app_handle,
        &session,
        None,
        AssistantUiEvent::SessionCreated {
            session: Box::new(session.clone()),
        },
    )
    .map_err(RunnerError::AssistantSession)?;

    Ok(session)
}

async fn find_background_session(
    pool: &DbPool,
    agent_config: &crate::config::AgentConfig,
    space_id: Option<&str>,
    room_id: Option<&str>,
) -> Result<Option<crate::assistant::types::AssistantSession>, RunnerError> {
    let session_space_id = space_id.map(str::to_string);
    let session_room_id = room_id.map(str::to_string);

    Ok(repository::list_sessions(pool, None)
        .await
        .map_err(RunnerError::AssistantPersistence)?
        .into_iter()
        .find(|session| {
            session.kind == SessionKind::BackgroundJob
                && session.context.space_id == session_space_id
                && session.context.room_id == session_room_id
                && session.context.automation_id.as_deref() == Some(agent_config.id.as_str())
        }))
}

async fn resolve_agent_connections(
    pool: &DbPool,
    agent_config: &crate::config::AgentConfig,
) -> Result<Vec<ProviderConnection>, RunnerError> {
    let all_connections = repository::list_provider_connections(pool)
        .await
        .map_err(RunnerError::AssistantPersistence)?;

    let mut resolved = Vec::new();
    for connection_id in &agent_config.provider_connection_ids {
        match all_connections
            .iter()
            .find(|connection| connection.id == *connection_id)
        {
            Some(connection) if connection.enabled => resolved.push(connection.clone()),
            Some(connection) => {
                tracing::warn!(
                    agent_id = %agent_config.id,
                    connection_id = %connection.id,
                    "Skipping disabled provider connection"
                );
            }
            None => {
                tracing::warn!(
                    agent_id = %agent_config.id,
                    connection_id = %connection_id,
                    "Skipping missing provider connection"
                );
            }
        }
    }

    if resolved.is_empty() {
        return Err(RunnerError::NoProviderConfigured(agent_config.id.clone()));
    }

    Ok(resolved)
}

async fn run_scheduled_agent_with_fallback(
    app_handle: &AppHandle,
    pool: &DbPool,
    instance_id: &str,
    session: &crate::assistant::types::AssistantSession,
    connections: &[ProviderConnection],
) -> Result<(), RunnerError> {
    let deps = AssistantDeps {
        pool: pool.clone(),
        app: app_handle.clone(),
    };
    let mut last_error: Option<String> = None;

    for (index, connection) in connections.iter().enumerate() {
        tracing::info!(
            session_id = %session.id,
            connection_id = %connection.id,
            provider_id = %connection.provider_id,
            model_id = %connection.model_id,
            fallback_index = index,
            "Starting scheduled assistant run"
        );

        let run = repository::create_run(
            pool,
            CreateRunParams {
                session_id: session.id.clone(),
                status: RunStatus::Queued,
                trigger: RunTrigger::Scheduled,
                connection_id: connection.id.clone(),
                provider_id: connection.provider_id.clone(),
                model_id: connection.model_id.clone(),
                usage: None,
                error: None,
            },
        )
        .await
        .map_err(RunnerError::AssistantPersistence)?;

        let runtime_run_id = format!("scheduled:{}:{}", instance_id, Uuid::new_v4());
        let cancel_token = runtime::register_run(&runtime_run_id);
        let input = RunTurnInput {
            session_id: session.id.clone(),
            run_id: Some(run.id.clone()),
            trigger: RunTrigger::Scheduled,
            connection_id: connection.id.clone(),
            cancel_token,
            inter_agent_call_depth: None,
        };
        let result = engine::run_session_turn(&deps, input).await;
        runtime::unregister_run(&runtime_run_id);

        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                let error_text = error.to_string();
                last_error = Some(error_text.clone());
                let can_fallback = run_allows_fallback(pool, session.id.as_str(), &run).await?;

                tracing::warn!(
                    run_id = %run.id,
                    connection_id = %connection.id,
                    can_fallback,
                    error = %error_text,
                    "Scheduled assistant run failed"
                );

                if !can_fallback {
                    break;
                }
            }
        }
    }

    Err(RunnerError::AssistantSession(
        last_error.unwrap_or_else(|| "scheduled run failed".to_string()),
    ))
}

async fn run_allows_fallback(
    pool: &DbPool,
    session_id: &str,
    run: &AssistantRun,
) -> Result<bool, RunnerError> {
    let tool_calls = repository::list_tool_calls(pool, session_id, Some(&run.id))
        .await
        .map_err(RunnerError::AssistantPersistence)?;
    if !tool_calls.is_empty() {
        return Ok(false);
    }

    let messages = repository::list_messages(pool, session_id)
        .await
        .map_err(RunnerError::AssistantPersistence)?;
    let completed_at = run.completed_at.unwrap_or(i64::MAX);
    let has_assistant_output = messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.created_at >= run.started_at
            && message.created_at <= completed_at
            && message_contains_output(&message.content)
    });

    Ok(!has_assistant_output)
}

fn message_contains_output(content: &[ContentPart]) -> bool {
    content.iter().any(|part| match part {
        ContentPart::Text { text } => !text.trim().is_empty(),
        ContentPart::ToolUse { .. } | ContentPart::ToolResult { .. } => true,
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use tokio::time::sleep;

    #[test]
    fn test_runner_error_display() {
        let err = RunnerError::InstanceNotFound("test-id".to_string());
        assert!(err.to_string().contains("test-id"));

        let err = RunnerError::AgentNotFound("test-agent".to_string());
        assert!(err.to_string().contains("test-agent"));
    }

    #[test]
    fn test_check_interval() {
        // Verify the check interval is reasonable
        assert!(CHECK_INTERVAL_SECS >= 1, "Check interval too short");
        assert!(CHECK_INTERVAL_SECS <= 120, "Check interval too long");
    }

    #[tokio::test]
    async fn waits_for_persisted_tab() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::query("CREATE TABLE tabs (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();

        let pool_for_insert = pool.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            sqlx::query("INSERT INTO tabs (id) VALUES (?)")
                .bind("tab-delayed")
                .execute(&pool_for_insert)
                .await
                .unwrap();
        });

        let result = wait_for_persisted_tab(
            &pool,
            "tab-delayed",
            Duration::from_millis(500),
            Duration::from_millis(10),
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn times_out_when_tab_is_not_persisted() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::query("CREATE TABLE tabs (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();

        let result = wait_for_persisted_tab(
            &pool,
            "tab-missing",
            Duration::from_millis(100),
            Duration::from_millis(10),
        )
        .await;

        match result {
            Err(RunnerError::TabPersistenceFailed(message)) => {
                assert!(message.contains("tab-missing"));
            }
            other => panic!("expected timeout error, got {:?}", other),
        }
    }
}

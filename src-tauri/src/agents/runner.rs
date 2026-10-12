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

use crate::agents::{SchedulerState, SharedScheduler};
use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository::{self, CreateRunParams};
use crate::assistant::runtime;
use crate::assistant::types::{ProviderConnection, RunStatus, RunTrigger};
use crate::config::{workspace_config, AgentConfig};
use crate::db::DbPool;
use crate::AppState;

/// Loads a workspace-local agent from `<workspace>/.clai/config.json`.
fn load_workspace_agent_as_config(
    state: &AppState,
    id: &str,
) -> Result<Option<AgentConfig>, RunnerError> {
    let app_config = state
        .config_manager
        .lock()
        .map_err(|e| RunnerError::AssistantPersistence(format!("Lock error: {}", e)))?
        .get();
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| {
            RunnerError::AssistantPersistence(format!("Workspace index lock error: {}", e))
        })?
        .locators_sorted();

    for locator in locators {
        let (config, roster) = state.resolve_workspace_roster(&locator.id).map_err(|e| {
            RunnerError::AssistantPersistence(format!("Failed to load workspace config: {}", e))
        })?;
        if let Some(agent) = roster
            .iter()
            .map(|resolved| &resolved.agent)
            .find(|agent| agent.id == id)
        {
            let created_at = chrono::DateTime::from_timestamp_millis(agent.created_at)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();
            let updated_at = chrono::DateTime::from_timestamp_millis(agent.updated_at)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();
            let is_manager = agent.id == config.main_agent_id();
            return Ok(Some(AgentConfig {
                id: agent.id.clone(),
                workspace_id: config.id,
                name: agent.name.clone(),
                description: agent.description.clone(),
                // Schedule is workspace-level. Only the default (manager)
                // agent inherits the workspace schedule; sub-agents never
                // run on their own schedule.
                schedule_enabled: is_manager && config.schedule.enabled,
                schedule_kind: if is_manager {
                    config.schedule.kind.clone()
                } else {
                    crate::config::workspace_config::ScheduleKind::default()
                },
                enabled: agent.enabled,
                selected_mcp_server_ids: workspace_config::enabled_mcp_ids(
                    &agent.selected_mcp_servers,
                ),
                provider_connection_ids: agent.provider_connection_ids.clone(),
                selected_skill_ids: workspace_config::refs_to_skill_ids(
                    &app_config,
                    &agent.selected_skills,
                ),
                execution: agent.execution.clone(),
                created_at,
                updated_at,
            }));
        }
    }
    Ok(None)
}

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
#[allow(
    clippy::cognitive_complexity,
    reason = "lint debt: cognitive complexity 107 against a budget of 25"
)]
#[expect(
    clippy::too_many_lines,
    reason = "lint debt: 143 lines against a 100-line budget; split it, do not raise the budget"
)]
async fn run_next_agent(
    app_handle: &AppHandle,
    scheduler: &SharedScheduler,
) -> Result<(), RunnerError> {
    // Get app state
    let state = app_handle.state::<AppState>();

    // Note: pre-refactor this guard waited on a global `DbPool` in
    // managed state. After the move to per-workspace SQLite that pool
    // no longer exists — the per-workspace pool is loaded on demand
    // below via `state.workspace_db(...)`. Leaving the guard in caused
    // every tick to exit early, so scheduled workspaces never ran.

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

    let Some(instance_id) = instance_id else {
        tracing::debug!("No agents ready");
        return Ok(());
    };

    tracing::info!(instance_id = %instance_id, "Running agent");

    // Get the instance details
    let agent_id = {
        let sched = scheduler.lock().await;
        let instance = sched
            .get_instance(&instance_id)
            .ok_or_else(|| RunnerError::InstanceNotFound(instance_id.clone()))?;

        instance.agent_id.clone()
    };

    tracing::debug!(agent_id = %agent_id, "Got agent instance");

    let agent_config = load_workspace_agent_as_config(state.inner(), &agent_id)?
        .ok_or_else(|| RunnerError::AgentNotFound(agent_id.clone()))?;
    let workspace_pool = state
        .workspace_db(&agent_config.workspace_id)
        .await
        .map_err(RunnerError::AssistantPersistence)?;

    // Check if the automation is still enabled.
    if !agent_config.enabled {
        tracing::info!(
            agent_id = %agent_id,
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

    // Don't start a scheduled run while this workspace's canonical manager
    // session is already busy. The scheduler's one-at-a-time guard only
    // covers scheduler-started runs; an interactive turn the user kicked
    // off from the chat UI runs *outside* the scheduler and shares the same
    // session. Without this check a due tick would start a second,
    // concurrent run on that session — surfacing as two live conversations
    // in the workspace. Defer instead: release the instance back to
    // ready-now (without consuming the schedule) so a later tick fires once
    // the workspace is idle.
    if let Some(existing) = repository::find_manager_session(&workspace_pool, &agent_config.id)
        .await
        .map_err(RunnerError::AssistantPersistence)?
    {
        let busy = repository::session_has_active_run(&workspace_pool, &existing.id)
            .await
            .map_err(RunnerError::AssistantPersistence)?;
        if busy {
            tracing::info!(
                instance_id = %instance_id,
                session_id = %existing.id,
                "Workspace session is busy with an active run; deferring scheduled tick"
            );
            let mut sched = scheduler.lock().await;
            sched.defer_running_instance(&instance_id);
            return Ok(());
        }
    }

    let connection = resolve_agent_connection(state.inner(), &agent_config)?;

    let session =
        ensure_workspace_manager_session(app_handle, &workspace_pool, &agent_config).await?;

    let result = run_scheduled_agent(
        app_handle,
        &agent_config.workspace_id,
        &workspace_pool,
        &session,
        &connection,
    )
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

    // Compute the next wall-clock fire time from the workspace's
    // schedule kind. For Interval mode this is `now + N min`; for Cron
    // this is the next cron fire in the user's timezone. Errors are
    // logged but non-fatal — the instance won't be re-scheduled, which
    // is the safest behavior in the face of a malformed cron expression
    // (user can fix via the UI without the agent loop-firing).
    let now_unix_ms = chrono::Utc::now().timestamp_millis();
    let next_run_at_unix_ms = match crate::agents::schedule::compute_next_run_at(
        &agent_config.schedule_kind,
        now_unix_ms,
    ) {
        Ok(target) => Some(target),
        Err(e) => {
            tracing::warn!(
                workspace_id = %agent_config.workspace_id,
                error = %e,
                "Failed to compute next run time; agent will not be rescheduled"
            );
            None
        }
    };
    {
        let mut sched = scheduler.lock().await;
        // When we can't compute a next time, pass `i64::MAX` so the
        // scheduler parks the instance indefinitely. The user has to
        // fix the schedule via the UI before it ticks again.
        sched.complete_agent(
            &instance_id,
            success,
            next_run_at_unix_ms.unwrap_or(i64::MAX),
        );

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

    // Persist the new next-run wall-clock time (so an app restart
    // resumes the schedule instead of firing immediately) and the
    // run-completion stamp (so the workspace rail can flag the result
    // as unread until the user opens the workspace). Best-effort: a
    // write failure here only loses the restart-survival guarantee
    // for this one workspace until the next successful tick.
    if let Err(e) = persist_workspace_run_completion(
        state.inner(),
        &agent_config.workspace_id,
        next_run_at_unix_ms,
    ) {
        tracing::warn!(
            workspace_id = %agent_config.workspace_id,
            error = %e,
            "Failed to persist run completion to workspace config"
        );
    }

    Ok(())
}

/// Persist a run's completion to the workspace config: the new
/// `schedule.next_run_at_unix_ms` anchor (`None` parks the schedule) and
/// `last_run_completed_at`, which the workspace rail compares against
/// `last_opened_at` to show the "unread" indicator. Refreshes the
/// in-memory workspace index so subsequent loads see both. Failed runs
/// stamp completion too — a failure the user hasn't seen is exactly the
/// kind of activity the unread marker exists for.
pub(crate) fn persist_workspace_run_completion(
    state: &AppState,
    workspace_id: &str,
    next_run_at_unix_ms: Option<i64>,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();
    // Atomic RMW: `workspace_mark_opened` fires from the FE the moment a
    // run ends while the user is viewing the workspace, so a bare
    // load→save here raced it and the anchor written below could be
    // clobbered by the other writer's stale copy (the schedule then
    // re-fired on every app restart).
    state.update_workspace_config(workspace_id, |config| {
        config.schedule.next_run_at_unix_ms = next_run_at_unix_ms;
        config.last_run_completed_at = now;
        config.updated_at = now;
        Ok(())
    })?;
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
        }
    }
}

impl std::error::Error for RunnerError {}

/// Find-or-create the canonical (workspace, manager) session for a
/// scheduled run.
///
/// This is the **same** session row that the workspace card-click path
/// resolves via `commands::workspace::find_workspace_session`, so the user
/// sees one continuous thread regardless of how a turn was triggered. The
/// agent's configuration is resolved when the run starts, so an existing row
/// is used as-is.
async fn ensure_workspace_manager_session(
    app_handle: &AppHandle,
    pool: &DbPool,
    agent_config: &crate::config::AgentConfig,
) -> Result<crate::assistant::types::AssistantSession, RunnerError> {
    let session = repository::find_or_create_manager_session(
        pool,
        &agent_config.id,
        Some(agent_config.name.clone()),
    )
    .await
    .map_err(RunnerError::AssistantPersistence)?;

    emit_event(
        app_handle,
        &agent_config.workspace_id,
        &session,
        None,
        AssistantUiEvent::SessionCreated {
            session: Box::new(session.clone()),
        },
    )
    .map_err(RunnerError::AssistantSession)?;

    Ok(session)
}

fn resolve_agent_connection(
    state: &AppState,
    agent_config: &AgentConfig,
) -> Result<ProviderConnection, RunnerError> {
    let all_connections = state
        .config_manager
        .lock()
        .map_err(|e| RunnerError::AssistantPersistence(format!("Lock error: {}", e)))?
        .get_provider_connections();

    let connection = agent_config
        .first_enabled_connection(&all_connections)
        .ok_or_else(|| RunnerError::NoProviderConfigured(agent_config.id.clone()))?;

    if agent_config.provider_connection_ids.first() != Some(&connection.id) {
        tracing::warn!(
            agent_id = %agent_config.id,
            connection_id = %connection.id,
            "First configured provider connection is disabled or missing; using the first enabled one"
        );
    }

    Ok(connection.clone())
}

async fn run_scheduled_agent(
    app_handle: &AppHandle,
    workspace_id: &str,
    pool: &DbPool,
    session: &crate::assistant::types::AssistantSession,
    connection: &ProviderConnection,
) -> Result<(), RunnerError> {
    let deps = AssistantDeps {
        workspace_id: workspace_id.to_string(),
        pool: pool.clone(),
        app: app_handle.clone(),
    };

    tracing::info!(
        session_id = %session.id,
        connection_id = %connection.id,
        protocol_id = %connection.protocol_id,
        model_id = %connection.model_id,
        "Starting scheduled assistant run"
    );

    let run = repository::create_run(
        pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::Scheduled,
            connection_id: connection.id.clone(),
            protocol_id: connection.protocol_id.clone(),
            model_id: connection.model_id.clone(),
            error: None,
        },
    )
    .await
    .map_err(RunnerError::AssistantPersistence)?;

    // Register the cancel token under the DB run.id (the same key the
    // chat and workspace-task paths use) so `assistant_cancel_run(workspace_id, run.id)`
    // can find it.
    let run_registration = runtime::register_run(&run.id);
    let cancel_token = run_registration.token();
    let input = RunTurnInput {
        session_id: session.id.clone(),
        run_id: Some(run.id.clone()),
        trigger: RunTrigger::Scheduled,
        connection_id: connection.id.clone(),
        cancel_token,
        inter_agent_call_depth: None,
        trigger_message_id: None,
    };
    let result = engine::run_session_turn(&deps, input).await;
    drop(run_registration);

    if let Err(error) =
        crate::commands::assistant::start_queued_followup_if_idle(deps.clone(), session.id.clone())
            .await
    {
        tracing::error!(
            session_id = %session.id,
            error = %error,
            "Failed to start queued assistant follow-up after scheduled run"
        );
    }

    result.map_err(|error| {
        tracing::warn!(
            run_id = %run.id,
            connection_id = %connection.id,
            error = %error,
            "Scheduled assistant run failed"
        );
        RunnerError::AssistantSession(error.to_string())
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runner_error_display() {
        let err = RunnerError::InstanceNotFound("test-id".to_string());
        assert!(err.to_string().contains("test-id"));

        let err = RunnerError::AgentNotFound("test-agent".to_string());
        assert!(err.to_string().contains("test-agent"));
    }

    #[test]
    fn no_provider_configured_error_names_agent() {
        let err = RunnerError::NoProviderConfigured("agent-1".to_string());
        let text = err.to_string();
        assert!(text.contains("No provider configured"));
        assert!(text.contains("agent-1"));
    }
}

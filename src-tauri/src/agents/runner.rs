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
use crate::assistant::repository::{self, CreateRunParams, CreateSessionParams};
use crate::assistant::runtime;
use crate::assistant::types::{
    AssistantRun, ContentPart, MessageRole, ProviderConnection, RunStatus, RunTrigger,
    SessionContext, SessionKind,
};
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
        let config = workspace_config::load(&locator.root_path).map_err(|e| {
            RunnerError::AssistantPersistence(format!("Failed to load workspace config: {}", e))
        })?;
        if let Some(agent) = config.agents.iter().find(|agent| agent.id == id) {
            let created_at = chrono::DateTime::from_timestamp_millis(agent.created_at)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();
            let updated_at = chrono::DateTime::from_timestamp_millis(agent.updated_at)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();
            let is_manager = agent.id == config.default_agent_id;
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
                selected_mcp_server_ids: workspace_config::refs_to_mcp_ids(
                    &app_config,
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

    let connections = resolve_agent_connections(state.inner(), &agent_config)?;

    let session = ensure_workspace_manager_session(
        app_handle,
        &workspace_pool,
        &agent_config,
        &space_id,
        &room_id,
    )
    .await?;

    let result =
        run_scheduled_agent_with_fallback(app_handle, &workspace_pool, &session, &connections)
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
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let now = chrono::Utc::now().timestamp_millis();
    // Atomic RMW: `workspace_mark_opened` fires from the FE the moment a
    // run ends while the user is viewing the workspace, so a bare
    // load→save here raced it and the anchor written below could be
    // clobbered by the other writer's stale copy (the schedule then
    // re-fired on every app restart).
    let ((), config) = workspace_config::update(&root, |config| {
        config.schedule.next_run_at_unix_ms = next_run_at_unix_ms;
        config.last_run_completed_at = now;
        config.updated_at = now;
        Ok(())
    })?;
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .insert_config(root, &config);
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
/// resolves via `commands::workspace::find_workspace_session`: the resolver
/// returns the most-recent non-task session for this manager in this
/// workspace, and the workspace itself only owns one such session, which
/// this helper creates as Interactive. User chats and scheduler ticks both
/// write here, so the user sees one continuous thread regardless of how
/// the turn was triggered.
///
/// Reconcile rules: title and most context fields track the live agent
/// config; **dynamic per-run fields are preserved** when an existing row
/// is found:
/// - `cli_session_id`: the Claude Code CLI session id, set on first turn
///   and reused across ticks so the model has its own conversation state
///   to resume. Wiping this on every reconcile would force a fresh CLI
///   session every 5 min — i.e. the model loses its prior reasoning.
/// - `inter_agent_call`: only meaningful inside a synchronous inter-agent
///   call frame; outside one it's already `None`, and overwriting an
///   in-flight value here would be a data race anyway (this function is
///   called between turns, never during one).
#[allow(clippy::too_many_arguments)]
async fn ensure_workspace_manager_session(
    app_handle: &AppHandle,
    pool: &DbPool,
    agent_config: &crate::config::AgentConfig,
    space_id: &str,
    room_id: &str,
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

    // Load the workspace's agent roster so the manager session knows
    // who it can delegate to. Without this, `is_workspace_manager_context`
    // returns false (empty Vec → no `is_default` entry matches), the
    // four `workspace_*` tools never get added to the tool list, and
    // the periodic manager run cannot poll, assign, or request user
    // input. The on-demand chat path already populates this via
    // `desired_workspace_context` — this matches that behavior for
    // scheduled runs. On failure we fall back to an empty Vec rather
    // than crashing the run; the manager will then act as a solo
    // agent (current pre-fix behavior).
    let workspace_agents = if agent_config.workspace_id.is_empty() {
        Vec::new()
    } else {
        crate::commands::workspace::workspace_agent_summaries(
            &state,
            &agent_config.workspace_id,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                workspace_id = %agent_config.workspace_id,
                "Failed to load workspace agent summaries for scheduled run: {} — manager tools will be unavailable for this run",
                e
            );
            Vec::new()
        })
    };
    let desired_context = SessionContext {
        space_id: session_space_id.clone(),
        room_id: session_room_id.clone(),
        workspace_id: Some(agent_config.workspace_id.clone()),
        tool_scopes: agent_config
            .required_tools()
            .into_iter()
            .map(str::to_string)
            .collect(),
        mcp_server_ids: agent_config.selected_mcp_server_ids.clone(),
        execution: agent_config.execution.clone(),
        cli_session_id: None,
        cli_session_provider: None,
        automation_id: Some(agent_config.id.clone()),
        // Despite the misleading name, `agent_workspace_id` is the *workspace*
        // id the tools use to derive the on-disk working directory. Setting it
        // to the agent's own id meant periodic runs wrote to a per-agent dir
        // that the workspace UI never enumerates — manifesting as the
        // "0 memories" inconsistency. On-demand chats already pass workspace_id
        // here via `desired_workspace_context`; this matches them.
        agent_workspace_id: Some(agent_config.workspace_id.clone()),
        automation_name: Some(agent_config.name.clone()),
        inter_agent_call: None,
        workspace_agents,
    };

    let existing = find_manager_session(pool, agent_config).await?;

    if let Some(session) = existing {
        // Preserve dynamic per-run fields that aren't part of the "config
        // reconcile" — see the doc comment above.
        let mut merged = desired_context.clone();
        merged.cli_session_id = session.context.cli_session_id.clone();
        merged.cli_session_provider = session.context.cli_session_provider.clone();
        merged.inter_agent_call = session.context.inter_agent_call.clone();

        let session = if session.title.as_deref() != Some(agent_config.name.as_str())
            || session.context != merged
        {
            let mut updated = session;
            updated.title = Some(agent_config.name.clone());
            updated.context = merged;
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
            // Interactive — the user can type into this same session from
            // the workspace's chat UI. See the doc comment above.
            kind: SessionKind::Interactive,
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

/// Look up the canonical (workspace, manager) session by `automation_id`.
///
/// Kind is intentionally not part of the filter: legacy rows may still be
/// `BackgroundJob` and we want to keep using them rather than creating a
/// parallel Interactive row. Going forward, new rows are created Interactive
/// (see `ensure_workspace_manager_session`).
async fn find_manager_session(
    pool: &DbPool,
    agent_config: &crate::config::AgentConfig,
) -> Result<Option<crate::assistant::types::AssistantSession>, RunnerError> {
    Ok(repository::list_sessions(pool)
        .await
        .map_err(RunnerError::AssistantPersistence)?
        .into_iter()
        .find(|session| session.context.automation_id.as_deref() == Some(agent_config.id.as_str())))
}

fn resolve_agent_connections(
    state: &AppState,
    agent_config: &crate::config::AgentConfig,
) -> Result<Vec<ProviderConnection>, RunnerError> {
    let all_connections = state
        .config_manager
        .lock()
        .map_err(|e| RunnerError::AssistantPersistence(format!("Lock error: {}", e)))?
        .get_provider_connections();

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

        // Register the cancel token under the DB run.id (the same key the
        // chat and workspace-task paths use). Previously this was a
        // synthetic `scheduled:{instance_id}:{uuid}` string, which meant
        // `assistant_cancel_run(run.id)` couldn't find the token and
        // scheduled runs were effectively un-cancellable from the UI.
        let cancel_token = runtime::register_run(&run.id);
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
        runtime::unregister_run(&run.id);

        match result {
            Ok(()) => {
                if let Err(error) = crate::commands::assistant::start_queued_followup_if_idle(
                    pool.clone(),
                    app_handle.clone(),
                    session.id.clone(),
                )
                .await
                {
                    tracing::error!(
                        session_id = %session.id,
                        error = %error,
                        "Failed to start queued assistant follow-up after scheduled run"
                    );
                }
                return Ok(());
            }
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

    if let Err(error) = crate::commands::assistant::start_queued_followup_if_idle(
        pool.clone(),
        app_handle.clone(),
        session.id.clone(),
    )
    .await
    {
        tracing::error!(
            session_id = %session.id,
            error = %error,
            "Failed to start queued assistant follow-up after failed scheduled run"
        );
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
        // Thinking alone doesn't count as user-visible output — it's
        // the model's internal reasoning, not a deliverable.
        ContentPart::Thinking { .. } => false,
        // A user-attached image is visible, deliverable content.
        ContentPart::Image { .. } => true,
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
}

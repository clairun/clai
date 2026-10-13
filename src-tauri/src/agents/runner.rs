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
use crate::assistant::dispatcher;
use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::events::{emit_event, AssistantUiEvent, RunSkipReason};
use crate::assistant::repository::{self, CreateRunParams};
use crate::assistant::runtime;
use crate::assistant::types::{
    AssistantRun, AssistantSession, ProviderConnection, RunStatus, RunTrigger,
};
use crate::config::{workspace_config, AgentConfig};
use crate::db::DbPool;
use crate::AppState;

/// Loads a workspace-local agent from `<workspace>/.clai/config.json`.
///
/// Reads only `workspace_id`'s config, so another workspace's unreadable
/// config cannot stop this one's scheduled runs.
fn load_workspace_agent_as_config(
    state: &AppState,
    workspace_id: &str,
    id: &str,
) -> Result<Option<AgentConfig>, RunnerError> {
    let app_config = state
        .config_manager
        .lock()
        .map_err(|e| RunnerError::AssistantPersistence(format!("Lock error: {}", e)))?
        .get();
    let (config, roster) = state.resolve_workspace_roster(workspace_id).map_err(|e| {
        RunnerError::AssistantPersistence(format!("Failed to load workspace config: {}", e))
    })?;
    let Some(agent) = roster
        .iter()
        .map(|resolved| &resolved.agent)
        .find(|agent| agent.id == id)
    else {
        return Ok(None);
    };
    let created_at = chrono::DateTime::from_timestamp_millis(agent.created_at)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    let updated_at = chrono::DateTime::from_timestamp_millis(agent.updated_at)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    let is_manager = agent.id == config.main_agent_id();
    Ok(Some(AgentConfig {
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
        provider_connection_ids: agent.provider_connection_ids.clone(),
        selected_skill_ids: workspace_config::refs_to_skill_ids(
            &app_config,
            &agent.selected_skills,
        ),
        created_at,
        updated_at,
    }))
}

// =============================================================================
// Configuration
// =============================================================================

/// How often to check for ready agents (in seconds).
const CHECK_INTERVAL_SECS: u64 = 5;

/// How long a scheduled fire that failed to start (no enabled provider,
/// config or DB error, ...) waits before it is retried.
const START_FAILURE_RETRY_MS: i64 = 5 * 60 * 1000;

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
    let Some(instance_id) = claim_next_instance(scheduler).await else {
        tracing::debug!("No agents ready");
        return Ok(());
    };
    release_slot_on_error(
        scheduler,
        &instance_id,
        run_claimed_instance(app_handle, scheduler, &instance_id),
    )
    .await
}

/// Claims the next ready instance, marking it running in the scheduler.
async fn claim_next_instance(scheduler: &SharedScheduler) -> Option<String> {
    let mut sched = scheduler.lock().await;

    let instance_count = sched.instance_count();
    let state = sched.state();
    let running_agent = sched.running_instance();
    tracing::debug!(
        instance_count,
        scheduler_state = ?state,
        running_agent = ?running_agent,
        "Scheduler status"
    );

    if matches!(state, SchedulerState::Paused { .. }) {
        tracing::debug!("Scheduler is paused");
        return None;
    }

    sched.next_ready()
}

/// Awaits `body` for a claimed instance and, if it fails while the instance
/// still holds the scheduler's single running slot, releases the slot with a
/// retry after [`START_FAILURE_RETRY_MS`]. Without this an early error left
/// the instance marked running, blocking every later scheduled run. A missing
/// agent is retried too rather than removed: a failed workspace delete
/// restores the workspace and expects its scheduler entries intact, while a
/// successful delete removes them itself. The error is returned unchanged for
/// the runner loop to log.
async fn release_slot_on_error(
    scheduler: &SharedScheduler,
    instance_id: &str,
    body: impl std::future::Future<Output = Result<(), RunnerError>>,
) -> Result<(), RunnerError> {
    let result = body.await;
    if result.is_err() {
        let mut sched = scheduler.lock().await;
        if sched.running_instance().map(String::as_str) == Some(instance_id) {
            let retry_at = chrono::Utc::now().timestamp_millis() + START_FAILURE_RETRY_MS;
            sched.complete_agent(instance_id, false, retry_at);
        }
    }
    result
}

/// Runs an instance already claimed by [`claim_next_instance`]. Every `Ok`
/// path releases the scheduler slot itself; errors are released by
/// [`release_slot_on_error`].
#[allow(
    clippy::cognitive_complexity,
    reason = "lint debt: cognitive complexity 85 against a budget of 25"
)]
async fn run_claimed_instance(
    app_handle: &AppHandle,
    scheduler: &SharedScheduler,
    instance_id: &str,
) -> Result<(), RunnerError> {
    let state = app_handle.state::<AppState>();

    tracing::info!(instance_id = %instance_id, "Running agent");

    // Get the instance details
    let (workspace_id, agent_id) = {
        let sched = scheduler.lock().await;
        let instance = sched
            .get_instance(instance_id)
            .ok_or_else(|| RunnerError::InstanceNotFound(instance_id.to_string()))?;

        (instance.workspace_id.clone(), instance.agent_id.clone())
    };

    tracing::debug!(agent_id = %agent_id, "Got agent instance");

    let agent_config = load_workspace_agent_as_config(state.inner(), &workspace_id, &agent_id)?
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
            sched.remove_instance(instance_id);
        }

        return Ok(());
    }

    tracing::debug!(agent_name = %agent_config.name, "Got agent config");

    let connection = resolve_agent_connection(state.inner(), &agent_config)?;

    let session =
        ensure_workspace_manager_session(app_handle, &workspace_pool, &agent_config).await?;

    let manual = scheduler
        .lock()
        .await
        .get_instance(instance_id)
        .is_some_and(|instance| instance.manual_run_pending);
    let run = start_tick_run(
        &workspace_pool,
        &session,
        &connection,
        TickContext {
            instance_id,
            agent_id: &agent_id,
            manual,
        },
        |event| {
            let _ = emit_event(app_handle, &workspace_id, &session, None, event);
        },
    )
    .await?;
    let result = match run {
        Some(run) => {
            run_scheduled_agent(
                app_handle,
                &agent_config.workspace_id,
                &workspace_pool,
                &session,
                &connection,
                run,
            )
            .await
        }
        None => Ok(()),
    };

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
    let next_run_at_unix_ms = complete_tick(scheduler, instance_id, success, &agent_config).await;

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

/// Releases the scheduler slot and schedules the next fire. Returns the next
/// fire time, `None` when the schedule cannot compute one.
async fn complete_tick(
    scheduler: &SharedScheduler,
    instance_id: &str,
    success: bool,
    agent_config: &AgentConfig,
) -> Option<i64> {
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
    let mut sched = scheduler.lock().await;
    // When we can't compute a next time, pass `i64::MAX` so the
    // scheduler parks the instance indefinitely. The user has to
    // fix the schedule via the UI before it ticks again.
    sched.complete_agent(
        instance_id,
        success,
        next_run_at_unix_ms.unwrap_or(i64::MAX),
    );

    // Log next run time
    if let Some(instance) = sched.get_instance(instance_id) {
        let seconds_until_next = instance.seconds_until_next_run();
        tracing::info!(
            instance_id = %instance_id,
            seconds_until_next,
            minutes_until_next = seconds_until_next / 60,
            "Scheduled next agent run"
        );
    }
    next_run_at_unix_ms
}

/// Which scheduler tick [`start_tick_run`] is starting.
struct TickContext<'a> {
    instance_id: &'a str,
    agent_id: &'a str,
    /// A user-requested "Run now" rather than a scheduled fire.
    manual: bool,
}

/// Creates the tick's run, or skips the tick when the session already has an
/// active run: a busy tick is dropped, never queued or deferred, and the
/// schedule advances as usual. A skipped manual run is reported through
/// `emit` because a user is waiting on it.
async fn start_tick_run(
    pool: &DbPool,
    session: &AssistantSession,
    connection: &ProviderConnection,
    tick: TickContext<'_>,
    emit: impl FnOnce(AssistantUiEvent),
) -> Result<Option<AssistantRun>, RunnerError> {
    let run = repository::create_run_if_idle(
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
    if run.is_none() {
        let active_run_id = repository::active_run_id(pool, &session.id)
            .await
            .map_err(RunnerError::AssistantPersistence)?;
        tracing::info!(
            instance_id = tick.instance_id,
            agent_id = tick.agent_id,
            session_id = %session.id,
            manual = tick.manual,
            "session has an active run; skipping tick"
        );
        if tick.manual {
            emit(AssistantUiEvent::RunSkipped {
                reason: RunSkipReason::SessionBusy,
                active_run_id,
            });
        }
    }
    Ok(run)
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
    let session = repository::find_or_create_manager_session(pool, &agent_config.id)
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

    let connection = crate::config::first_enabled_connection(
        &agent_config.provider_connection_ids,
        &all_connections,
    )
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
    session: &AssistantSession,
    connection: &ProviderConnection,
    run: AssistantRun,
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

    // Registered like every other run, so `assistant_cancel_run(workspace_id,
    // run.id)` and mid-run delivery reach it.
    let run_registration =
        runtime::register_run(runtime::LiveRunKey::new(workspace_id, &session.id), &run.id);
    let cancel_token = run_registration.token();
    let input = RunTurnInput {
        session_id: session.id.clone(),
        run_id: Some(run.id.clone()),
        trigger: RunTrigger::Scheduled,
        connection_id: connection.id.clone(),
        cancel_token,
        trigger_message_id: None,
    };
    let result = engine::run_session_turn(&deps, input).await;
    drop(run_registration);

    dispatcher::kick_logged(&deps, &session.id).await;

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

    #[tokio::test]
    async fn failed_start_releases_scheduler_slot() {
        let scheduler = crate::agents::create_shared_scheduler();
        let instance_id = {
            let mut sched = scheduler.lock().await;
            sched.register_definition(crate::agents::AgentDefinition::new("agent-1", "Agent"));
            sched.create_instance("agent-1").unwrap()
        };

        let claimed = claim_next_instance(&scheduler).await;
        assert_eq!(claimed.as_deref(), Some(instance_id.as_str()));

        let result = release_slot_on_error(&scheduler, &instance_id, async {
            Err(RunnerError::NoProviderConfigured("agent-1".to_string()))
        })
        .await;
        assert!(matches!(result, Err(RunnerError::NoProviderConfigured(_))));

        let mut sched = scheduler.lock().await;
        assert!(!sched.is_running());
        let instance = sched.get_instance(&instance_id).unwrap();
        assert!(!instance.is_running);
        // Retried later, not hot-looped on every tick.
        assert!(instance.next_run_at.is_some());
        assert_eq!(sched.next_ready(), None);

        sched.set_instance_next_run_at(&instance_id, None);
        assert_eq!(sched.next_ready(), Some(instance_id));
    }

    #[tokio::test]
    async fn start_for_missing_agent_keeps_instance_for_retry() {
        let scheduler = crate::agents::create_shared_scheduler();
        let instance_id = {
            let mut sched = scheduler.lock().await;
            sched.register_definition(crate::agents::AgentDefinition::new("agent-1", "Agent"));
            sched.create_instance("agent-1").unwrap()
        };
        claim_next_instance(&scheduler).await;

        let result = release_slot_on_error(&scheduler, &instance_id, async {
            Err(RunnerError::AgentNotFound("agent-1".to_string()))
        })
        .await;
        assert!(matches!(result, Err(RunnerError::AgentNotFound(_))));

        let sched = scheduler.lock().await;
        assert!(!sched.is_running());
        let instance = sched.get_instance(&instance_id).unwrap();
        assert!(instance.next_run_at.is_some());
    }

    // The scheduled agent is looked up in its own workspace only: another
    // workspace's unreadable config must not stop it from starting.
    #[test]
    fn scheduled_agent_resolves_despite_another_unreadable_workspace() {
        use crate::config::workspace_config::WorkspaceConfig;
        use crate::workspace_index::WorkspaceIndex;

        let temp = tempfile::tempdir().expect("temp dir");
        let parent = temp.path().join("workspaces");
        let mut index = WorkspaceIndex::default();
        let mut sched = crate::agents::scheduler::Scheduler::new();
        for (id, main, updated_at) in [
            ("ws-broken", "main-broken", 2_000),
            ("ws-ok", "main-ok", 1_000),
        ] {
            let root = parent.join(id);
            let mut config =
                WorkspaceConfig::new(id.to_string(), id.to_string(), updated_at, main.to_string());
            config.schedule.enabled = true;
            workspace_config::save(&root, &config).expect("seed workspace");
            index.insert_config(root, &config);
            crate::agents::init::apply_workspace_schedule(&mut sched, &config);
        }
        std::fs::write(
            workspace_config::config_path(&parent.join("ws-broken")),
            "{ not json",
        )
        .expect("corrupt config");
        let config_manager = crate::ConfigManager::new_for_tests(
            crate::config::AppConfig::default(),
            temp.path().join("config.json"),
        );
        let state = AppState::new_for_tests(config_manager, index).expect("app state");

        let instance = sched
            .get_instance(&crate::agents::types::AgentInstance::instance_id_for(
                "main-ok",
            ))
            .expect("healthy workspace is scheduled");
        let agent =
            load_workspace_agent_as_config(&state, &instance.workspace_id, &instance.agent_id)
                .expect("healthy workspace config loads")
                .expect("scheduled agent resolves");
        assert_eq!(agent.workspace_id, "ws-ok");
        assert!(agent.schedule_enabled);
    }

    mod busy_tick {
        use super::*;
        use crate::agents::scheduler::Scheduler;
        use crate::agents::AgentDefinition;
        use crate::assistant::types::{AuthMode, SessionContext};
        use crate::config::workspace_config::ScheduleKind;
        use crate::db::test_support::workspace_pool;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        fn connection() -> ProviderConnection {
            ProviderConnection {
                id: "conn".into(),
                name: "conn".into(),
                protocol_id: "openai".into(),
                provider_id: "openai".into(),
                auth_mode: AuthMode::DeveloperApiKey,
                base_url: None,
                secret_ref: String::new(),
                model_id: "model".into(),
                account_label: None,
                enabled: true,
                created_at: 0,
                updated_at: 0,
            }
        }

        /// A claimed tick of a manager session that already has an active run.
        async fn busy_tick(
            manual: bool,
        ) -> (
            tempfile::TempDir,
            DbPool,
            AssistantSession,
            AssistantRun,
            SharedScheduler,
            String,
        ) {
            let (tmp, pool) = workspace_pool().await;
            let session = repository::create_session(&pool, SessionContext::default())
                .await
                .unwrap();
            let active = repository::create_run(
                &pool,
                CreateRunParams {
                    session_id: session.id.clone(),
                    status: RunStatus::Running,
                    trigger: RunTrigger::SignalWake,
                    connection_id: "conn".into(),
                    protocol_id: "openai".into(),
                    model_id: "model".into(),
                    error: None,
                },
            )
            .await
            .unwrap();
            let mut scheduler = Scheduler::new();
            scheduler.register_definition(AgentDefinition::new("agent", "Agent"));
            let instance_id = scheduler.create_instance("agent").unwrap();
            if manual {
                scheduler.set_instance_enabled(&instance_id, false);
                assert!(scheduler.force_ready("agent"));
            }
            assert_eq!(scheduler.next_ready(), Some(instance_id.clone()));
            (
                tmp,
                pool,
                session,
                active,
                Arc::new(Mutex::new(scheduler)),
                instance_id,
            )
        }

        struct Skipped {
            _tmp: tempfile::TempDir,
            pool: DbPool,
            active: AssistantRun,
            scheduler: SharedScheduler,
            instance_id: String,
            events: Vec<AssistantUiEvent>,
        }

        /// Runs a busy tick through the runner's start and completion steps.
        async fn skip(manual: bool) -> Skipped {
            let (tmp, pool, session, active, scheduler, instance_id) = busy_tick(manual).await;
            let mut events = Vec::new();
            let run = start_tick_run(
                &pool,
                &session,
                &connection(),
                TickContext {
                    instance_id: &instance_id,
                    agent_id: "agent",
                    manual,
                },
                |event| events.push(event),
            )
            .await
            .unwrap();
            assert!(run.is_none(), "a busy session must not get a second run");

            let mut agent = AgentConfig::new(
                "Agent".into(),
                String::new(),
                ScheduleKind::Interval {
                    interval_minutes: 30,
                },
            );
            agent.workspace_id = "ws".into();
            let before = chrono::Utc::now().timestamp_millis();
            let next = complete_tick(&scheduler, &instance_id, true, &agent)
                .await
                .unwrap();
            assert!(
                next > before,
                "the schedule must advance past the skipped tick"
            );
            Skipped {
                _tmp: tmp,
                pool,
                active,
                scheduler,
                instance_id,
                events,
            }
        }

        async fn counts(pool: &DbPool) -> (i64, i64) {
            sqlx::query_as(
                "SELECT (SELECT COUNT(*) FROM assistant_runs), (SELECT COUNT(*) FROM session_signals)",
            )
            .fetch_one(pool)
            .await
            .unwrap()
        }

        #[tokio::test]
        async fn scheduled_tick_skipped_when_session_run_active() {
            let tick = skip(false).await;

            assert_eq!(counts(&tick.pool).await, (1, 0), "no second run, no signal");
            assert!(tick.events.is_empty(), "a scheduled skip emits nothing");
            let sched = tick.scheduler.lock().await;
            assert!(!sched.is_running());
            let instance = sched.get_instance(&tick.instance_id).unwrap();
            assert!(instance.seconds_until_next_run() > 0);
        }

        #[tokio::test]
        async fn manual_run_now_on_busy_session_skips_clears_flag_and_emits_run_skipped() {
            let tick = skip(true).await;

            assert_eq!(counts(&tick.pool).await, (1, 0), "no second run, no signal");
            assert!(matches!(
                tick.events.as_slice(),
                [AssistantUiEvent::RunSkipped {
                    reason: RunSkipReason::SessionBusy,
                    active_run_id: Some(id),
                }] if *id == tick.active.id
            ));
            let sched = tick.scheduler.lock().await;
            assert!(!sched.is_running());
            assert!(
                !sched
                    .get_instance(&tick.instance_id)
                    .unwrap()
                    .manual_run_pending
            );
        }
    }
}

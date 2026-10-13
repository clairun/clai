use crate::assistant::dispatcher;
use crate::assistant::engine::AssistantDeps;
use crate::assistant::events::AssistantUiEvent;
use crate::assistant::repository;
use crate::assistant::runtime::{self, LiveRunKey};
use crate::assistant::signals::{self, NewEvent, SignalKind};
use crate::assistant::tools::ToolExecutionContext;
use crate::assistant::types::{
    AgentRefSnapshot, AssistantMessage, AssistantRun, AssistantSession, ContentPart, MessageOrigin,
    MessageRole, ProviderConnection, RunStatus, SessionContext,
};
use crate::assistant::workspace_agents::{
    agent_display_names, concise_agent_description, load_workspace_agent_rows,
    workspace_agent_display_names, WorkspaceAgentRow,
};
use crate::assistant::workspace_tasks_repo::{
    self, create_task_row, CycleOutcome, FailureReason, NewTask, TaskStatus, WorkspaceTaskRow,
};
use crate::config::first_enabled_connection;
use crate::db::{enum_text, DbPool};
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListWorkspaceAgentsParams {
    #[serde(default)]
    pub include_disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignWorkspaceTaskParams {
    /// The target agent's workspace agent id, exactly as shown in the roster.
    /// A display name is rejected.
    pub workspace_agent_id: String,
    pub title: String,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetWorkspaceTaskResultParams {
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopWorkspaceTaskParams {
    pub task_id: String,
}

pub async fn execute(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match tool_name {
        "workspace_listAgents" => {
            let params: ListWorkspaceAgentsParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_listAgents params: {}", e))?;
            list_agents(deps, params)
        }
        "workspace_assignTask" => {
            let params: AssignWorkspaceTaskParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_assignTask params: {}", e))?;
            assign_task(deps, context, params).await
        }
        "workspace_getTaskResult" => {
            let params: GetWorkspaceTaskResultParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_getTaskResult params: {}", e))?;
            get_task_result(deps, params).await
        }
        "workspace_stopTask" => {
            let params: StopWorkspaceTaskParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_stopTask params: {}", e))?;
            let task = match stop_task(deps, &params.task_id).await? {
                StopOutcome::AlreadyTerminal(task) => {
                    return Err(format!(
                        "already_terminal: task {} already ended as {}; nothing to stop.",
                        task.id,
                        enum_text(&task.status)?
                    ))
                }
                outcome => outcome.into_task(),
            };
            let agent_names = workspace_agent_display_names(
                deps.app.state::<AppState>().inner(),
                &deps.workspace_id,
            );
            Ok(serde_json::json!({
                "ok": true,
                "task": task.into_response(&deps.workspace_id, &agent_names),
            }))
        }
        _ => Err(format!("Unknown workspace task tool: {}", tool_name)),
    }
}

fn list_agents(
    deps: &AssistantDeps,
    params: ListWorkspaceAgentsParams,
) -> Result<serde_json::Value, String> {
    let workspace_id = deps.workspace_id.clone();
    let app_state = deps.app.state::<AppState>();
    let rows = load_workspace_agent_rows(app_state.inner(), &workspace_id)?;

    let agents: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| params.include_disabled || row.enabled)
        .map(|row| {
            serde_json::json!({
                "id": row.id,
                "workspaceId": row.workspace_id,
                "agentDefinitionId": row.agent_definition_id,
                "displayName": row.display_name(),
                "role": row.role,
                "enabled": row.enabled,
                "description": concise_agent_description(Some(row.description.clone())),
                "providerConnectionIds": row.provider_connection_ids,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "ok": true,
        "workspaceId": workspace_id,
        "agents": agents,
    }))
}

async fn assign_task(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    params: AssignWorkspaceTaskParams,
) -> Result<serde_json::Value, String> {
    let workspace_id = deps.workspace_id.clone();
    let title = params.title.trim();
    let instructions = params.instructions.trim();
    if title.is_empty() {
        return Err("Task title is required.".to_string());
    }
    if instructions.is_empty() {
        return Err("Task instructions are required.".to_string());
    }

    let app_state = deps.app.state::<AppState>();
    let rows = load_workspace_agent_rows(app_state.inner(), &workspace_id)?;
    let target = rows
        .iter()
        .find(|row| row.id == params.workspace_agent_id)
        .cloned()
        .ok_or_else(|| unknown_workspace_agent_error(&params.workspace_agent_id, &rows))?;

    if !target.enabled {
        return Err(disabled_workspace_agent_error(
            &params.workspace_agent_id,
            &rows,
        ));
    }

    let automation_id = context.run_context.automation_id.as_deref();
    let creator = creator_workspace_agent_id(&rows, automation_id);
    let lineage =
        workspace_tasks_repo::lineage_for_caller(&deps.pool, &context.session_id, &context.run_id)
            .await?;

    let task_id = uuid::Uuid::new_v4().to_string();
    create_task_row(
        &deps.pool,
        NewTask {
            id: &task_id,
            created_by_workspace_agent_id: creator,
            assigned_to_workspace_agent_id: &target.id,
            assigned_agent_definition_id: &target.agent_definition_id,
            title,
            instructions,
            lineage,
        },
    )
    .await?;

    let connection = match resolve_first_connection(deps, &target) {
        Ok(connection) => connection,
        Err(message) => {
            workspace_tasks_repo::fail_unstarted(&deps.pool, &task_id, &message).await?;
            return task_result(deps, &task_id, &agent_display_names(&rows)).await;
        }
    };

    let caller = agent_snapshot(&rows, automation_id.unwrap_or("unknown"), None);
    let session = enqueue_assignment(
        &deps.pool,
        &task_id,
        &target.id,
        assignment_text(&task_id, title, instructions),
        MessageOrigin::Agent {
            agent: caller,
            session_id: Some(context.session_id.clone()),
            run_id: Some(context.run_id.clone()),
        },
        &connection.id,
    )
    .await?;

    // Read before the kick so the caller gets the queued placeholder.
    let result = task_result(deps, &task_id, &agent_display_names(&rows)).await;
    dispatcher::kick_logged(deps, &session.id).await;
    result
}

/// Creates the task's session and posts the instruction to it as a pending
/// `agent_message`; the next kick starts the first cycle.
pub(crate) async fn enqueue_assignment(
    pool: &DbPool,
    task_id: &str,
    assignee_id: &str,
    instruction: String,
    origin: MessageOrigin,
    connection_id: &str,
) -> Result<AssistantSession, String> {
    // Insert the session and stamp its id on the workspace_tasks row in one
    // SQLite transaction so a concurrent resolver cannot observe the session
    // before the task row links to it (see create_session_and_link_task).
    let session =
        repository::create_session_and_link_task(pool, task_session_context(assignee_id), task_id)
            .await?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start task assignment transaction: {}", e))?;
    signals::insert_event(
        &mut tx,
        NewEvent {
            session_id: &session.id,
            kind: SignalKind::AgentMessage,
            text: instruction,
            origin,
            payload: serde_json::json!({ "taskId": task_id, "assignment": true }),
            delivery_key: None,
            connection_id: Some(connection_id),
        },
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit task assignment transaction: {}", e))?;
    Ok(session)
}

fn assignment_text(task_id: &str, title: &str, instructions: &str) -> String {
    format!(
        "You have been assigned a workspace-local task by the manager.\n\n\
         Task ID: {}\n\
         Task title: {}\n\n\
         Instructions:\n{}\n\n\
         Work only with the current workspace context and available tools. \
         Return a concise result summary. If blocked by missing capability, context, permission, or runtime failure, start your response with `BLOCKED:` and describe the blocker. If you need a human decision, call the `ask_user` tool directly — do not surface the question via your text response.",
        task_id, title, instructions
    )
}

async fn get_task_result(
    deps: &AssistantDeps,
    params: GetWorkspaceTaskResultParams,
) -> Result<serde_json::Value, String> {
    let agent_names =
        workspace_agent_display_names(deps.app.state::<AppState>().inner(), &deps.workspace_id);
    task_result(deps, &params.task_id, &agent_names).await
}

async fn task_result(
    deps: &AssistantDeps,
    task_id: &str,
    agent_names: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let task = load_task(&deps.pool, task_id).await?;
    Ok(serde_json::json!({
        "ok": true,
        "task": task.into_response(&deps.workspace_id, agent_names),
    }))
}

async fn load_task(
    pool: &DbPool,
    task_id: &str,
) -> Result<workspace_tasks_repo::WorkspaceTaskRow, String> {
    workspace_tasks_repo::load_task(pool, task_id)
        .await?
        .ok_or_else(|| format!("Workspace task not found: {}", task_id))
}

/// Emits `task_updated` for `task` on its own session.
pub(crate) async fn emit_task_updated(deps: &AssistantDeps, task: WorkspaceTaskRow) {
    let Some(session_id) = task.session_id.clone() else {
        return;
    };
    let Ok(Some(session)) = repository::get_session(&deps.pool, &session_id).await else {
        return;
    };
    let run_id = task.run_id.clone();
    let agent_names =
        workspace_agent_display_names(deps.app.state::<AppState>().inner(), &deps.workspace_id);
    let _ = deps.emit(
        &session,
        run_id.as_deref(),
        AssistantUiEvent::TaskUpdated {
            task: Box::new(task.into_response(&deps.workspace_id, &agent_names)),
        },
    );
}

/// `queued → running` as the cycle's run starts.
pub(crate) async fn start_task_cycle(deps: &AssistantDeps, task_id: &str, run_id: &str) {
    match workspace_tasks_repo::mark_running(&deps.pool, task_id, run_id).await {
        Ok(Some(task)) => emit_task_updated(deps, task).await,
        Ok(None) => {}
        Err(error) => tracing::error!(task_id, run_id, error, "Failed to mark task running"),
    }
}

/// Ends the cycle of `run_id`, posts its result to the parent session and
/// wakes the parent.
pub(crate) async fn finalize_task_cycle(deps: &AssistantDeps, task_id: &str, run_id: &str) {
    let roster =
        load_workspace_agent_rows(deps.app.state::<AppState>().inner(), &deps.workspace_id)
            .unwrap_or_default();
    match finish_task_cycle(&deps.pool, task_id, Some(run_id), &roster).await {
        Ok(Some(finished)) => announce_finished(deps, finished).await,
        Ok(None) => {}
        Err(error) => tracing::error!(task_id, run_id, error, "Failed to finalize task cycle"),
    }
}

/// Emits a finished cycle and wakes the parent with its result.
async fn announce_finished(deps: &AssistantDeps, finished: FinishedCycle) {
    emit_task_updated(deps, finished.task).await;
    let Some(message) = finished.result else {
        return;
    };
    if let Ok(Some(parent)) = repository::get_session(&deps.pool, &message.session_id).await {
        let _ = deps.emit(
            &parent,
            None,
            AssistantUiEvent::MessageCreated {
                message: message.clone(),
            },
        );
    }
    dispatcher::kick_logged(deps, &message.session_id).await;
}

#[derive(Debug, Clone)]
pub(crate) struct FinishedCycle {
    pub(crate) task: WorkspaceTaskRow,
    /// The result message written to the parent session.
    pub(crate) result: Option<AssistantMessage>,
}

/// The database half of [`finalize_task_cycle`]: the task's terminal update
/// and its result event, in one transaction. `None` when the cycle was
/// already finalized.
pub(crate) async fn finish_task_cycle(
    pool: &DbPool,
    task_id: &str,
    run_id: Option<&str>,
    roster: &[WorkspaceAgentRow],
) -> Result<Option<FinishedCycle>, String> {
    let run = match run_id {
        Some(run_id) => repository::get_run(pool, run_id).await,
        None => Ok(None),
    };
    let summary = match &run {
        Ok(Some(run)) => final_assistant_text(&repository::list_messages_for_run(pool, run).await?),
        _ => None,
    };
    let outcome = cycle_outcome(run, summary);

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start task finalize transaction: {}", e))?;
    let Some(task) = workspace_tasks_repo::finish_cycle(&mut tx, task_id, run_id, &outcome).await?
    else {
        return Ok(None);
    };
    let result = match &task.parent_session_id {
        Some(parent_session_id) => {
            signals::insert_event(&mut tx, result_event(&task, parent_session_id, roster)?).await?
        }
        None => None,
    };
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit task finalize transaction: {}", e))?;
    Ok(Some(FinishedCycle { task, result }))
}

/// The automated event that tells the parent how a cycle ended.
fn result_event<'a>(
    task: &WorkspaceTaskRow,
    parent_session_id: &'a str,
    roster: &[WorkspaceAgentRow],
) -> Result<NewEvent<'a>, String> {
    let (kind, text, payload) = match task.failure_reason {
        Some(reason) => (
            SignalKind::TaskFailed,
            format!(
                "Failure reason: {}\n\n{}",
                enum_text(&reason)?,
                task.error.as_deref().unwrap_or("(no error text)")
            ),
            serde_json::json!({
                "taskId": task.id,
                "cycleNo": task.cycle_no,
                "failureReason": reason,
                "error": task.error,
            }),
        ),
        None => (
            SignalKind::TaskResult,
            task.result_summary
                .clone()
                .or_else(|| task.error.clone())
                .unwrap_or_else(|| "(no output)".to_string()),
            serde_json::json!({
                "taskId": task.id,
                "cycleNo": task.cycle_no,
                "status": task.status,
                "title": task.title,
            }),
        ),
    };
    Ok(NewEvent {
        session_id: parent_session_id,
        kind,
        text,
        origin: MessageOrigin::Task {
            task_id: task.id.clone(),
            agent: agent_snapshot(
                roster,
                &task.assigned_to_workspace_agent_id,
                Some(&task.assigned_agent_definition_id),
            ),
            title: task.title.clone(),
            cycle_no: task.cycle_no,
            status: task.status,
        },
        payload,
        delivery_key: Some(format!("task-result:{}:{}", task.id, task.cycle_no)),
        connection_id: None,
    })
}

/// `id` as the roster shows it now; an agent off the roster keeps its ids.
fn agent_snapshot(
    rows: &[WorkspaceAgentRow],
    id: &str,
    definition_id: Option<&str>,
) -> Box<AgentRefSnapshot> {
    Box::new(match rows.iter().find(|row| row.id == id) {
        Some(row) => AgentRefSnapshot {
            workspace_agent_id: row.id.clone(),
            agent_definition_id: Some(row.agent_definition_id.clone()),
            display_name: row.display_name().to_string(),
            is_default: row.role == "manager",
            avatar: row.avatar.clone(),
        },
        None => AgentRefSnapshot {
            workspace_agent_id: id.to_string(),
            agent_definition_id: definition_id.map(str::to_string),
            display_name: definition_id.unwrap_or(id).to_string(),
            is_default: false,
            avatar: None,
        },
    })
}

/// How a cycle ended, from its run and the run's final text. A run that is
/// missing or still active fails the task: nothing advances it after this.
fn cycle_outcome(
    run: Result<Option<AssistantRun>, String>,
    summary: Option<String>,
) -> CycleOutcome {
    let failed = |reason, error: String| CycleOutcome {
        status: TaskStatus::Failed,
        failure_reason: Some(reason),
        error: Some(error),
        result_summary: summary.clone(),
    };
    let run = match run {
        Ok(Some(run)) => run,
        Ok(None) => {
            return failed(
                FailureReason::Error,
                "Worker run record not found".to_string(),
            )
        }
        Err(error) => {
            return failed(
                FailureReason::Error,
                format!("Could not load worker run: {error}"),
            )
        }
    };
    match run.status {
        status @ (RunStatus::Queued | RunStatus::Running | RunStatus::WaitingForTool) => failed(
            FailureReason::Error,
            format!("Worker run ended while still {status:?}"),
        ),
        RunStatus::Cancelled => CycleOutcome {
            status: TaskStatus::Stopped,
            failure_reason: None,
            error: run.error,
            result_summary: summary,
        },
        RunStatus::Completed | RunStatus::CompletedWithWarnings | RunStatus::Failed => {
            if let Some(reply) = summary.as_deref().filter(|text| is_blocked_reply(text)) {
                failed(FailureReason::Blocked, reply.to_string())
            } else if run.status == RunStatus::Failed {
                failed(
                    FailureReason::Error,
                    run.error.unwrap_or_else(|| "Worker run failed".to_string()),
                )
            } else {
                CycleOutcome {
                    status: TaskStatus::Completed,
                    failure_reason: None,
                    error: run.error,
                    result_summary: summary,
                }
            }
        }
    }
}

/// Requests a stop of the task's current cycle: `stopping` until the run's
/// teardown finalizes it, `stopped` at once when no run is active.
pub(crate) async fn stop_task(deps: &AssistantDeps, task_id: &str) -> Result<StopOutcome, String> {
    let roster =
        load_workspace_agent_rows(deps.app.state::<AppState>().inner(), &deps.workspace_id)
            .unwrap_or_default();
    let outcome = request_stop(&deps.pool, &deps.workspace_id, task_id, &roster).await?;
    match &outcome {
        StopOutcome::Requested(task) => emit_task_updated(deps, task.clone()).await,
        StopOutcome::Stopped(finished) => announce_finished(deps, finished.clone()).await,
        StopOutcome::AlreadyStopping(_) | StopOutcome::AlreadyTerminal(_) => {}
    }
    Ok(outcome)
}

#[derive(Debug)]
pub(crate) enum StopOutcome {
    /// Now `stopping`: the live run was cancelled, or the run about to start
    /// sees the request before it does (`dispatcher::spawn_run`).
    Requested(WorkspaceTaskRow),
    /// No run was active: the cycle ended `stopped` here.
    Stopped(FinishedCycle),
    AlreadyStopping(WorkspaceTaskRow),
    AlreadyTerminal(WorkspaceTaskRow),
}

impl StopOutcome {
    pub(crate) fn into_task(self) -> WorkspaceTaskRow {
        match self {
            Self::Requested(task) | Self::AlreadyStopping(task) | Self::AlreadyTerminal(task) => {
                task
            }
            Self::Stopped(finished) => finished.task,
        }
    }
}

/// The database and registry half of [`stop_task`]. The request is written
/// before the registry is read, and the dispatcher registers a run before it
/// reads the request, so one of the two always sees the other.
pub(crate) async fn request_stop(
    pool: &DbPool,
    workspace_id: &str,
    task_id: &str,
    roster: &[WorkspaceAgentRow],
) -> Result<StopOutcome, String> {
    let (task, changed) = workspace_tasks_repo::request_stop(pool, task_id)
        .await?
        .ok_or_else(|| format!("Workspace task not found: {}", task_id))?;
    if !changed {
        return Ok(if task.status.is_terminal() {
            StopOutcome::AlreadyTerminal(task)
        } else {
            StopOutcome::AlreadyStopping(task)
        });
    }
    if let Some(session_id) = &task.session_id {
        if runtime::cancel_session(&LiveRunKey::new(workspace_id, session_id))
            || repository::active_run_id(pool, session_id).await?.is_some()
        {
            return Ok(StopOutcome::Requested(task));
        }
    }
    Ok(
        match finish_task_cycle(pool, &task.id, task.run_id.as_deref(), roster).await? {
            Some(finished) => StopOutcome::Stopped(finished),
            // The run's own teardown finalized the cycle first.
            None => StopOutcome::Requested(load_task(pool, &task.id).await?),
        },
    )
}

/// Restart recovery, once per process per workspace database, before
/// `recover_stale_runs`: input of runs that never started is handed back,
/// interrupted tasks end `stopped`, and each stopped task's parent gets the
/// usual result event (one per cycle, by delivery key). Emits nothing: no
/// window is listening yet.
pub(crate) async fn recover_interrupted_tasks(pool: &DbPool) -> Result<(), String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start task recovery transaction: {}", e))?;
    signals::release_claims_of_unstarted_runs(&mut tx).await?;
    let stopped = workspace_tasks_repo::stop_interrupted(&mut tx).await?;
    for task in &stopped {
        if let Some(parent_session_id) = &task.parent_session_id {
            signals::insert_event(&mut tx, result_event(task, parent_session_id, &[])?).await?;
        }
    }
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit task recovery transaction: {}", e))?;
    if !stopped.is_empty() {
        tracing::info!(
            count = stopped.len(),
            "Stopped workspace tasks interrupted by the previous app session"
        );
    }
    Ok(())
}

/// The worker runs with its own configuration, resolved when each run starts —
/// never the caller's capabilities.
pub(crate) fn task_session_context(assignee_id: &str) -> SessionContext {
    SessionContext {
        automation_id: Some(assignee_id.to_string()),
        ..SessionContext::default()
    }
}

fn resolve_first_connection(
    deps: &AssistantDeps,
    agent: &WorkspaceAgentRow,
) -> Result<ProviderConnection, String> {
    let all = deps
        .app
        .state::<AppState>()
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connections();
    assignee_connection(agent, &all)
}

fn assignee_connection(
    agent: &WorkspaceAgentRow,
    connections: &[ProviderConnection],
) -> Result<ProviderConnection, String> {
    first_enabled_connection(&agent.provider_connection_ids, connections)
        .cloned()
        .ok_or_else(|| format!("Agent '{}' has no active provider connection.", agent.name))
}

/// Message for an unknown `workspaceAgentId`.
///
/// The common failure is a caller passing an agent's *display name*, which the
/// system-prompt roster used to be the only thing it showed. The message names
/// that mistake and lists the ids that would have worked, so a caller can
/// recover on the next call instead of guessing or falling back to
/// `workspace_listAgents`.
fn unknown_workspace_agent_error(requested: &str, rows: &[WorkspaceAgentRow]) -> String {
    if rows.is_empty() {
        return format!(
            "Workspace agent assignment not found in this workspace: {requested}. This workspace has no assigned agents."
        );
    }
    let known = rows
        .iter()
        .map(|row| {
            if row.enabled {
                format!("{} ({})", row.id, row.display_name())
            } else {
                format!("{} ({}, disabled)", row.id, row.display_name())
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Workspace agent assignment not found in this workspace: {requested}. \
         `workspaceAgentId` must be one of the workspace agent ids, not a display name. \
         Known ids: {known}"
    )
}

/// Message for an id that exists but is disabled.
///
/// The bare "is disabled" is a dead end: it names no next step. Offer the
/// enabled ids so the caller can redirect the task in the same turn.
fn disabled_workspace_agent_error(requested: &str, rows: &[WorkspaceAgentRow]) -> String {
    let enabled = rows
        .iter()
        .filter(|row| row.enabled)
        .map(|row| format!("{} ({})", row.id, row.display_name()))
        .collect::<Vec<_>>()
        .join(", ");
    if enabled.is_empty() {
        return format!(
            "Workspace agent assignment is disabled: {requested}. No other workspace agent is enabled — enable one in the workspace's agent settings."
        );
    }
    format!(
        "Workspace agent assignment is disabled: {requested}. \
         Enable it in the workspace's agent settings, or assign to one of: {enabled}"
    )
}

/// The caller's own workspace agent id, if it is one: a run's
/// `automation_id` is the workspace agent id of the agent running it.
fn creator_workspace_agent_id<'rows>(
    rows: &'rows [WorkspaceAgentRow],
    automation_id: Option<&str>,
) -> Option<&'rows str> {
    let automation_id = automation_id?;
    rows.iter()
        .find(|row| row.id == automation_id)
        .map(|row| row.id.as_str())
}

/// A worker reply that starts with `BLOCKED:` ends its cycle as
/// `failed{blocked}`.
fn is_blocked_reply(text: &str) -> bool {
    text.trim_start().starts_with("BLOCKED:")
}

/// The last non-empty text the assistant produced in a run, used as the
/// task's result summary and for `BLOCKED:` classification.
///
/// Both messages AND the parts within a message are scanned newest-first.
/// Some providers (Codex via the appserver) accumulate an entire run —
/// opening remark, tool_use items, final verdict — as content parts of a
/// single assistant message. Scanning parts oldest-first there returns the
/// worker's *opening sentence* instead of its final verdict/summary, and
/// makes `BLOCKED:` prefixes in the final text undetectable.
fn final_assistant_text(messages: &[AssistantMessage]) -> Option<String> {
    for message in messages.iter().rev() {
        if message.role != MessageRole::Assistant {
            continue;
        }
        for part in message.content.iter().rev() {
            let ContentPart::Text { text } = part else {
                continue;
            };
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::RunTrigger;

    #[tokio::test]
    async fn task_session_persists_only_assignee_id() {
        let (_tmp, pool) = crate::db::test_support::workspace_pool().await;
        crate::db::test_support::insert_task(&pool, "task-1", "queued", None, None).await;
        let session = repository::create_session_and_link_task(
            &pool,
            task_session_context("assignee"),
            "task-1",
        )
        .await
        .expect("session");
        let stored: String =
            sqlx::query_scalar("SELECT context_json FROM assistant_sessions WHERE id = ?")
                .bind(&session.id)
                .fetch_one(&pool)
                .await
                .expect("context_json");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored).expect("json"),
            serde_json::json!({ "automationId": "assignee" })
        );
    }

    // -----------------------------------------------------------------
    // unknown_workspace_agent_error
    // -----------------------------------------------------------------

    fn agent_row(id: &str, name: &str) -> WorkspaceAgentRow {
        WorkspaceAgentRow {
            id: id.to_string(),
            workspace_id: "workspace-1".to_string(),
            agent_definition_id: format!("def-{id}"),
            role: "member".to_string(),
            enabled: true,
            name: name.to_string(),
            ..WorkspaceAgentRow::default()
        }
    }

    #[test]
    fn assignee_without_enabled_connection_names_agent_in_error() {
        let error = assignee_connection(&agent_row("agent-2", "Code Reviewer"), &[]).unwrap_err();
        assert_eq!(
            error,
            "Agent 'Code Reviewer' has no active provider connection."
        );
    }

    #[test]
    fn unknown_workspace_agent_error_lists_the_ids_that_would_have_worked() {
        let rows = vec![
            agent_row("agent-1", "Manager"),
            agent_row("agent-2", "Code Reviewer"),
        ];

        let message = unknown_workspace_agent_error("Reviewer #2", &rows);

        // The rejected value is echoed so the caller can see what it sent,
        // the display-name mistake is named, and every known id is offered.
        assert!(message.contains("not found in this workspace: Reviewer #2"));
        assert!(message.contains("not a display name"));
        assert!(message.contains("agent-1 (Manager)"));
        assert!(message.contains("agent-2 (Code Reviewer)"));
    }

    #[test]
    fn unknown_workspace_agent_error_marks_disabled_agents() {
        let mut disabled = agent_row("agent-2", "Code Reviewer");
        disabled.enabled = false;
        let rows = vec![agent_row("agent-1", "Manager"), disabled];

        let message = unknown_workspace_agent_error("agent-9", &rows);

        // A disabled agent's id is listed — it is the right id to ask about —
        // but assigning to it fails, so the hint must not call it available.
        assert!(message.contains("agent-2 (Code Reviewer, disabled)"));
        assert!(message.contains("agent-1 (Manager)"));
    }

    #[test]
    fn unknown_workspace_agent_error_without_agents_says_so() {
        let message = unknown_workspace_agent_error("agent-1", &[]);

        assert!(message.contains("no assigned agents"));
        assert!(!message.contains("Known ids:"));
    }

    #[test]
    fn unknown_workspace_agent_error_uses_the_name_the_caller_was_shown() {
        let message = unknown_workspace_agent_error("x", &[agent_row("agent-2", " ")]);

        // A blank name shows as the definition id in the roster and in
        // `workspace_listAgents`, so the hint has to use it too — naming the
        // agent something the caller has never seen breaks the mapping.
        assert!(message.contains("agent-2 (def-agent-2)"));
    }

    #[test]
    fn creator_is_the_calling_workspace_agent_when_it_is_on_the_roster() {
        let rows = vec![agent_row("agent-1", "Manager")];

        assert_eq!(
            creator_workspace_agent_id(&rows, Some("agent-1")),
            Some("agent-1")
        );
        // A definition id is never a run's automation id, so it does not match.
        assert_eq!(creator_workspace_agent_id(&rows, Some("def-agent-1")), None);
        assert_eq!(creator_workspace_agent_id(&rows, Some("gone")), None);
        assert_eq!(creator_workspace_agent_id(&rows, None), None);
    }

    #[test]
    fn disabled_workspace_agent_error_offers_the_enabled_agents() {
        let mut disabled = agent_row("agent-2", "Code Reviewer");
        disabled.enabled = false;
        let rows = vec![agent_row("agent-1", "Manager"), disabled];

        let message = disabled_workspace_agent_error("agent-2", &rows);

        assert!(message.contains("is disabled: agent-2"));
        assert!(message.contains("agent-1 (Manager)"));
        assert!(!message.contains("agent-2 (Code Reviewer)"));
    }

    #[test]
    fn disabled_workspace_agent_error_without_any_enabled_agent_says_so() {
        let mut only = agent_row("agent-1", "Manager");
        only.enabled = false;

        let message = disabled_workspace_agent_error("agent-1", &[only]);

        assert!(message.contains("No other workspace agent is enabled"));
        assert!(!message.contains("assign to one of"));
    }

    // -----------------------------------------------------------------
    // final_assistant_text
    // -----------------------------------------------------------------

    fn msg(role: MessageRole, content: Vec<ContentPart>) -> AssistantMessage {
        AssistantMessage {
            origin: Default::default(),
            id: "m".to_string(),
            session_id: "s".to_string(),
            role,
            content,
            created_at: 0,
            provider_metadata: None,
        }
    }

    fn text(t: &str) -> ContentPart {
        ContentPart::Text {
            text: t.to_string(),
        }
    }

    #[test]
    fn final_assistant_text_empty_returns_none() {
        assert_eq!(final_assistant_text(&[]), None);
    }

    #[test]
    fn final_assistant_text_takes_last_assistant_message() {
        let messages = vec![
            msg(MessageRole::Assistant, vec![text("first reply")]),
            msg(MessageRole::User, vec![text("user says")]),
            msg(MessageRole::Assistant, vec![text("final reply")]),
        ];
        assert_eq!(
            final_assistant_text(&messages),
            Some("final reply".to_string())
        );
    }

    #[test]
    fn final_assistant_text_takes_last_text_part_within_message() {
        // Regression: Codex-style sessions store an entire run as ONE
        // assistant message whose parts are [opening text, tool_use...,
        // final verdict text]. The summary must be the final text part,
        // not the opening remark.
        let messages = vec![msg(
            MessageRole::Assistant,
            vec![
                text("I'm applying the code-review checklist."),
                ContentPart::ToolUse {
                    tool_call_id: "t1".to_string(),
                    tool_name: "bash_exec".to_string(),
                    arguments: serde_json::json!({}),
                },
                text("Verdict: production_quality. No findings."),
            ],
        )];
        assert_eq!(
            final_assistant_text(&messages),
            Some("Verdict: production_quality. No findings.".to_string())
        );
    }

    #[test]
    fn final_assistant_text_skips_trailing_empty_text_parts() {
        let messages = vec![msg(
            MessageRole::Assistant,
            vec![text("real summary"), text("   \n  ")],
        )];
        assert_eq!(
            final_assistant_text(&messages),
            Some("real summary".to_string())
        );
    }

    #[test]
    fn final_assistant_text_blocked_prefix_reaches_classifier() {
        // End-to-end shape of the blocked-worker path for accumulated
        // single-message sessions: the BLOCKED verdict is the LAST part.
        let messages = vec![msg(
            MessageRole::Assistant,
            vec![text("Starting work."), text("BLOCKED: missing API key")],
        )];
        let summary = final_assistant_text(&messages);
        assert!(is_blocked_reply(summary.as_deref().unwrap()));
    }

    #[test]
    fn final_assistant_text_ignores_non_assistant_messages() {
        let messages = vec![
            msg(MessageRole::Assistant, vec![text("assistant text")]),
            msg(MessageRole::Tool, vec![text("tool output")]),
        ];
        assert_eq!(
            final_assistant_text(&messages),
            Some("assistant text".to_string())
        );
    }

    #[test]
    fn is_blocked_reply_needs_the_prefix_after_leading_whitespace() {
        assert!(is_blocked_reply("BLOCKED: Missing API key."));
        assert!(is_blocked_reply("  BLOCKED: Whitespace before."));
        assert!(!is_blocked_reply("NEEDS_USER_INPUT: Please review."));
        assert!(!is_blocked_reply("Task completed successfully."));
        assert!(!is_blocked_reply(""));
    }

    fn run_with(status: RunStatus, error: Option<&str>) -> AssistantRun {
        AssistantRun {
            id: "run".to_string(),
            session_id: "session".to_string(),
            status,
            trigger: RunTrigger::WorkspaceTask,
            connection_id: "c".to_string(),
            protocol_id: "p".to_string(),
            model_id: "m".to_string(),
            started_at: 1,
            completed_at: None,
            error: error.map(str::to_string),
        }
    }

    const BLOCKED: &str = "BLOCKED: need a key";

    #[test]
    fn cycle_outcome_maps_terminal_statuses() {
        for status in [RunStatus::Completed, RunStatus::CompletedWithWarnings] {
            let outcome = cycle_outcome(Ok(Some(run_with(status, None))), Some("done".into()));
            assert_eq!(outcome.status, TaskStatus::Completed);
            assert_eq!(outcome.failure_reason, None);
            assert_eq!(outcome.result_summary.as_deref(), Some("done"));
        }
        let cancelled = cycle_outcome(
            Ok(Some(run_with(RunStatus::Cancelled, None))),
            Some(BLOCKED.to_string()),
        );
        assert_eq!(cancelled.status, TaskStatus::Stopped);
        assert_eq!(cancelled.failure_reason, None);
    }

    #[test]
    fn cycle_outcome_fails_a_run_that_is_still_active_or_missing() {
        for status in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::WaitingForTool,
        ] {
            let outcome =
                cycle_outcome(Ok(Some(run_with(status, None))), Some(BLOCKED.to_string()));
            assert_eq!(outcome.status, TaskStatus::Failed);
            assert_eq!(outcome.failure_reason, Some(FailureReason::Error));
            assert!(outcome.error.unwrap().contains("still"));
        }
        let missing = cycle_outcome(Ok(None), Some(BLOCKED.to_string()));
        assert_eq!(missing.failure_reason, Some(FailureReason::Error));
        assert!(missing.error.unwrap().contains("not found"));
        let unreadable = cycle_outcome(Err("db gone".to_string()), Some(BLOCKED.to_string()));
        assert_eq!(unreadable.failure_reason, Some(FailureReason::Error));
        assert!(unreadable.error.unwrap().contains("db gone"));
    }

    // -----------------------------------------------------------------
    // task cycles end to end, against the database
    // -----------------------------------------------------------------

    use crate::assistant::dispatcher::{claim, Kick};
    use crate::assistant::repository::CreateRunParams;
    use crate::assistant::workspace_tasks_repo::TaskLineage;
    use crate::db::test_support::workspace_pool;

    fn connection(id: &str) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            name: id.to_string(),
            protocol_id: "openai".into(),
            provider_id: "openai".into(),
            auth_mode: crate::assistant::types::AuthMode::DeveloperApiKey,
            base_url: None,
            secret_ref: String::new(),
            model_id: "model".into(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    async fn run_in(pool: &DbPool, session_id: &str, status: RunStatus) -> AssistantRun {
        repository::create_run(
            pool,
            CreateRunParams {
                session_id: session_id.to_string(),
                status,
                trigger: RunTrigger::UserMessage,
                connection_id: "conn".into(),
                protocol_id: "openai".into(),
                model_id: "model".into(),
                error: None,
            },
        )
        .await
        .unwrap()
    }

    /// A manager session that has run once, so its connection is known.
    async fn parent_session(pool: &DbPool) -> String {
        let session = repository::create_session(pool, SessionContext::default())
            .await
            .unwrap();
        run_in(pool, &session.id, RunStatus::Completed).await;
        session.id
    }

    /// What `assign_task` writes, minus the roster: the task row, its
    /// session and the pending instruction.
    async fn assign(pool: &DbPool, parent_session_id: &str, task_id: &str) -> String {
        create_task_row(
            pool,
            NewTask {
                id: task_id,
                created_by_workspace_agent_id: Some("wa-mgr"),
                assigned_to_workspace_agent_id: "wa-1",
                assigned_agent_definition_id: "def-1",
                title: "Review",
                instructions: "Review it",
                lineage: TaskLineage {
                    parent_session_id: Some(parent_session_id.to_string()),
                    ..TaskLineage::default()
                },
            },
        )
        .await
        .unwrap();
        enqueue_assignment(
            pool,
            task_id,
            "wa-1",
            "Review it".to_string(),
            MessageOrigin::Agent {
                agent: agent_snapshot(&[], "wa-mgr", None),
                session_id: Some(parent_session_id.to_string()),
                run_id: None,
            },
            "conn",
        )
        .await
        .unwrap()
        .id
    }

    /// The dispatcher claim that opens a cycle.
    async fn open(pool: &DbPool, session_id: &str) -> (AssistantRun, WorkspaceTaskRow) {
        match claim(pool, session_id, |id| Some(connection(id)))
            .await
            .unwrap()
        {
            Kick::Started {
                run,
                task: Some(task),
                ..
            } => (run, *task),
            other => panic!("expected a task cycle to start, got {other:?}"),
        }
    }

    /// The run writes `reply` and ends with `status`.
    async fn end_run(pool: &DbPool, run: &AssistantRun, reply: &str, status: RunStatus) {
        repository::create_message(
            pool,
            repository::CreateMessageParams {
                session_id: run.session_id.clone(),
                role: MessageRole::Assistant,
                content: vec![text(reply)],
                provider_metadata: None,
                run_id: Some(run.id.clone()),
            },
        )
        .await
        .unwrap();
        let error = (status == RunStatus::Failed).then_some("boom");
        repository::update_run_status(pool, &run.id, status, error)
            .await
            .unwrap();
    }

    async fn finish(pool: &DbPool, task_id: &str, run: &AssistantRun) -> Option<FinishedCycle> {
        finish_task_cycle(pool, task_id, Some(&run.id), &[])
            .await
            .unwrap()
    }

    async fn signals_of(pool: &DbPool, session_id: &str) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT kind, status FROM session_signals WHERE target_session_id = ?
             ORDER BY created_at, id",
        )
        .bind(session_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    fn signal(kind: &str, status: &str) -> (String, String) {
        (kind.to_string(), status.to_string())
    }

    #[tokio::test]
    async fn assign_returns_queued_and_parent_gets_result_once() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let task_session = assign(&pool, &parent, "task-1").await;

        let queued = load_task(&pool, "task-1").await.unwrap();
        assert_eq!(queued.status, TaskStatus::Queued);
        assert_eq!(queued.cycle_no, 0);
        assert_eq!(queued.run_id, None);
        assert_eq!(
            signals_of(&pool, &task_session).await,
            [signal("agent_message", "pending")]
        );

        let (run, task) = open(&pool, &task_session).await;
        assert_eq!(run.trigger, RunTrigger::WorkspaceTask);
        assert_eq!((task.status, task.cycle_no), (TaskStatus::Queued, 1));
        end_run(&pool, &run, "All good.", RunStatus::Completed).await;

        let finished = finish(&pool, "task-1", &run).await.expect("first finalize");
        assert_eq!(finished.task.status, TaskStatus::Completed);
        let result = finished.result.expect("a result for the parent");
        assert_eq!(result.session_id, parent);
        assert_eq!(result.content.len(), 1);
        assert!(matches!(&result.content[0], ContentPart::Text { text } if text == "All good."));
        assert!(matches!(
            &result.origin,
            MessageOrigin::Task { task_id, cycle_no: 1, status: TaskStatus::Completed, .. }
                if task_id == "task-1"
        ));

        // A second finalize of the same cycle writes nothing.
        assert!(finish(&pool, "task-1", &run).await.is_none());
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "pending")]
        );
    }

    #[tokio::test]
    async fn busy_parent_batches_two_results_in_one_block() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let parent_run = run_in(&pool, &parent, RunStatus::Running).await;
        for (task_id, reply) in [("task-1", "first done"), ("task-2", "second done")] {
            let session = assign(&pool, &parent, task_id).await;
            let (run, _) = open(&pool, &session).await;
            end_run(&pool, &run, reply, RunStatus::Completed).await;
            finish(&pool, task_id, &run).await.unwrap();
        }
        assert!(matches!(
            claim(&pool, &parent, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Busy { .. }
        ));

        repository::update_run_status(&pool, &parent_run.id, RunStatus::Completed, None)
            .await
            .unwrap();
        let Kick::Started {
            message_ids, task, ..
        } = claim(&pool, &parent, |id| Some(connection(id)))
            .await
            .unwrap()
        else {
            panic!("the parent must wake once its run ended");
        };
        assert_eq!(message_ids.len(), 2, "both results go to one run");
        assert!(task.is_none());

        let history = repository::list_messages(&pool, &parent).await.unwrap();
        let provider = crate::assistant::api_turn::normalize_history_for_provider(&history);
        assert_eq!(provider.len(), 1, "one user block: {provider:?}");
        let block = crate::assistant::conversation::content_text(&provider[0].content);
        assert_eq!(block.matches("[Automated event: task").count(), 2);
        assert!(block.contains("first done") && block.contains("second done"));
    }

    #[tokio::test]
    async fn result_message_deleted_does_not_duplicate() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        end_run(&pool, &run, "done", RunStatus::Completed).await;
        let result = finish(&pool, "task-1", &run).await.unwrap().result.unwrap();

        repository::delete_message(&pool, &result.id).await.unwrap();
        // A retried finalizer for the same cycle.
        sqlx::query("UPDATE workspace_tasks SET status = 'running' WHERE id = 'task-1'")
            .execute(&pool)
            .await
            .unwrap();
        let retried = finish(&pool, "task-1", &run).await.unwrap();

        assert!(retried.result.is_none());
        assert!(repository::list_messages(&pool, &parent)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "dropped")]
        );
    }

    #[tokio::test]
    async fn task_cycle_reads_only_own_output() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (first, _) = open(&pool, &session).await;
        end_run(&pool, &first, "first cycle answer", RunStatus::Completed).await;
        finish(&pool, "task-1", &first).await.unwrap();

        // A second cycle whose run writes no text of its own.
        repository::create_user_message(&pool, session.clone(), "again".into(), Some("conn"))
            .await
            .unwrap();
        let (second, task) = open(&pool, &session).await;
        assert_eq!(task.cycle_no, 2);
        repository::update_run_status(&pool, &second.id, RunStatus::Completed, None)
            .await
            .unwrap();
        let finished = finish(&pool, "task-1", &second).await.unwrap();

        assert_eq!(finished.task.cycle_no, 2);
        assert_eq!(finished.task.result_summary, None);
        let result = finished.result.unwrap();
        assert!(matches!(&result.content[0], ContentPart::Text { text } if text == "(no output)"));
    }

    #[tokio::test]
    async fn legacy_null_run_id_rows_fall_back_to_window() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();
        let mut run = run_in(&pool, &session.id, RunStatus::Completed).await;
        run.started_at = 100;
        run.completed_at = Some(200);
        for (id, created_at) in [("before", 50), ("inside", 150), ("after", 250)] {
            sqlx::query(
                "INSERT INTO assistant_messages (id, session_id, role, content_json, created_at)
                 VALUES (?, ?, 'assistant', '[]', ?)",
            )
            .bind(id)
            .bind(&session.id)
            .bind(created_at)
            .execute(&pool)
            .await
            .unwrap();
        }
        let ids = |messages: Vec<AssistantMessage>| -> Vec<String> {
            messages.into_iter().map(|message| message.id).collect()
        };

        let legacy = repository::list_messages_for_run(&pool, &run)
            .await
            .unwrap();
        assert_eq!(ids(legacy), ["inside"]);

        // Once the run has attributed rows, the window is not consulted.
        sqlx::query("UPDATE assistant_messages SET run_id = ? WHERE id = 'after'")
            .bind(&run.id)
            .execute(&pool)
            .await
            .unwrap();
        let attributed = repository::list_messages_for_run(&pool, &run)
            .await
            .unwrap();
        assert_eq!(ids(attributed), ["after"]);
    }

    #[tokio::test]
    async fn every_task_transition_emits_task_updated_without_signal_row() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;

        // Each transition hands back the row its caller emits as
        // `task_updated`: claim (kick), run start, finalize.
        let (run, queued) = open(&pool, &session).await;
        assert_eq!(queued.status, TaskStatus::Queued);
        let running = workspace_tasks_repo::mark_running(&pool, "task-1", &run.id)
            .await
            .unwrap()
            .expect("running transition");
        assert_eq!(running.status, TaskStatus::Running);
        end_run(&pool, &run, "done", RunStatus::Completed).await;
        let finished = finish(&pool, "task-1", &run).await.unwrap();
        assert_eq!(finished.task.status, TaskStatus::Completed);

        // Status changes write no signals: only the instruction and the result exist.
        let kinds: Vec<String> =
            sqlx::query_scalar("SELECT kind FROM session_signals ORDER BY created_at")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(kinds, ["agent_message", "task_result"]);
    }

    #[tokio::test]
    async fn blocked_reply_finalizes_as_failed_with_reason_blocked() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        end_run(
            &pool,
            &run,
            "BLOCKED: need an API key",
            RunStatus::Completed,
        )
        .await;

        let finished = finish(&pool, "task-1", &run).await.unwrap();

        assert_eq!(finished.task.status, TaskStatus::Failed);
        assert_eq!(finished.task.failure_reason, Some(FailureReason::Blocked));
        assert_eq!(
            finished.task.error.as_deref(),
            Some("BLOCKED: need an API key")
        );
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_failed", "pending")]
        );
    }

    #[tokio::test]
    async fn run_failure_finalizes_with_reason_error_and_run_error_text() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        end_run(&pool, &run, "partial work", RunStatus::Failed).await;

        let finished = finish(&pool, "task-1", &run).await.unwrap();

        assert_eq!(finished.task.status, TaskStatus::Failed);
        assert_eq!(finished.task.failure_reason, Some(FailureReason::Error));
        assert_eq!(finished.task.error.as_deref(), Some("boom"));
        assert_eq!(
            finished.task.result_summary.as_deref(),
            Some("partial work")
        );
        let result = finished.result.unwrap();
        assert!(matches!(
            &result.content[0],
            ContentPart::Text { text } if text == "Failure reason: error\n\nboom"
        ));
    }

    // -----------------------------------------------------------------
    // stop and restart recovery
    // -----------------------------------------------------------------

    async fn stop(pool: &DbPool, task_id: &str) -> StopOutcome {
        request_stop(pool, "ws-stop-tests", task_id, &[])
            .await
            .unwrap()
    }

    async fn set_run_status(pool: &DbPool, run: &AssistantRun, status: RunStatus) {
        repository::update_run_status(pool, &run.id, status, None)
            .await
            .unwrap();
    }

    async fn restart(pool: &DbPool) {
        recover_interrupted_tasks(pool).await.unwrap();
        repository::recover_stale_runs(pool).await.unwrap();
    }

    #[tokio::test]
    async fn stop_before_registration_cancels_queued_run() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        // Claimed, not yet registered: no token can reach the run.
        let (run, _) = open(&pool, &session).await;

        let StopOutcome::Requested(task) = stop(&pool, "task-1").await else {
            panic!("a claimed run is still to come: the stop waits for it");
        };
        assert_eq!(task.status, TaskStatus::Stopping);

        // What the dispatcher does once it registers the run.
        assert!(
            workspace_tasks_repo::stop_requested(&pool, "task-1", &run.id)
                .await
                .unwrap()
        );
        set_run_status(&pool, &run, RunStatus::Cancelled).await;
        let finished = finish(&pool, "task-1", &run).await.unwrap();

        assert_eq!(finished.task.status, TaskStatus::Stopped);
        assert_eq!(finished.task.failure_reason, None);
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "pending")]
        );
    }

    #[tokio::test]
    async fn stop_during_tool_honoured_at_boundary() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        workspace_tasks_repo::mark_running(&pool, "task-1", &run.id)
            .await
            .unwrap();
        let registration =
            runtime::register_run(LiveRunKey::new("ws-stop-tests", &session), &run.id);
        set_run_status(&pool, &run, RunStatus::WaitingForTool).await;

        let StopOutcome::Requested(task) = stop(&pool, "task-1").await else {
            panic!("a live run is cancelled, not finalized by the stop");
        };

        // The token is cancelled; the run and task wait for the run to reach
        // its next cancellation point.
        assert!(registration.token().is_cancelled());
        assert_eq!(task.status, TaskStatus::Stopping);
        let stored = repository::get_run(&pool, &run.id).await.unwrap().unwrap();
        assert_eq!(stored.status, RunStatus::WaitingForTool);

        set_run_status(&pool, &run, RunStatus::Cancelled).await;
        drop(registration);
        let finished = finish(&pool, "task-1", &run).await.unwrap();
        assert_eq!(finished.task.status, TaskStatus::Stopped);
    }

    #[tokio::test]
    async fn stop_vs_completion_yields_stopped() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        set_run_status(&pool, &run, RunStatus::Running).await;

        assert!(matches!(
            stop(&pool, "task-1").await,
            StopOutcome::Requested(_)
        ));
        // The provider finished before the cancel landed.
        end_run(&pool, &run, "BLOCKED: done anyway", RunStatus::Completed).await;
        let finished = finish(&pool, "task-1", &run).await.unwrap();

        assert_eq!(finished.task.status, TaskStatus::Stopped);
        assert_eq!(finished.task.failure_reason, None);
        assert_eq!(finished.task.error, None);
        assert_eq!(
            finished.task.result_summary.as_deref(),
            Some("BLOCKED: done anyway")
        );
        assert!(matches!(
            finished.result.unwrap().origin,
            MessageOrigin::Task {
                status: TaskStatus::Stopped,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn stop_twice_idempotent() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;

        let StopOutcome::Requested(first) = stop(&pool, "task-1").await else {
            panic!("the first stop requests");
        };
        let StopOutcome::AlreadyStopping(second) = stop(&pool, "task-1").await else {
            panic!("the second stop changes nothing");
        };
        assert!(first.stop_requested_at.is_some());
        assert_eq!(second.stop_requested_at, first.stop_requested_at);

        set_run_status(&pool, &run, RunStatus::Cancelled).await;
        finish(&pool, "task-1", &run).await.unwrap();
        let StopOutcome::AlreadyTerminal(ended) = stop(&pool, "task-1").await else {
            panic!("a stopped task is terminal");
        };
        assert_eq!(ended.status, TaskStatus::Stopped);
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "pending")]
        );
    }

    #[tokio::test]
    async fn stop_without_a_run_stops_at_once_and_old_input_does_not_resume() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;

        let StopOutcome::Stopped(finished) = stop(&pool, "task-1").await else {
            panic!("nothing to wait for: the stop ends the cycle");
        };
        assert_eq!(finished.task.status, TaskStatus::Stopped);
        assert!(finished.result.is_some());

        // The instruction predates the stop: no new cycle.
        assert!(matches!(
            claim(&pool, &session, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Idle
        ));
    }

    #[tokio::test]
    async fn stop_survives_restart() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        set_run_status(&pool, &run, RunStatus::Running).await;
        let StopOutcome::Requested(requested) = stop(&pool, "task-1").await else {
            panic!("the run is active");
        };

        // The process dies before the run winds down.
        restart(&pool).await;
        restart(&pool).await;

        let task = load_task(&pool, "task-1").await.unwrap();
        assert_eq!(task.status, TaskStatus::Stopped);
        assert_eq!(task.stop_requested_at, requested.stop_requested_at);
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "pending")]
        );
    }

    #[tokio::test]
    async fn restart_marks_running_task_stopped_not_failed() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        workspace_tasks_repo::mark_running(&pool, "task-1", &run.id)
            .await
            .unwrap();
        set_run_status(&pool, &run, RunStatus::Running).await;

        restart(&pool).await;

        let task = load_task(&pool, "task-1").await.unwrap();
        assert_eq!(task.status, TaskStatus::Stopped);
        assert_eq!(task.failure_reason, None);
        assert_eq!(
            task.error.as_deref(),
            Some("task interrupted by app restart")
        );
        assert!(task.stop_requested_at.is_some());
        let events = signals::list_open_content(&pool, &parent).await.unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].message.origin,
            MessageOrigin::Task {
                status: TaskStatus::Stopped,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn restart_leaves_never_started_task_queued() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        // Never claimed, and claimed by a run that never started.
        let unclaimed = assign(&pool, &parent, "task-1").await;
        let claimed = assign(&pool, &parent, "task-2").await;
        let (run, _) = open(&pool, &claimed).await;

        restart(&pool).await;

        for id in ["task-1", "task-2"] {
            assert_eq!(
                load_task(&pool, id).await.unwrap().status,
                TaskStatus::Queued
            );
        }
        assert_eq!(
            signals_of(&pool, &claimed).await,
            [signal("agent_message", "pending")]
        );
        assert!(signals_of(&pool, &parent).await.is_empty());
        let stale = repository::get_run(&pool, &run.id).await.unwrap().unwrap();
        assert_eq!(stale.status, RunStatus::Failed);

        // The next kick dispatches both again.
        for session in [&unclaimed, &claimed] {
            let (rerun, _) = open(&pool, session).await;
            let input = signals::list_content_for_run(&pool, session, &rerun.id)
                .await
                .unwrap();
            assert_eq!(input.len(), 1);
        }
    }
}

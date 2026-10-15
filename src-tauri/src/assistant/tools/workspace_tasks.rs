use crate::assistant::dispatcher::{self, Kick};
use crate::assistant::engine::AssistantDeps;
use crate::assistant::events::AssistantUiEvent;
use crate::assistant::repository;
use crate::assistant::runtime::{self, LiveRunKey};
use crate::assistant::signals::{self, NewEvent, SignalKind};
use crate::assistant::tools::ask_user::{self, AskUserAnswer, AskUserParams};
use crate::assistant::tools::ToolExecutionContext;
use crate::assistant::types::{
    AgentRefSnapshot, AssistantMessage, AssistantRun, ContentPart, MessageOrigin, MessageRole,
    ProviderConnection, RunStatus, SessionContext,
};
use crate::assistant::workspace_agents::{
    agent_display_names, concise_agent_description, load_workspace_agent_rows,
    workspace_agent_display_names, WorkspaceAgentRow,
};
use crate::assistant::workspace_tasks_repo::{
    self, create_task_row, CycleOutcome, FailureReason, NewTask, TaskStatus, WorkspaceTaskRow,
    WorkspaceTaskSendResponse,
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendWorkspaceTaskMessageParams {
    pub task_id: String,
    pub message: String,
}

pub(crate) const MAX_TASK_MESSAGE_CHARS: usize = 20_000;

pub async fn execute(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let roster =
        load_workspace_agent_rows(deps.app.state::<AppState>().inner(), &deps.workspace_id)?;
    let caller = authorize_caller(&roster, context.run_context.automation_id.as_deref())?;
    match tool_name {
        "workspace_listAgents" => {
            let params: ListWorkspaceAgentsParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_listAgents params: {}", e))?;
            Ok(list_agents(&deps.workspace_id, roster, params))
        }
        "workspace_assignTask" => {
            let params: AssignWorkspaceTaskParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_assignTask params: {}", e))?;
            assign_task(deps, context, &roster, caller, params).await
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
        "workspace_sendTaskMessage" => {
            let params: SendWorkspaceTaskMessageParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_sendTaskMessage params: {}", e))?;
            let origin = MessageOrigin::Agent {
                agent: agent_snapshot(&roster, caller, None),
                session_id: Some(context.session_id.clone()),
                run_id: Some(context.run_id.clone()),
            };
            let sent = send_task_message(deps, &params.task_id, &params.message, origin).await?;
            Ok(serde_json::json!({
                "ok": true,
                "task": sent.task,
                "queued": sent.queued,
                "run": sent.run,
            }))
        }
        _ => Err(format!("Unknown workspace task tool: {}", tool_name)),
    }
}

fn list_agents(
    workspace_id: &str,
    rows: Vec<WorkspaceAgentRow>,
    params: ListWorkspaceAgentsParams,
) -> serde_json::Value {
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

    serde_json::json!({
        "ok": true,
        "workspaceId": workspace_id,
        "agents": agents,
    })
}

async fn assign_task(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    rows: &[WorkspaceAgentRow],
    caller: &str,
    params: AssignWorkspaceTaskParams,
) -> Result<serde_json::Value, String> {
    let title = params.title.trim();
    let instructions = params.instructions.trim();
    if title.is_empty() {
        return Err("invalid_input: task title is required.".to_string());
    }
    if instructions.is_empty() {
        return Err("invalid_input: task instructions are required.".to_string());
    }

    let target = rows
        .iter()
        .find(|row| row.id == params.workspace_agent_id)
        .cloned()
        .ok_or_else(|| {
            format!(
                "not_found: {}",
                unknown_workspace_agent_error(&params.workspace_agent_id, rows)
            )
        })?;

    if !target.enabled {
        return Err(format!(
            "agent_disabled: {}",
            disabled_workspace_agent_error(&params.workspace_agent_id, rows)
        ));
    }

    let lineage =
        workspace_tasks_repo::lineage_for_caller(&deps.pool, &context.session_id, &context.run_id)
            .await?;

    let task_id = uuid::Uuid::new_v4().to_string();
    let connection = resolve_first_connection(deps, &target);
    let session_id = create_assignment(
        &deps.pool,
        &NewTask {
            id: &task_id,
            created_by_workspace_agent_id: Some(caller),
            assigned_to_workspace_agent_id: &target.id,
            assigned_agent_definition_id: &target.agent_definition_id,
            title,
            instructions,
            lineage,
        },
        connection
            .as_ref()
            .map(|connection| connection.id.as_str())
            .map_err(String::as_str),
        MessageOrigin::Agent {
            agent: agent_snapshot(rows, caller, None),
            session_id: Some(context.session_id.clone()),
            run_id: Some(context.run_id.clone()),
        },
    )
    .await?;

    // Read before the kick so the caller gets the queued placeholder.
    let result = task_result(deps, &task_id, &agent_display_names(rows)).await;
    if let Some(session_id) = session_id {
        dispatcher::kick_logged(deps, &session_id).await;
    }
    result
}

/// Writes an assignment in one transaction: the task row and, given the
/// assignee's connection, its session and the instruction as a pending
/// `agent_message` for the next kick. Without a connection (`Err` carries
/// why) the task is stored failed. Returns the task session.
pub(crate) async fn create_assignment(
    pool: &DbPool,
    task: &NewTask<'_>,
    connection: Result<&str, &str>,
    origin: MessageOrigin,
) -> Result<Option<String>, String> {
    // IMMEDIATE: the limit checks read before the insert writes, and a
    // deferred read transaction cannot upgrade once another writer commits.
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to start task assignment transaction: {}", e))?;
    create_task_row(&mut tx, task, connection.err()).await?;
    let session_id = match connection {
        Ok(connection_id) => {
            let session = repository::create_session_and_link_task(
                &mut tx,
                task_session_context(task.assigned_to_workspace_agent_id),
                task.id,
            )
            .await?;
            signals::insert_event(
                &mut tx,
                NewEvent {
                    session_id: &session.id,
                    kind: SignalKind::AgentMessage,
                    text: assignment_text(task.id, task.title, task.instructions),
                    origin,
                    delivery_key: None,
                    connection_id: Some(connection_id),
                },
            )
            .await?;
            Some(session.id)
        }
        Err(_) => None,
    };
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit task assignment transaction: {}", e))?;
    Ok(session_id)
}

fn assignment_text(task_id: &str, title: &str, instructions: &str) -> String {
    format!(
        "You have been assigned a workspace-local task by a workspace agent.\n\n\
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
    let task = workspace_tasks_repo::load_task(&deps.pool, task_id)
        .await?
        .ok_or_else(|| not_found(task_id))?;
    Ok(serde_json::json!({
        "ok": true,
        "task": task.into_response(&deps.workspace_id, agent_names),
    }))
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
    if let Some(message) = finished.result {
        announce_event(deps, message).await;
    }
}

/// Shows an automated event in its session and wakes the session.
async fn announce_event(deps: &AssistantDeps, message: AssistantMessage) {
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
/// and its result event, in one transaction. A completed cycle whose task
/// still waits on its subtasks stays open instead, returned with no result.
/// `None` when the cycle was already finalized.
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
    if let (Some(run_id), TaskStatus::Completed) = (run_id, outcome.status) {
        if let Some(task) =
            workspace_tasks_repo::park_for_subtasks(&mut tx, task_id, run_id).await?
        {
            tx.commit()
                .await
                .map_err(|e| format!("Failed to commit task park: {}", e))?;
            return Ok(Some(FinishedCycle { task, result: None }));
        }
    }
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
    let (kind, text) = match task.failure_reason {
        Some(reason) => (
            SignalKind::TaskFailed,
            format!(
                "Failure reason: {}\n\n{}",
                enum_text(&reason)?,
                task.error.as_deref().unwrap_or("(no error text)")
            ),
        ),
        None => (
            SignalKind::TaskResult,
            task.result_summary
                .clone()
                .or_else(|| task.error.clone())
                .unwrap_or_else(|| "(no output)".to_string()),
        ),
    };
    Ok(NewEvent {
        session_id: parent_session_id,
        kind,
        text,
        origin: task_origin(task, roster),
        delivery_key: Some(format!("task-result:{}:{}", task.id, task.cycle_no)),
        connection_id: None,
    })
}

/// The origin of an event a task posts to its parent, as the task is now.
fn task_origin(task: &WorkspaceTaskRow, roster: &[WorkspaceAgentRow]) -> MessageOrigin {
    MessageOrigin::Task {
        task_id: task.id.clone(),
        agent: agent_snapshot(
            roster,
            &task.assigned_to_workspace_agent_id,
            Some(&task.assigned_agent_definition_id),
        ),
        title: task.title.clone(),
        cycle_no: task.cycle_no,
        status: task.status,
    }
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

/// Sends `text` to a task from `origin`: answers its open `ask_user`
/// question when it has one, otherwise queues for its current run or starts
/// a new cycle for agent-origin messages.
pub(crate) async fn send_task_message(
    deps: &AssistantDeps,
    task_id: &str,
    text: &str,
    origin: MessageOrigin,
) -> Result<WorkspaceTaskSendResponse, String> {
    let delivery = deliver_task_message(&deps.pool, task_id, text, origin).await?;
    let session_id = delivery.message.session_id.clone();
    if let Ok(Some(session)) = repository::get_session(&deps.pool, &session_id).await {
        let _ = deps.emit(
            &session,
            None,
            AssistantUiEvent::MessageCreated {
                message: delivery.message.clone(),
            },
        );
    }
    let (run_id, queued) = if delivery.answered {
        (delivery.task.run_id.clone(), false)
    } else {
        match dispatcher::kick(deps, &session_id).await? {
            Kick::Started { run, .. } => (Some(run.id), false),
            Kick::Busy { run_id } => (Some(run_id), true),
            Kick::Idle | Kick::Deferred => (None, true),
        }
    };
    let run = match run_id {
        Some(run_id) => repository::get_run(&deps.pool, &run_id).await?,
        None => None,
    };
    let task = workspace_tasks_repo::load_task(&deps.pool, task_id)
        .await?
        .ok_or_else(|| not_found(task_id))?;
    let agent_names =
        workspace_agent_display_names(deps.app.state::<AppState>().inner(), &deps.workspace_id);
    Ok(WorkspaceTaskSendResponse {
        task: task.into_response(&deps.workspace_id, &agent_names),
        message: delivery.message,
        queued,
        run,
    })
}

#[derive(Debug)]
pub(crate) struct TaskMessageDelivery {
    /// The task as it was when the message was written.
    pub(crate) task: WorkspaceTaskRow,
    pub(crate) message: AssistantMessage,
    /// The message answered the task's open question and is consumed.
    pub(crate) answered: bool,
}

/// The database half of [`send_task_message`]: writes the message and its
/// signal, then hands it to a waiting `ask_user` if there is one.
pub(crate) async fn deliver_task_message(
    pool: &DbPool,
    task_id: &str,
    text: &str,
    origin: MessageOrigin,
) -> Result<TaskMessageDelivery, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("invalid_input: message is required.".to_string());
    }
    if text.chars().count() > MAX_TASK_MESSAGE_CHARS {
        return Err(format!(
            "invalid_input: message is longer than {MAX_TASK_MESSAGE_CHARS} characters."
        ));
    }
    let kind = match origin {
        MessageOrigin::User => SignalKind::UserMessage,
        _ => SignalKind::AgentMessage,
    };
    // IMMEDIATE for the same reason as in `create_assignment`.
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to start task message transaction: {}", e))?;
    let task = workspace_tasks_repo::load_task(&mut *tx, task_id)
        .await?
        .ok_or_else(|| not_found(task_id))?;
    if matches!(origin, MessageOrigin::User)
        && !matches!(
            task.status,
            TaskStatus::Queued | TaskStatus::Running | TaskStatus::InputRequired
        )
    {
        return Err(
            "invalid_input: This task is not accepting messages."
                .to_string(),
        );
    }
    let session_id = task.session_id.clone().ok_or_else(|| {
        format!("invalid_input: task {task_id} never started, so it cannot take messages.")
    })?;
    if let MessageOrigin::Agent { agent, .. } = &origin {
        signals::ensure_sender_budget(&mut tx, &agent.workspace_agent_id).await?;
    }
    let message = signals::insert_event(
        &mut tx,
        NewEvent {
            session_id: &session_id,
            kind,
            text: text.to_string(),
            origin: origin.clone(),
            delivery_key: None,
            connection_id: None,
        },
    )
    .await?
    .ok_or_else(|| format!("invalid_input: task {task_id} has no session any more."))?;
    // An answer is consumed before the waiting tool sees it, so the run's
    // next request cannot also read it as pending input.
    let answer_run = match (task.status, &task.run_id) {
        (TaskStatus::InputRequired, Some(run_id)) => Some(run_id),
        _ => None,
    };
    if let Some(run_id) = answer_run {
        signals::consume_one(&mut tx, &session_id, run_id, &message.id).await?;
    }
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit task message transaction: {}", e))?;

    let answered = answer_run.is_some()
        && ask_user::submit_answer_for_session(
            &session_id,
            AskUserAnswer {
                text: text.to_string(),
                selected_option_index: None,
                selected_option_indexes: None,
                origin,
            },
        )
        .is_ok();
    if answer_run.is_some() && !answered {
        // The message is saved; failing here would invite a duplicate send.
        if let Err(error) = signals::unconsume(pool, &message.id).await {
            tracing::error!(task_id, error, "Failed to hand an unanswered message back");
        }
    }
    Ok(TaskMessageDelivery {
        task,
        message,
        answered,
    })
}

/// A task's run asked `question`: the task waits for input and its parent
/// gets the question as an automated event.
pub(crate) async fn ask_parent(
    deps: &AssistantDeps,
    task: &WorkspaceTaskRow,
    run_id: &str,
    question: &str,
    params: &AskUserParams,
) {
    let roster =
        load_workspace_agent_rows(deps.app.state::<AppState>().inner(), &deps.workspace_id)
            .unwrap_or_default();
    match post_input_required(&deps.pool, &task.id, run_id, question, params, &roster).await {
        Ok(Some((task, event))) => {
            emit_task_updated(deps, task).await;
            if let Some(message) = event {
                announce_event(deps, message).await;
            }
        }
        Ok(None) => {}
        Err(error) => {
            tracing::error!(task_id = %task.id, run_id, error, "Failed to post task question");
        }
    }
}

/// The database half of [`ask_parent`]. `None` when the cycle is no longer
/// running; the event is `None` for a task with no parent.
pub(crate) async fn post_input_required(
    pool: &DbPool,
    task_id: &str,
    run_id: &str,
    question: &str,
    params: &AskUserParams,
    roster: &[WorkspaceAgentRow],
) -> Result<Option<(WorkspaceTaskRow, Option<AssistantMessage>)>, String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start task question transaction: {}", e))?;
    let Some(task) = workspace_tasks_repo::mark_input_required(&mut tx, task_id, run_id).await?
    else {
        return Ok(None);
    };
    let event = match task.parent_session_id.as_deref() {
        Some(parent_session_id) => {
            signals::insert_event(
                &mut tx,
                NewEvent {
                    session_id: parent_session_id,
                    kind: SignalKind::TaskInputRequired,
                    text: question_text(&task.id, question, params),
                    origin: task_origin(&task, roster),
                    delivery_key: None,
                    connection_id: None,
                },
            )
            .await?
        }
        None => None,
    };
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit task question transaction: {}", e))?;
    Ok(Some((task, event)))
}

fn question_text(task_id: &str, question: &str, params: &AskUserParams) -> String {
    let mut text = format!("Question: {question}");
    if let Some(context) = params.context.as_deref().filter(|c| !c.trim().is_empty()) {
        text.push_str(&format!("\n\nContext: {}", context.trim()));
    }
    if let Some(options) = params.options.as_ref().filter(|o| !o.is_empty()) {
        text.push_str("\n\nOptions:");
        for option in options {
            text.push_str(&format!("\n- {}", option.label));
        }
    }
    text.push_str(&format!(
        "\n\nAnswer with workspace_sendTaskMessage (taskId: {task_id})."
    ));
    text
}

/// `input_required → running` once the task's question was answered.
pub(crate) async fn record_ask_answered(
    deps: &AssistantDeps,
    task_id: &str,
    run_id: &str,
    answered_by: &MessageOrigin,
) {
    match workspace_tasks_repo::record_answer(&deps.pool, task_id, run_id, answered_by).await {
        Ok(Some(task)) => emit_task_updated(deps, task).await,
        Ok(None) => {}
        Err(error) => tracing::error!(task_id, run_id, error, "Failed to record task answer"),
    }
}

/// Requests a stop of the task's current cycle and, unless the task had
/// already ended, of every unfinished subtask below it, parents first so no
/// subtask result can resume a parent being stopped: `stopping` until a
/// run's teardown finalizes it, `stopped` at once when no run is active.
/// Returns the task's outcome; a subtask that fails to stop is only logged.
pub(crate) async fn stop_task(deps: &AssistantDeps, task_id: &str) -> Result<StopOutcome, String> {
    let roster =
        load_workspace_agent_rows(deps.app.state::<AppState>().inner(), &deps.workspace_id)
            .unwrap_or_default();
    let outcome = request_stop(&deps.pool, &deps.workspace_id, task_id, &roster).await?;
    if matches!(outcome, StopOutcome::AlreadyTerminal(_)) {
        return Ok(outcome);
    }
    announce_stop(deps, &outcome).await;
    let descendants = workspace_tasks_repo::descendant_ids(&deps.pool, task_id)
        .await
        .unwrap_or_else(|error| {
            tracing::error!(task_id, error, "Failed to list subtasks to stop");
            Vec::new()
        });
    for id in descendants {
        match request_stop(&deps.pool, &deps.workspace_id, &id, &roster).await {
            Ok(sub) => announce_stop(deps, &sub).await,
            Err(error) => tracing::error!(task_id = %id, error, "Failed to stop subtask"),
        }
    }
    Ok(outcome)
}

async fn announce_stop(deps: &AssistantDeps, outcome: &StopOutcome) {
    match outcome {
        StopOutcome::Requested(task) => emit_task_updated(deps, task.clone()).await,
        StopOutcome::Stopped(finished) => announce_finished(deps, finished.clone()).await,
        StopOutcome::AlreadyStopping(_) | StopOutcome::AlreadyTerminal(_) => {}
    }
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
    let (task, changed) = workspace_tasks_repo::mark_stopping(pool, task_id)
        .await?
        .ok_or_else(|| not_found(task_id))?;
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
            None => StopOutcome::Requested(
                workspace_tasks_repo::load_task(pool, &task.id)
                    .await?
                    .ok_or_else(|| not_found(&task.id))?,
            ),
        },
    )
}

/// Restart recovery, once per process per workspace database, before
/// `recover_stale_runs`: input of runs that never started is handed back,
/// interrupted tasks end `stopped`, and each stopped task's parent gets the
/// usual result event (one per cycle, by delivery key), naming the agent
/// from `roster`. Emits nothing: no window is listening yet.
pub(crate) async fn recover_interrupted_tasks(
    pool: &DbPool,
    roster: &[WorkspaceAgentRow],
) -> Result<(), String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start task recovery transaction: {}", e))?;
    signals::release_claims_of_unstarted_runs(&mut tx).await?;
    let stopped = workspace_tasks_repo::stop_interrupted(&mut tx).await?;
    for task in &stopped {
        if let Some(parent_session_id) = &task.parent_session_id {
            signals::insert_event(&mut tx, result_event(task, parent_session_id, roster)?).await?;
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

/// Only an enabled agent of this workspace may use the task tools. A run's
/// `automation_id` is the workspace agent id of the agent running it.
pub(crate) fn authorize_caller<'rows>(
    rows: &'rows [WorkspaceAgentRow],
    automation_id: Option<&str>,
) -> Result<&'rows str, String> {
    let caller = automation_id
        .and_then(|id| rows.iter().find(|row| row.id == id))
        .ok_or_else(|| {
            "not_member: only agents of this workspace can use task tools.".to_string()
        })?;
    if !caller.enabled {
        return Err(format!(
            "agent_disabled: {} is disabled in this workspace.",
            caller.display_name()
        ));
    }
    Ok(&caller.id)
}

/// Tasks are per workspace database, so another workspace's task id is
/// simply unknown here.
fn not_found(task_id: &str) -> String {
    format!("not_found: workspace task {task_id} does not exist.")
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
            &mut pool.acquire().await.unwrap(),
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
    fn only_enabled_roster_agents_are_authorized() {
        let mut disabled = agent_row("agent-2", "Code Reviewer");
        disabled.enabled = false;
        let rows = vec![agent_row("agent-1", "Manager"), disabled];

        assert_eq!(authorize_caller(&rows, Some("agent-1")), Ok("agent-1"));
        assert!(authorize_caller(&rows, Some("agent-2"))
            .unwrap_err()
            .starts_with("agent_disabled:"));
        // A definition id is never a run's automation id, so it does not match.
        for outsider in [Some("def-agent-1"), Some("gone"), None] {
            assert!(authorize_caller(&rows, outsider)
                .unwrap_err()
                .starts_with("not_member:"));
        }
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

    use crate::assistant::dispatcher::{claim, start_cycle, CycleStart, Kick};
    use crate::assistant::repository::CreateRunParams;
    use crate::assistant::workspace_tasks_repo::TaskLineage;
    use crate::db::test_support::{restart, workspace_pool};

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

    const WS: &str = "ws-task-tests";

    fn new_task<'a>(task_id: &'a str, lineage: TaskLineage) -> NewTask<'a> {
        NewTask {
            id: task_id,
            created_by_workspace_agent_id: Some("wa-mgr"),
            assigned_to_workspace_agent_id: "wa-1",
            assigned_agent_definition_id: "def-1",
            title: "Review",
            instructions: "Review it",
            lineage,
        }
    }

    /// What `assign_task` writes, minus the roster: the task row, its
    /// session and the pending instruction.
    async fn assign(pool: &DbPool, parent_session_id: &str, task_id: &str) -> String {
        let lineage = TaskLineage {
            parent_session_id: Some(parent_session_id.to_string()),
            ..TaskLineage::default()
        };
        create_assignment(
            pool,
            &new_task(task_id, lineage),
            Ok("conn"),
            parent_agent(),
        )
        .await
        .unwrap()
        .expect("a task with a connection gets a session")
    }

    /// The dispatcher claim that opens a cycle.
    async fn open(pool: &DbPool, session_id: &str) -> (AssistantRun, WorkspaceTaskRow) {
        match claim(pool, WS, session_id, |id| Some(connection(id)))
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

    async fn task_row(pool: &DbPool, task_id: &str) -> WorkspaceTaskRow {
        workspace_tasks_repo::load_task(pool, task_id)
            .await
            .unwrap()
            .expect("task exists")
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

        let queued = task_row(&pool, "task-1").await;
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
            claim(&pool, WS, &parent, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Busy { .. }
        ));

        repository::update_run_status(&pool, &parent_run.id, RunStatus::Completed, None)
            .await
            .unwrap();
        let Kick::Started {
            message_ids, task, ..
        } = claim(&pool, WS, &parent, |id| Some(connection(id)))
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
        request_stop(pool, WS, task_id, &[]).await.unwrap()
    }

    async fn set_run_status(pool: &DbPool, run: &AssistantRun, status: RunStatus) {
        repository::update_run_status(pool, &run.id, status, None)
            .await
            .unwrap();
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

        let CycleStart::Cancelled(cancelled) =
            start_cycle(&pool, Some("task-1"), &run.id).await.unwrap()
        else {
            panic!("the run sees the stop as it starts and never runs");
        };
        assert_eq!(cancelled.status, RunStatus::Cancelled);
        let finished = finish(&pool, "task-1", &run).await.unwrap();

        assert_eq!(finished.task.status, TaskStatus::Stopped);
        assert_eq!(finished.task.failure_reason, None);
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "pending")]
        );
    }

    #[tokio::test]
    async fn start_cycle_marks_an_unstopped_cycle_running() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;

        let CycleStart::Run(Some(task)) =
            start_cycle(&pool, Some("task-1"), &run.id).await.unwrap()
        else {
            panic!("an unstopped cycle runs");
        };
        assert_eq!(task.status, TaskStatus::Running);
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
        let registration = runtime::register_run(LiveRunKey::new(WS, &session), &run.id);
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
            claim(&pool, WS, &session, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Idle
        ));
    }

    #[tokio::test]
    async fn stop_survives_restart() {
        let (tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        set_run_status(&pool, &run, RunStatus::Running).await;
        let StopOutcome::Requested(requested) = stop(&pool, "task-1").await else {
            panic!("the run is active");
        };

        // The process dies before the run winds down.
        restart(tmp.path()).await;
        let pool = restart(tmp.path()).await;

        let task = task_row(&pool, "task-1").await;
        assert_eq!(task.status, TaskStatus::Stopped);
        assert_eq!(task.stop_requested_at, requested.stop_requested_at);
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "pending")]
        );
    }

    #[tokio::test]
    async fn restart_marks_running_task_stopped_not_failed() {
        let (tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;
        let (run, _) = open(&pool, &session).await;
        workspace_tasks_repo::mark_running(&pool, "task-1", &run.id)
            .await
            .unwrap();
        set_run_status(&pool, &run, RunStatus::Running).await;

        let pool = restart(tmp.path()).await;

        let task = task_row(&pool, "task-1").await;
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
        let (tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        // Never claimed, and claimed by a run that never started.
        let unclaimed = assign(&pool, &parent, "task-1").await;
        let claimed = assign(&pool, &parent, "task-2").await;
        let (run, _) = open(&pool, &claimed).await;

        let pool = restart(tmp.path()).await;

        for id in ["task-1", "task-2"] {
            assert_eq!(task_row(&pool, id).await.status, TaskStatus::Queued);
        }
        assert_eq!(
            signals_of(&pool, &claimed).await,
            [signal("agent_message", "pending")]
        );
        assert!(signals_of(&pool, &parent).await.is_empty());
        let stale = repository::get_run(&pool, &run.id).await.unwrap().unwrap();
        assert_eq!(stale.status, RunStatus::Failed);

        // The next kick dispatches both again, as their first cycle.
        for session in [&unclaimed, &claimed] {
            let (rerun, task) = open(&pool, session).await;
            assert_eq!(task.cycle_no, 1);
            let input = signals::list_content_for_run(&pool, session, &rerun.id)
                .await
                .unwrap();
            assert_eq!(input.len(), 1);
        }
    }

    #[tokio::test]
    async fn kick_between_run_end_and_finalize_waits_for_finalizer() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        end_run(&pool, &run, "cycle one done", RunStatus::Completed).await;

        // Input arrives after the run ended, before its finalizer ran.
        send(&pool, "task-1", "and another thing", parent_agent()).await;
        assert!(matches!(
            claim(&pool, WS, &session, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Idle
        ));

        let finished = finish(&pool, "task-1", &run)
            .await
            .expect("cycle one is still the finalizer's to end");
        assert_eq!(finished.task.status, TaskStatus::Completed);
        assert_eq!(
            finished.task.result_summary.as_deref(),
            Some("cycle one done")
        );
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "pending")]
        );
        // The finalizer's own kick opens cycle two.
        let (_, task) = open(&pool, &session).await;
        assert_eq!(task.cycle_no, 2);
    }

    #[tokio::test]
    async fn stop_between_claim_read_and_claim_commit_is_honoured() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let session = assign(&pool, &parent, "task-1").await;

        // The claim has read the task as `queued`; the stop commits first.
        let StopOutcome::Stopped(stopped) = stop(&pool, "task-1").await else {
            panic!("no run yet: the stop ends the cycle");
        };
        // Then the claim's transaction runs.
        let mut tx = pool.begin().await.unwrap();
        let run = repository::insert_run_if_idle(
            &mut tx,
            CreateRunParams {
                session_id: session.clone(),
                status: RunStatus::Queued,
                trigger: RunTrigger::WorkspaceTask,
                connection_id: "conn".into(),
                protocol_id: "openai".into(),
                model_id: "model".into(),
                error: None,
            },
        )
        .await
        .unwrap()
        .expect("the session is idle");
        signals::claim_batch(&mut tx, &session, &run.id, "conn")
            .await
            .unwrap();
        assert_eq!(
            workspace_tasks_repo::open_cycle(&mut tx, &session, &run.id)
                .await
                .unwrap(),
            None
        );
        drop(tx);

        let task = task_row(&pool, "task-1").await;
        assert_eq!(task.status, TaskStatus::Stopped);
        assert_eq!(task.stop_requested_at, stopped.task.stop_requested_at);
    }

    #[tokio::test]
    async fn message_sent_while_stopping_resumes_after_the_stop() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        stop(&pool, "task-1").await;
        set_run_status(&pool, &run, RunStatus::Cancelled).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        // Sent while the cycle winds down: it waits for the finalizer.
        let sent = send(&pool, "task-1", "then do this", MessageOrigin::User).await;
        assert!(matches!(
            claim(&pool, WS, &session, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Idle
        ));
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let finished = finish(&pool, "task-1", &run).await.unwrap();
        assert_eq!(finished.task.status, TaskStatus::Stopped);

        let Kick::Started {
            message_ids, task, ..
        } = claim(&pool, WS, &session, |id| Some(connection(id)))
            .await
            .unwrap()
        else {
            panic!("input sent after the stop request resumes the stopped task");
        };
        assert_eq!(task.expect("a task cycle").cycle_no, 2);
        assert_eq!(message_ids, [sent.message.id]);
    }

    #[tokio::test]
    async fn stop_cascades_to_subtasks_whose_results_do_not_resume_it() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        let child_session = delegate(&pool, &session, &run.id, "child").await.unwrap();
        let (child_run, _) = open(&pool, &child_session).await;
        delegate(&pool, &child_session, &child_run.id, "grandchild")
            .await
            .unwrap();

        let mut outcomes = vec![stop(&pool, "task-1").await];
        for id in workspace_tasks_repo::descendant_ids(&pool, "task-1")
            .await
            .unwrap()
        {
            outcomes.push(stop(&pool, &id).await);
        }
        assert!(matches!(
            outcomes.as_slice(),
            [
                StopOutcome::Requested(_),
                StopOutcome::Requested(_),
                StopOutcome::Stopped(_)
            ]
        ));
        assert_eq!(
            task_row(&pool, "grandchild").await.status,
            TaskStatus::Stopped
        );

        // The child's run sees the stop as it starts and reports back.
        start_cycle(&pool, Some("child"), &child_run.id)
            .await
            .unwrap();
        let child = finish(&pool, "child", &child_run).await.unwrap();
        assert_eq!(child.task.status, TaskStatus::Stopped);
        set_run_status(&pool, &run, RunStatus::Cancelled).await;
        finish(&pool, "task-1", &run).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert!(matches!(
            claim(&pool, WS, &session, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Idle
        ));

        // Explicit input resumes it, and the result rides along.
        send(&pool, "task-1", "carry on", parent_agent()).await;
        let Kick::Started { message_ids, .. } =
            claim(&pool, WS, &session, |id| Some(connection(id)))
                .await
                .unwrap()
        else {
            panic!("explicit input resumes the task");
        };
        assert_eq!(message_ids.len(), 2);
    }

    #[tokio::test]
    async fn delegating_worker_reports_once_after_its_subtask() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        let child_session = delegate(&pool, &session, &run.id, "child").await.unwrap();

        // The worker ends its turn while its subtask runs: no result yet.
        end_run(&pool, &run, "waiting for the child", RunStatus::Completed).await;
        let parked = finish(&pool, "task-1", &run).await.expect("parked");
        assert!(parked.result.is_none());
        assert_eq!(
            (parked.task.status, parked.task.run_id),
            (TaskStatus::Running, None)
        );
        assert!(signals_of(&pool, &parent).await.is_empty());

        // The subtask's result wakes it in the same cycle.
        let (child_run, _) = open(&pool, &child_session).await;
        end_run(&pool, &child_run, "child done", RunStatus::Completed).await;
        finish(&pool, "child", &child_run).await.unwrap();
        let (run, task) = open(&pool, &session).await;
        assert_eq!((task.status, task.cycle_no), (TaskStatus::Queued, 1));
        workspace_tasks_repo::mark_running(&pool, "task-1", &run.id)
            .await
            .unwrap();
        end_run(&pool, &run, "all done", RunStatus::Completed).await;

        let finished = finish(&pool, "task-1", &run).await.expect("finalized");
        assert_eq!(finished.task.status, TaskStatus::Completed);
        assert!(matches!(
            &finished.result.expect("one result").content[0],
            ContentPart::Text { text } if text == "all done"
        ));
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_result", "pending")]
        );
    }

    #[tokio::test]
    async fn blocked_worker_finishes_without_waiting_for_its_subtask() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        delegate(&pool, &session, &run.id, "child").await.unwrap();

        end_run(&pool, &run, "BLOCKED: need X", RunStatus::Completed).await;
        let finished = finish(&pool, "task-1", &run).await.expect("finalized");

        assert_eq!(finished.task.failure_reason, Some(FailureReason::Blocked));
        assert!(finished.result.is_some());
    }

    #[tokio::test]
    async fn deleted_parent_session_does_not_wedge_finalize_or_recovery() {
        let (tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (_, run) = running(&pool, &parent, "task-1").await;
        running(&pool, &parent, "task-2").await;
        repository::delete_session(&pool, &parent).await.unwrap();

        end_run(&pool, &run, "done", RunStatus::Completed).await;
        let finished = finish(&pool, "task-1", &run).await.expect("finalized");
        assert_eq!(finished.task.status, TaskStatus::Completed);
        assert!(finished.result.is_none());

        // task-2 is interrupted; recovery stops it with nowhere to report.
        let pool = restart(tmp.path()).await;
        assert_eq!(task_row(&pool, "task-2").await.status, TaskStatus::Stopped);
    }

    #[tokio::test]
    async fn resumed_task_stopped_after_restart_reports_its_own_cycle() {
        let (tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        end_run(&pool, &run, "cycle one", RunStatus::Completed).await;
        finish(&pool, "task-1", &run).await.unwrap();
        send(&pool, "task-1", "again", parent_agent()).await;
        // Cycle two is claimed, then the app restarts before its run starts.
        let (_, task) = open(&pool, &session).await;
        assert_eq!(task.cycle_no, 2);
        let pool = restart(tmp.path()).await;

        let StopOutcome::Stopped(stopped) = stop(&pool, "task-1").await else {
            panic!("no run: the stop ends the cycle");
        };
        assert_eq!(stopped.task.cycle_no, 2);
        assert!(
            stopped.result.is_some(),
            "the parent gets cycle two's result"
        );
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT delivery_key FROM session_signals WHERE target_session_id = ?
             ORDER BY created_at, id",
        )
        .bind(&parent)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(keys, ["task-result:task-1:1", "task-result:task-1:2"]);
    }

    #[tokio::test]
    async fn assignment_error_stores_nothing() {
        let (_tmp, pool) = workspace_pool().await;
        sqlx::query(
            "CREATE TRIGGER no_signals BEFORE INSERT ON session_signals
             BEGIN SELECT RAISE(ABORT, 'disk full'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let sessions = || async {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM assistant_sessions")
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let before = (sessions().await, message_count(&pool).await);

        let error = create_assignment(
            &pool,
            &new_task("task-1", TaskLineage::default()),
            Ok("conn"),
            parent_agent(),
        )
        .await
        .unwrap_err();

        assert!(error.contains("disk full"), "{error}");
        assert_eq!(task_count(&pool).await, 0);
        assert_eq!((sessions().await, message_count(&pool).await), before);
    }

    #[tokio::test]
    async fn assignment_without_connection_is_stored_failed_without_session() {
        let (_tmp, pool) = workspace_pool().await;

        let session = create_assignment(
            &pool,
            &new_task("task-1", TaskLineage::default()),
            Err("Agent 'x' has no active provider connection."),
            parent_agent(),
        )
        .await
        .unwrap();

        assert_eq!(session, None);
        let task = task_row(&pool, "task-1").await;
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(
            task.failure_reason,
            Some(FailureReason::ConnectionUnavailable)
        );
        assert_eq!(task.session_id, None);
    }

    // -----------------------------------------------------------------
    // messages to tasks and input_required
    // -----------------------------------------------------------------

    use crate::assistant::tools::ask_user::{AskUserOption, AskUserOutcome};
    use tokio::sync::oneshot;

    fn parent_agent() -> MessageOrigin {
        MessageOrigin::Agent {
            agent: agent_snapshot(&[], "wa-mgr", None),
            session_id: None,
            run_id: None,
        }
    }

    /// An assigned task whose first cycle's run is running.
    async fn running(pool: &DbPool, parent: &str, task_id: &str) -> (String, AssistantRun) {
        let session = assign(pool, parent, task_id).await;
        let (run, _) = open(pool, &session).await;
        workspace_tasks_repo::mark_running(pool, task_id, &run.id)
            .await
            .unwrap();
        set_run_status(pool, &run, RunStatus::Running).await;
        (session, run)
    }

    async fn send(
        pool: &DbPool,
        task_id: &str,
        text: &str,
        origin: MessageOrigin,
    ) -> TaskMessageDelivery {
        deliver_task_message(pool, task_id, text, origin)
            .await
            .unwrap()
    }

    async fn signal_status(pool: &DbPool, delivery: &TaskMessageDelivery) -> String {
        sqlx::query_scalar("SELECT status FROM session_signals WHERE message_id = ?")
            .bind(&delivery.message.id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// What `ask_user` does in a task session: registers the question, then
    /// posts it to the parent.
    async fn ask(
        pool: &DbPool,
        task_id: &str,
        run: &AssistantRun,
    ) -> oneshot::Receiver<AskUserOutcome> {
        let rx = ask_user::register_test_ask(&run.session_id);
        post_input_required(pool, task_id, &run.id, "Which env?", &ask_params(), &[])
            .await
            .unwrap()
            .expect("a running task can ask");
        rx
    }

    fn ask_params() -> AskUserParams {
        AskUserParams {
            question: "Which env?".to_string(),
            options: Some(vec![AskUserOption {
                label: "staging".to_string(),
                description: None,
            }]),
            multi_select: None,
            context: None,
        }
    }

    fn answer_of(mut rx: oneshot::Receiver<AskUserOutcome>) -> AskUserAnswer {
        match rx.try_recv() {
            Ok(AskUserOutcome::Answer(answer)) => answer,
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn message_running_task_is_pending_signal() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;

        let sent = send(&pool, "task-1", "also check the docs", parent_agent()).await;

        assert!(!sent.answered);
        assert_eq!(signal_status(&pool, &sent).await, "pending");
        assert!(matches!(
            claim(&pool, WS, &session, |id| Some(connection(id))).await.unwrap(),
            Kick::Busy { run_id } if run_id == run.id
        ));
        assert_eq!(
            signals_of(&pool, &session).await,
            [
                signal("agent_message", "claimed"),
                signal("agent_message", "pending")
            ]
        );
    }

    #[tokio::test]
    async fn message_stopped_task_starts_new_cycle() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        stop(&pool, "task-1").await;
        set_run_status(&pool, &run, RunStatus::Cancelled).await;
        finish(&pool, "task-1", &run).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let sent = send(&pool, "task-1", "carry on", parent_agent()).await;
        let Kick::Started {
            message_ids, task, ..
        } = claim(&pool, WS, &session, |id| Some(connection(id)))
            .await
            .unwrap()
        else {
            panic!("input sent after the stop resumes the task");
        };

        let task = task.expect("a task cycle");
        assert_eq!((task.status, task.cycle_no), (TaskStatus::Queued, 2));
        assert_eq!(task.stop_requested_at, None);
        assert_eq!(message_ids, std::slice::from_ref(&sent.message.id));
        assert_eq!(signal_status(&pool, &sent).await, "claimed");
    }

    #[tokio::test]
    async fn completed_task_rejects_user_message_but_agent_can_resume() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        set_run_status(&pool, &run, RunStatus::Completed).await;
        finish(&pool, "task-1", &run).await.unwrap();
        assert_eq!(
            task_row(&pool, "task-1").await.status,
            TaskStatus::Completed
        );

        let before = message_count(&pool).await;
        let error = deliver_task_message(&pool, "task-1", "from user", MessageOrigin::User)
            .await
            .unwrap_err();
        assert_eq!(error, "invalid_input: This task is not accepting messages.");
        assert_eq!(message_count(&pool).await, before);

        let sent = send(&pool, "task-1", "from agent", parent_agent()).await;
        let Kick::Started {
            message_ids, task, ..
        } = claim(&pool, WS, &session, |id| Some(connection(id)))
            .await
            .unwrap()
        else {
            panic!("agent message resumes the completed task");
        };
        assert_eq!(message_ids, vec![sent.message.id]);
        assert_eq!(task.unwrap().cycle_no, 2);
    }

    #[tokio::test]
    async fn stopped_task_not_resumed_by_old_pending_signal() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        // Queued for the running cycle, which is then stopped.
        let old = send(&pool, "task-1", "and the tests", parent_agent()).await;
        stop(&pool, "task-1").await;
        set_run_status(&pool, &run, RunStatus::Cancelled).await;
        finish(&pool, "task-1", &run).await.unwrap();

        assert!(matches!(
            claim(&pool, WS, &session, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Idle
        ));
        assert_eq!(signal_status(&pool, &old).await, "pending");

        // A new message resumes it, and the old one rides along.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        send(&pool, "task-1", "resume", parent_agent()).await;
        let Kick::Started { message_ids, .. } =
            claim(&pool, WS, &session, |id| Some(connection(id)))
                .await
                .unwrap()
        else {
            panic!("new input resumes the task");
        };
        assert_eq!(message_ids.len(), 2);
    }

    #[tokio::test]
    async fn ask_user_in_task_emits_input_required_to_parent() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (_session, run) = running(&pool, &parent, "task-1").await;

        let (task, event) =
            post_input_required(&pool, "task-1", &run.id, "Which env?", &ask_params(), &[])
                .await
                .unwrap()
                .expect("a running task can ask");

        assert_eq!(task.status, TaskStatus::InputRequired);
        let event = event.expect("the parent gets the question");
        assert_eq!(event.session_id, parent);
        assert!(matches!(
            &event.origin,
            MessageOrigin::Task { task_id, cycle_no: 1, status: TaskStatus::InputRequired, .. }
                if task_id == "task-1"
        ));
        let ContentPart::Text { text } = &event.content[0] else {
            panic!("a text event");
        };
        assert!(text.contains("Which env?") && text.contains("- staging"));
        assert!(text.contains("workspace_sendTaskMessage"));
        assert_eq!(
            signals_of(&pool, &parent).await,
            [signal("task_input_required", "pending")]
        );

        // A stopping cycle asks nothing.
        stop(&pool, "task-1").await;
        assert!(
            post_input_required(&pool, "task-1", &run.id, "Again?", &ask_params(), &[])
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn parent_answer_resolves_pending_ask_and_sets_answered_by() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (_session, run) = running(&pool, &parent, "task-1").await;
        let rx = ask(&pool, "task-1", &run).await;

        let sent = send(&pool, "task-1", "staging", parent_agent()).await;

        assert!(sent.answered);
        assert_eq!(signal_status(&pool, &sent).await, "consumed");
        let answer = answer_of(rx);
        assert_eq!(answer.text, "staging");
        assert_eq!(answer.origin, parent_agent());

        // What the waiting tool does with the answer.
        let task = workspace_tasks_repo::record_answer(&pool, "task-1", &run.id, &answer.origin)
            .await
            .unwrap()
            .expect("the task was waiting");
        assert_eq!(task.status, TaskStatus::Running);
        let response = serde_json::to_value(task.into_response("ws", &HashMap::new())).unwrap();
        assert_eq!(response["answeredBy"]["kind"], "agent");
        assert_eq!(
            response["answeredBy"]["agent"]["workspaceAgentId"],
            "wa-mgr"
        );
    }

    #[tokio::test]
    async fn human_composer_send_resolves_ask_with_user_origin() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        let rx = ask(&pool, "task-1", &run).await;

        let sent = send(&pool, "task-1", "production", MessageOrigin::User).await;

        assert!(sent.answered);
        assert_eq!(sent.message.origin, MessageOrigin::User);
        let answer = answer_of(rx);
        assert_eq!(answer.origin, MessageOrigin::User);
        assert_eq!(
            signals_of(&pool, &session).await,
            [
                signal("agent_message", "claimed"),
                signal("user_message", "consumed")
            ]
        );
        let task = workspace_tasks_repo::record_answer(&pool, "task-1", &run.id, &answer.origin)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.input_answered_by, Some(MessageOrigin::User));
    }

    #[tokio::test]
    async fn concurrent_answers_first_commit_wins_second_stays_content_signal() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        let (session, run) = running(&pool, &parent, "task-1").await;
        let rx = ask(&pool, "task-1", &run).await;

        // Both senders saw the task waiting for input.
        let first = send(&pool, "task-1", "from the parent", parent_agent()).await;
        let second = send(&pool, "task-1", "from the human", MessageOrigin::User).await;

        assert!(first.answered);
        assert!(!second.answered);
        assert_eq!(answer_of(rx).text, "from the parent");
        assert_eq!(signal_status(&pool, &first).await, "consumed");
        assert_eq!(signal_status(&pool, &second).await, "pending");
        assert!(matches!(
            claim(&pool, WS, &session, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Busy { .. }
        ));
    }

    // -----------------------------------------------------------------
    // delegation limits
    // -----------------------------------------------------------------

    /// A task assigned from `caller_session`'s run `run_id`, with the
    /// lineage `assign_task` computes. Returns the task's session.
    async fn delegate(
        pool: &DbPool,
        caller_session: &str,
        run_id: &str,
        task_id: &str,
    ) -> Result<String, String> {
        let lineage = workspace_tasks_repo::lineage_for_caller(pool, caller_session, run_id)
            .await
            .unwrap();
        Ok(create_assignment(
            pool,
            &new_task(task_id, lineage),
            Ok("conn"),
            parent_agent(),
        )
        .await?
        .expect("a task with a connection gets a session"))
    }

    async fn task_count(pool: &DbPool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_tasks")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn message_count(pool: &DbPool) -> (i64, i64) {
        sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM assistant_messages),
                    (SELECT COUNT(*) FROM session_signals)",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn cross_workspace_task_id_hidden() {
        let (_tmp, pool) = workspace_pool().await;
        let (_other_tmp, other_pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        running(&pool, &parent, "task-1").await;

        let sent = deliver_task_message(&other_pool, "task-1", "hi", parent_agent())
            .await
            .unwrap_err();
        assert!(sent.starts_with("not_found:"), "{sent}");
        let stopped = request_stop(&other_pool, "ws-other", "task-1", &[])
            .await
            .unwrap_err();
        assert!(stopped.starts_with("not_found:"), "{stopped}");
        assert!(workspace_tasks_repo::load_task(&other_pool, "task-1")
            .await
            .unwrap()
            .is_none());
        assert_eq!(message_count(&other_pool).await, (0, 0));
    }

    #[tokio::test]
    async fn parallel_assignments_from_one_run_all_succeed() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;

        let (a, b) = tokio::join!(
            delegate(&pool, &parent, "run-1", "task-a"),
            delegate(&pool, &parent, "run-1", "task-b"),
        );

        a.unwrap();
        b.unwrap();
        assert_eq!(task_count(&pool).await, 2);
    }

    #[tokio::test]
    async fn depth_four_allowed_five_rejected() {
        let (_tmp, pool) = workspace_pool().await;
        let mut caller = parent_session(&pool).await;
        for level in 1..=4 {
            caller = delegate(
                &pool,
                &caller,
                &format!("run-{level}"),
                &format!("task-{level}"),
            )
            .await
            .unwrap_or_else(|error| panic!("level {level}: {error}"));
        }
        let fourth = workspace_tasks_repo::load_task(&pool, "task-4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fourth.depth, 3);

        let error = delegate(&pool, &caller, "run-5", "task-5")
            .await
            .unwrap_err();
        assert!(error.starts_with("depth_exceeded:"), "{error}");
        assert_eq!(task_count(&pool).await, 4);
    }

    #[tokio::test]
    async fn forty_first_assignment_rejected_budget_exceeded() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        for n in 1..=signals::AGENT_SENDS_PER_HOUR {
            delegate(&pool, &parent, "run-1", &format!("task-{n}"))
                .await
                .unwrap_or_else(|error| panic!("assignment {n}: {error}"));
        }

        let error = delegate(&pool, &parent, "run-1", "task-41")
            .await
            .unwrap_err();
        assert!(error.starts_with("budget_exceeded:"), "{error}");
        assert_eq!(task_count(&pool).await, signals::AGENT_SENDS_PER_HOUR);
    }

    #[tokio::test]
    async fn forty_first_agent_send_rejected_budget_exceeded_nothing_stored() {
        let (_tmp, pool) = workspace_pool().await;
        let parent = parent_session(&pool).await;
        // The assignment is the sender's first agent message of the hour.
        running(&pool, &parent, "task-1").await;
        for n in 2..=signals::AGENT_SENDS_PER_HOUR {
            send(&pool, "task-1", &format!("note {n}"), parent_agent()).await;
        }
        let before = message_count(&pool).await;

        let error = deliver_task_message(&pool, "task-1", "one more", parent_agent())
            .await
            .unwrap_err();
        assert!(error.starts_with("budget_exceeded:"), "{error}");
        let error = delegate(&pool, &parent, "run-2", "task-2")
            .await
            .unwrap_err();
        assert!(error.starts_with("budget_exceeded:"), "{error}");
        assert_eq!(message_count(&pool).await, before);
        assert_eq!(task_count(&pool).await, 1);

        // Human sends are never counted.
        send(&pool, "task-1", "from the human", MessageOrigin::User).await;
        assert_eq!(message_count(&pool).await, (before.0 + 1, before.1 + 1));
    }
}

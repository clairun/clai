use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::repository::{self, CreateMessageParams, CreateRunParams};
use crate::assistant::tools::ToolExecutionContext;
use crate::assistant::types::{
    AssistantMessage, ContentPart, MessageRole, ProviderConnection, RunStatus, RunTrigger,
    SessionContext,
};
use crate::assistant::workspace_agents::{
    agent_display_names, concise_agent_description, load_workspace_agent_rows,
    workspace_agent_display_names, WorkspaceAgentRow,
};
use crate::assistant::workspace_tasks_repo::{self, create_task_row, update_task_status};
use crate::config::first_enabled_connection;
use crate::db::DbPool;
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::{Emitter, Manager};

pub const WORKSPACE_TASK_ATTENTION_EVENT_NAME: &str = "workspace://task-attention";

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceTaskAttentionEvent {
    task_id: String,
    workspace_id: String,
    title: String,
    status: String,
    summary: Option<String>,
    error: Option<String>,
    updated_at: i64,
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

#[expect(
    clippy::too_many_lines,
    reason = "lint debt: 145 lines against a 100-line budget; split it, do not raise the budget"
)]
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

    let creator = creator_workspace_agent_id(&rows, context.run_context.automation_id.as_deref());

    let task_id = uuid::Uuid::new_v4().to_string();
    create_task_row(
        &deps.pool,
        &task_id,
        creator,
        &target.id,
        &target.agent_definition_id,
        title,
        instructions,
    )
    .await?;

    let connection = match resolve_first_connection(deps, &target) {
        Ok(connection) => connection,
        Err(message) => {
            update_task_status(
                &deps.pool,
                &task_id,
                "blocked",
                None,
                None,
                Some(&message),
                None,
                true,
            )
            .await?;
            let _ = emit_task_attention_event(&deps.app, &deps.pool, &workspace_id, &task_id).await;
            return task_result(deps, &task_id, &agent_display_names(&rows)).await;
        }
    };

    // Insert the session and stamp its id on the workspace_tasks row in one
    // SQLite transaction so a concurrent resolver cannot observe the session
    // before the task row links to it (see create_session_and_link_task).
    let session = repository::create_session_and_link_task(
        &deps.pool,
        task_session_context(&target.id),
        &task_id,
    )
    .await?;

    repository::create_message(
        &deps.pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::User,
            content: vec![ContentPart::Text {
                text: format!(
                    "You have been assigned a workspace-local task by the manager.\n\n\
                     Task ID: {}\n\
                     Task title: {}\n\n\
                     Instructions:\n{}\n\n\
                     Work only with the current workspace context and available tools. \
                     Return a concise result summary. If blocked by missing capability, context, permission, or runtime failure, start your response with `BLOCKED:` and describe the blocker. If you need a human decision, call the `ask_user` tool directly — do not surface the question via your text response.",
                    task_id, title, instructions
                ),
            }],
            provider_metadata: None,
        },
    )
    .await?;

    let run = repository::create_run(
        &deps.pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::WorkspaceTask,
            connection_id: connection.id.clone(),
            protocol_id: connection.protocol_id.clone(),
            model_id: connection.model_id.clone(),
            error: None,
        },
    )
    .await?;

    // session_id was already stamped by create_session_and_link_task above;
    // passing None here lets COALESCE preserve it while we set run_id + status.
    update_task_status(
        &deps.pool,
        &task_id,
        "queued",
        None,
        Some(&run.id),
        None,
        None,
        false,
    )
    .await?;

    spawn_task_run(
        deps.clone(),
        workspace_id.clone(),
        task_id.clone(),
        session.id.clone(),
        run.id.clone(),
        connection.id.clone(),
    );

    task_result(deps, &task_id, &agent_display_names(&rows)).await
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

fn spawn_task_run(
    deps: AssistantDeps,
    workspace_id: String,
    task_id: String,
    session_id: String,
    run_id: String,
    connection_id: String,
) {
    tauri::async_runtime::spawn(async move {
        let _ = update_task_status(
            &deps.pool,
            &task_id,
            "running",
            Some(&session_id),
            Some(&run_id),
            None,
            None,
            false,
        )
        .await;

        let run_registration = crate::assistant::runtime::register_run(&run_id);
        let cancel_token = run_registration.token();
        let input = RunTurnInput {
            session_id: session_id.clone(),
            run_id: Some(run_id.clone()),
            trigger: RunTrigger::WorkspaceTask,
            connection_id,
            cancel_token,
            trigger_message_id: None,
        };

        let result = engine::run_session_turn(&deps, input).await;
        drop(run_registration);

        match result {
            Ok(()) => {
                let run = repository::get_run(&deps.pool, &run_id)
                    .await
                    .ok()
                    .flatten();
                let status = match run.as_ref().map(|run| &run.status) {
                    Some(RunStatus::Completed | RunStatus::CompletedWithWarnings) => "completed",
                    Some(RunStatus::Cancelled) => "failed",
                    Some(RunStatus::Failed) => "failed",
                    _ => "completed",
                };
                let summary = latest_assistant_text(&deps.pool, &session_id)
                    .await
                    .unwrap_or(None);
                let status = classify_worker_status(summary.as_deref(), status);
                let error = run.and_then(|run| run.error);
                let _ = update_task_status(
                    &deps.pool,
                    &task_id,
                    status,
                    Some(&session_id),
                    Some(&run_id),
                    error.as_deref(),
                    summary.as_deref(),
                    true,
                )
                .await;
                if is_attention_status(status) {
                    let _ =
                        emit_task_attention_event(&deps.app, &deps.pool, &workspace_id, &task_id)
                            .await;
                }
            }
            Err(error) => {
                let _ = update_task_status(
                    &deps.pool,
                    &task_id,
                    "failed",
                    Some(&session_id),
                    Some(&run_id),
                    Some(&error.to_string()),
                    None,
                    true,
                )
                .await;
                let _ =
                    emit_task_attention_event(&deps.app, &deps.pool, &workspace_id, &task_id).await;
            }
        }
    });
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

async fn emit_task_attention_event(
    app: &tauri::AppHandle,
    pool: &DbPool,
    workspace_id: &str,
    task_id: &str,
) -> Result<(), String> {
    let task = load_task(pool, task_id).await?;
    if !is_attention_status(&task.status) {
        return Ok(());
    }

    let event = WorkspaceTaskAttentionEvent {
        task_id: task.id,
        workspace_id: workspace_id.to_string(),
        title: task.title,
        status: task.status,
        summary: task.result_summary,
        error: task.error,
        updated_at: task.updated_at,
    };

    app.emit(WORKSPACE_TASK_ATTENTION_EVENT_NAME, event)
        .map_err(|e| format!("Failed to emit workspace task attention event: {}", e))
}

fn is_attention_status(status: &str) -> bool {
    matches!(status, "blocked" | "failed")
}

fn classify_worker_status(summary: Option<&str>, fallback: &'static str) -> &'static str {
    let Some(summary) = summary.map(str::trim_start) else {
        return fallback;
    };

    if summary.starts_with("BLOCKED:") {
        "blocked"
    } else {
        fallback
    }
}

async fn latest_assistant_text(pool: &DbPool, session_id: &str) -> Result<Option<String>, String> {
    let messages = repository::list_messages(pool, session_id).await?;
    Ok(final_assistant_text(&messages))
}

/// The last non-empty text the assistant produced in a session, used as the
/// task's result summary and for BLOCKED-status classification.
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
    // is_attention_status
    // -----------------------------------------------------------------

    #[test]
    fn is_attention_status_blocked() {
        assert!(is_attention_status("blocked"));
    }

    #[test]
    fn is_attention_status_failed() {
        assert!(is_attention_status("failed"));
    }

    #[test]
    fn is_attention_status_needs_user_input_is_false() {
        // `needs_user_input` is no longer a status — the `ask_user` tool
        // blocks the run inline, so the row never enters that state.
        assert!(!is_attention_status("needs_user_input"));
    }

    #[test]
    fn is_attention_status_running_is_false() {
        assert!(!is_attention_status("running"));
    }

    #[test]
    fn is_attention_status_completed_is_false() {
        assert!(!is_attention_status("completed"));
    }

    #[test]
    fn is_attention_status_queued_is_false() {
        assert!(!is_attention_status("queued"));
    }

    #[test]
    fn is_attention_status_empty_is_false() {
        assert!(!is_attention_status(""));
    }

    // -----------------------------------------------------------------
    // final_assistant_text
    // -----------------------------------------------------------------

    fn msg(role: MessageRole, content: Vec<ContentPart>) -> AssistantMessage {
        AssistantMessage {
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
        assert_eq!(
            classify_worker_status(summary.as_deref(), "completed"),
            "blocked"
        );
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

    // -----------------------------------------------------------------
    // classify_worker_status
    // -----------------------------------------------------------------

    #[test]
    fn classify_worker_status_none_returns_fallback() {
        assert_eq!(classify_worker_status(None, "completed"), "completed");
    }

    #[test]
    fn classify_worker_status_needs_user_input_prefix_falls_through() {
        // The NEEDS_USER_INPUT prefix is no longer special-cased — the
        // `ask_user` tool is the only channel for human-input questions.
        assert_eq!(
            classify_worker_status(Some("NEEDS_USER_INPUT: Please review."), "completed"),
            "completed"
        );
    }

    #[test]
    fn classify_worker_status_blocked_prefix() {
        assert_eq!(
            classify_worker_status(Some("BLOCKED: Missing API key."), "completed"),
            "blocked"
        );
    }

    #[test]
    fn classify_worker_status_normal_summary_returns_fallback() {
        assert_eq!(
            classify_worker_status(Some("Task completed successfully."), "completed"),
            "completed"
        );
    }

    #[test]
    fn classify_worker_status_trims_leading_whitespace() {
        assert_eq!(
            classify_worker_status(Some("  BLOCKED: Whitespace before."), "running"),
            "blocked"
        );
    }

    #[test]
    fn classify_worker_status_empty_string_returns_fallback() {
        assert_eq!(classify_worker_status(Some(""), "completed"), "completed");
    }

    #[test]
    fn classify_worker_status_whitespace_only_returns_fallback() {
        assert_eq!(
            classify_worker_status(Some("   \n  "), "completed"),
            "completed"
        );
    }
}

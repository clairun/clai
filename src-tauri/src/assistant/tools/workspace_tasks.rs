use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::repository::{
    self, CreateMessageParams, CreateRunParams, CreateSessionParams,
};
use crate::assistant::tools::ToolExecutionContext;
use crate::assistant::types::{
    ContentPart, MessageRole, ProviderConnection, RunStatus, RunTrigger, SessionContext,
    SessionKind,
};
use crate::config::{
    agent_instructions_with_skills, workspace_config, AgentConfig, AppConfig, WorkspaceAgent,
    WorkspaceConfig,
};
use crate::db::DbPool;
use crate::AppState;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tauri::{Emitter, Manager};

pub const WORKSPACE_TASK_ATTENTION_EVENT_NAME: &str = "workspace://task-attention";

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListWorkspaceAgentsParams {
    #[serde(default)]
    pub include_disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssignWorkspaceTaskParams {
    pub workspace_agent_id: String,
    pub title: String,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkspaceTaskResultParams {
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestWorkspaceUserInputParams {
    pub title: String,
    pub question: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub requested_action: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceTaskResponse {
    id: String,
    workspace_id: String,
    created_by_workspace_agent_id: Option<String>,
    assigned_to_workspace_agent_id: String,
    assigned_agent_definition_id: String,
    title: String,
    instructions: String,
    status: String,
    result_summary: Option<String>,
    result_json: Option<serde_json::Value>,
    error: Option<String>,
    session_id: Option<String>,
    run_id: Option<String>,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
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

#[derive(Debug, Clone)]
struct WorkspaceAgentRow {
    id: String,
    workspace_id: String,
    agent_definition_id: String,
    display_name: Option<String>,
    role: String,
    enabled: bool,
    // Inline agent fields (Phase 1.4 — populated from workspace_agents columns;
    // no more join with the global ClaiConfig.agents catalog).
    name: String,
    description: String,
    selected_skill_ids: Vec<String>,
    selected_mcp_server_ids: Vec<String>,
    provider_connection_ids: Vec<String>,
    execution: crate::config::ExecutionCapabilityConfig,
}

#[derive(Debug, Clone)]
struct WorkspaceTaskRow {
    id: String,
    workspace_id: String,
    created_by_workspace_agent_id: Option<String>,
    assigned_to_workspace_agent_id: String,
    assigned_agent_definition_id: String,
    title: String,
    instructions: String,
    status: String,
    result_summary: Option<String>,
    result_json: Option<String>,
    error: Option<String>,
    session_id: Option<String>,
    run_id: Option<String>,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
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
            list_agents(deps, context, params).await
        }
        "workspace_assignTask" => {
            let params: AssignWorkspaceTaskParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_assignTask params: {}", e))?;
            assign_task(deps, context, params).await
        }
        "workspace_getTaskResult" => {
            let params: GetWorkspaceTaskResultParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_getTaskResult params: {}", e))?;
            get_task_result(deps, context, params).await
        }
        "workspace_requestUserInput" => {
            let params: RequestWorkspaceUserInputParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid workspace_requestUserInput params: {}", e))?;
            request_user_input(deps, context, params).await
        }
        _ => Err(format!("Unknown workspace task tool: {}", tool_name)),
    }
}

async fn list_agents(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    params: ListWorkspaceAgentsParams,
) -> Result<serde_json::Value, String> {
    let workspace_id = workspace_id_from_context(context)?;
    let app_state = deps.app.state::<AppState>();
    let rows = load_workspace_agent_rows(app_state.inner(), &workspace_id)?;

    let agents: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| params.include_disabled || row.enabled)
        .map(|row| {
            // Phase 1.4: every field below comes from the workspace_agents row
            // itself; no join with the global ClaiConfig.agents catalog.
            let display_name = row
                .display_name
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| row.name.clone());
            serde_json::json!({
                "id": row.id,
                "workspaceId": row.workspace_id,
                "agentDefinitionId": row.agent_definition_id,
                "displayName": display_name,
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
    let workspace_id = workspace_id_from_context(context)?;
    let title = params.title.trim();
    let instructions = params.instructions.trim();
    if title.is_empty() {
        return Err("Task title is required.".to_string());
    }
    if instructions.is_empty() {
        return Err("Task instructions are required.".to_string());
    }

    let app_state = deps.app.state::<AppState>();
    let target =
        load_workspace_agent_row(app_state.inner(), &workspace_id, &params.workspace_agent_id)?
            .ok_or_else(|| {
                format!(
                    "Workspace agent assignment not found in this workspace: {}",
                    params.workspace_agent_id
                )
            })?;

    if !target.enabled {
        return Err(format!(
            "Workspace agent assignment is disabled: {}",
            params.workspace_agent_id
        ));
    }

    // Phase 1.4: build the target AgentConfig from the workspace_agents row
    // inline fields — no global catalog lookup.
    let target_config = AgentConfig {
        id: target.id.clone(),
        workspace_id: target.workspace_id.clone(),
        name: target.name.clone(),
        description: target.description.clone(),
        schedule_enabled: false,
        interval_minutes: 0,
        enabled: target.enabled,
        selected_mcp_server_ids: target.selected_mcp_server_ids.clone(),
        provider_connection_ids: target.provider_connection_ids.clone(),
        selected_skill_ids: target.selected_skill_ids.clone(),
        execution: target.execution.clone(),
        created_at: String::new(),
        updated_at: String::new(),
    };

    let creator_workspace_agent_id = find_workspace_agent_for_definition(
        app_state.inner(),
        &workspace_id,
        context.automation_id.as_deref(),
    )?;

    let task_id = uuid::Uuid::new_v4().to_string();
    create_task_row(
        &deps.pool,
        &task_id,
        creator_workspace_agent_id.as_deref(),
        &target,
        title,
        instructions,
    )
    .await?;

    let connection = match resolve_first_connection(deps, &target_config).await {
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
                None,
                true,
            )
            .await?;
            let _ = emit_task_attention_event(&deps.app, &deps.pool, &workspace_id, &task_id).await;
            let task = load_task(&deps.pool, &workspace_id, &task_id).await?;
            return Ok(serde_json::json!({
                "ok": true,
                "task": task_to_response(task)?,
            }));
        }
    };

    let session = repository::create_session(
        &deps.pool,
        CreateSessionParams {
            kind: SessionKind::BackgroundJob,
            title: Some(format!("Task: {}", title)),
            context: task_session_context(deps, context, &workspace_id, &target_config),
        },
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
                     Return a concise result summary. If blocked by missing capability, context, permission, or runtime failure, start your response with `BLOCKED:`. If you specifically need user feedback or approval, start your response with `NEEDS_USER_INPUT:` and describe the decision or input needed.",
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
            provider_id: connection.provider_id.clone(),
            model_id: connection.model_id.clone(),
            usage: None,
            error: None,
        },
    )
    .await?;

    update_task_status(
        &deps.pool,
        &task_id,
        "queued",
        Some(&session.id),
        Some(&run.id),
        None,
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

    let task = load_task(&deps.pool, &workspace_id, &task_id).await?;
    Ok(serde_json::json!({
        "ok": true,
        "task": task_to_response(task)?,
    }))
}

async fn get_task_result(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    params: GetWorkspaceTaskResultParams,
) -> Result<serde_json::Value, String> {
    let workspace_id = workspace_id_from_context(context)?;
    let task = load_task(&deps.pool, &workspace_id, &params.task_id).await?;

    Ok(serde_json::json!({
        "ok": true,
        "task": task_to_response(task)?,
    }))
}

async fn request_user_input(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    params: RequestWorkspaceUserInputParams,
) -> Result<serde_json::Value, String> {
    let workspace_id = workspace_id_from_context(context)?;
    let title = params.title.trim();
    let question = params.question.trim();
    if title.is_empty() {
        return Err("User input request title is required.".to_string());
    }
    if question.is_empty() {
        return Err("User input request question is required.".to_string());
    }

    let app_state = deps.app.state::<AppState>();
    let manager = current_manager_workspace_agent_row(app_state.inner(), context, &workspace_id)?;
    let creator_workspace_agent_id = find_workspace_agent_for_definition(
        app_state.inner(),
        &workspace_id,
        context.automation_id.as_deref(),
    )?;
    let context_text = params
        .context
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let requested_action = params
        .requested_action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut instructions = format!("Question for the user:\n{}", question);
    if let Some(context_text) = context_text {
        instructions.push_str("\n\nContext:\n");
        instructions.push_str(context_text);
    }
    if let Some(requested_action) = requested_action {
        instructions.push_str("\n\nRequested action:\n");
        instructions.push_str(requested_action);
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    create_task_row(
        &deps.pool,
        &task_id,
        creator_workspace_agent_id.as_deref(),
        &manager,
        title,
        &instructions,
    )
    .await?;

    let result_json = serde_json::json!({
        "kind": "needs_user_input",
        "reason": question,
        "requestedAction": requested_action,
    });
    update_task_status(
        &deps.pool,
        &task_id,
        "needs_user_input",
        None,
        None,
        None,
        Some(question),
        Some(&result_json),
        false,
    )
    .await?;
    let _ = emit_task_attention_event(&deps.app, &deps.pool, &workspace_id, &task_id).await;

    let task = load_task(&deps.pool, &workspace_id, &task_id).await?;
    Ok(serde_json::json!({
        "ok": true,
        "task": task_to_response(task)?,
    }))
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
            None,
            false,
        )
        .await;

        let cancel_token = crate::assistant::runtime::register_run(&run_id);
        let input = RunTurnInput {
            session_id: session_id.clone(),
            run_id: Some(run_id.clone()),
            trigger: RunTrigger::WorkspaceTask,
            connection_id,
            cancel_token: cancel_token.clone(),
            inter_agent_call_depth: None,
        };

        let result = engine::run_session_turn(&deps, input).await;
        crate::assistant::runtime::unregister_run(&run_id);

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
                let result_json = summary
                    .as_ref()
                    .map(|summary| serde_json::json!({ "summary": summary }));
                let _ = update_task_status(
                    &deps.pool,
                    &task_id,
                    status,
                    Some(&session_id),
                    Some(&run_id),
                    error.as_deref(),
                    summary.as_deref(),
                    result_json.as_ref(),
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

fn workspace_id_from_context(context: &ToolExecutionContext) -> Result<String, String> {
    context
        .workspace_id
        .clone()
        .or_else(|| context.agent_workspace_id.clone())
        .ok_or_else(|| "Workspace task tools require a workspace context.".to_string())
}

fn task_session_context(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    workspace_id: &str,
    target_config: &AgentConfig,
) -> SessionContext {
    let mcp_server_ids = if context.mcp_server_ids.is_empty() {
        target_config.selected_mcp_server_ids.clone()
    } else {
        context.mcp_server_ids.clone()
    };

    let automation_description = {
        let state = deps.app.state::<AppState>();
        let config = state.config_manager.lock().map(|manager| manager.get());
        match config {
            Ok(config) => agent_instructions_with_skills(&config, target_config),
            Err(_) => target_config.description.clone(),
        }
    };

    SessionContext {
        space_id: context.space_id.clone(),
        room_id: context.room_id.clone(),
        workspace_id: Some(workspace_id.to_string()),
        tool_scopes: target_config
            .required_tools()
            .into_iter()
            .map(str::to_string)
            .collect(),
        mcp_server_ids,
        execution: target_config.execution.clone(),
        netdata_conversation_id: None,
        cli_session_id: None,
        automation_id: Some(target_config.id.clone()),
        agent_workspace_id: Some(workspace_id.to_string()),
        automation_name: Some(target_config.name.clone()),
        automation_description: Some(automation_description),
        inter_agent_call: None,
        workspace_agents: context.workspace_agents.clone(),
    }
}

async fn resolve_first_connection(
    deps: &AssistantDeps,
    config: &AgentConfig,
) -> Result<ProviderConnection, String> {
    let all = deps
        .app
        .state::<AppState>()
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connections();
    for id in &config.provider_connection_ids {
        if let Some(connection) = all
            .iter()
            .find(|connection| &connection.id == id && connection.enabled)
        {
            return Ok(connection.clone());
        }
    }
    Err(format!(
        "Agent '{}' has no active provider connection.",
        config.name
    ))
}

fn concise_agent_description(description: Option<String>) -> Option<String> {
    let text = description?;
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .to_string();

    const MAX_SUMMARY_CHARS: usize = 240;
    if first_line.chars().count() <= MAX_SUMMARY_CHARS {
        return Some(first_line);
    }

    let mut summary: String = first_line.chars().take(MAX_SUMMARY_CHARS).collect();
    summary.push_str("...");
    Some(summary)
}

fn load_workspace_agent_rows(
    state: &AppState,
    workspace_id: &str,
) -> Result<Vec<WorkspaceAgentRow>, String> {
    let app_config = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get();
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    let mut rows: Vec<_> = config
        .agents
        .iter()
        .map(|agent| workspace_agent_row_from_config(&app_config, &config, agent))
        .collect();
    rows.sort_by_key(|row| {
        (
            if row.id == config.default_agent_id {
                0
            } else {
                1
            },
            row.id.clone(),
        )
    });
    Ok(rows)
}

fn load_workspace_agent_row(
    state: &AppState,
    workspace_id: &str,
    workspace_agent_id: &str,
) -> Result<Option<WorkspaceAgentRow>, String> {
    Ok(load_workspace_agent_rows(state, workspace_id)?
        .into_iter()
        .find(|row| row.id == workspace_agent_id))
}

fn current_manager_workspace_agent_row(
    state: &AppState,
    context: &ToolExecutionContext,
    workspace_id: &str,
) -> Result<WorkspaceAgentRow, String> {
    if let Some(manager) = context
        .workspace_agents
        .iter()
        .find(|agent| agent.is_default)
    {
        if let Some(row) = load_workspace_agent_row(state, workspace_id, &manager.id)? {
            return Ok(row);
        }
    }

    if let Some(current_agent_definition_id) = context.automation_id.as_deref() {
        if let Some(workspace_agent_id) = find_workspace_agent_for_definition(
            state,
            workspace_id,
            Some(current_agent_definition_id),
        )? {
            if let Some(row) = load_workspace_agent_row(state, workspace_id, &workspace_agent_id)? {
                return Ok(row);
            }
        }
    }

    Err(
        "Could not resolve the workspace manager assignment for this user input request."
            .to_string(),
    )
}

fn find_workspace_agent_for_definition(
    state: &AppState,
    workspace_id: &str,
    agent_definition_id: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(agent_definition_id) = agent_definition_id else {
        return Ok(None);
    };

    Ok(load_workspace_agent_rows(state, workspace_id)?
        .into_iter()
        .find(|row| row.id == agent_definition_id || row.agent_definition_id == agent_definition_id)
        .map(|row| row.id))
}

fn workspace_agent_row_from_config(
    app_config: &AppConfig,
    workspace: &WorkspaceConfig,
    agent: &WorkspaceAgent,
) -> WorkspaceAgentRow {
    WorkspaceAgentRow {
        id: agent.id.clone(),
        workspace_id: workspace.id.clone(),
        agent_definition_id: agent.id.clone(),
        display_name: None,
        role: if workspace.default_agent_id == agent.id {
            "manager".to_string()
        } else {
            "member".to_string()
        },
        enabled: agent.enabled,
        name: agent.name.clone(),
        description: agent.description.clone(),
        selected_skill_ids: workspace_config::refs_to_skill_ids(app_config, &agent.selected_skills),
        selected_mcp_server_ids: workspace_config::refs_to_mcp_ids(
            app_config,
            &agent.selected_mcp_servers,
        ),
        provider_connection_ids: agent.provider_connection_ids.clone(),
        execution: agent.execution.clone(),
    }
}

async fn create_task_row(
    pool: &DbPool,
    task_id: &str,
    created_by_workspace_agent_id: Option<&str>,
    target: &WorkspaceAgentRow,
    title: &str,
    instructions: &str,
) -> Result<(), String> {
    let now = now_ms();
    sqlx::query(
        r#"
        INSERT INTO workspace_tasks (
            id, created_by_workspace_agent_id, assigned_to_workspace_agent_id,
            assigned_agent_definition_id, title, instructions, status, created_at, updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, 'queued', ?, ?)
        "#,
    )
    .bind(task_id)
    .bind(created_by_workspace_agent_id)
    .bind(&target.id)
    .bind(&target.agent_definition_id)
    .bind(title)
    .bind(instructions)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspace task: {}", e))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn update_task_status(
    pool: &DbPool,
    task_id: &str,
    status: &str,
    session_id: Option<&str>,
    run_id: Option<&str>,
    error: Option<&str>,
    result_summary: Option<&str>,
    result_json: Option<&serde_json::Value>,
    terminal: bool,
) -> Result<(), String> {
    let now = now_ms();
    sqlx::query(
        r#"
        UPDATE workspace_tasks
        SET status = ?,
            session_id = COALESCE(?, session_id),
            run_id = COALESCE(?, run_id),
            error = ?,
            result_summary = COALESCE(?, result_summary),
            result_json = COALESCE(?, result_json),
            updated_at = ?,
            completed_at = COALESCE(?, completed_at)
        WHERE id = ?
        "#,
    )
    .bind(status)
    .bind(session_id)
    .bind(run_id)
    .bind(error)
    .bind(result_summary)
    .bind(result_json.map(|value| value.to_string()))
    .bind(now)
    .bind(if terminal { Some(now) } else { None })
    .bind(task_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update workspace task: {}", e))?;

    Ok(())
}

async fn load_task(
    pool: &DbPool,
    workspace_id: &str,
    task_id: &str,
) -> Result<WorkspaceTaskRow, String> {
    let row = sqlx::query(
        r#"
        SELECT id, created_by_workspace_agent_id, assigned_to_workspace_agent_id,
               assigned_agent_definition_id, title, instructions, status, result_summary,
               result_json, error, session_id, run_id, created_at, updated_at, completed_at
        FROM workspace_tasks
        WHERE id = ?
        "#,
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load workspace task: {}", e))?
    .ok_or_else(|| format!("Workspace task not found: {}", task_id))?;

    map_task_row(&row, workspace_id)
}

fn map_task_row(
    row: &sqlx::sqlite::SqliteRow,
    workspace_id: &str,
) -> Result<WorkspaceTaskRow, String> {
    Ok(WorkspaceTaskRow {
        id: row.get("id"),
        workspace_id: workspace_id.to_string(),
        created_by_workspace_agent_id: row.get("created_by_workspace_agent_id"),
        assigned_to_workspace_agent_id: row.get("assigned_to_workspace_agent_id"),
        assigned_agent_definition_id: row.get("assigned_agent_definition_id"),
        title: row.get("title"),
        instructions: row.get("instructions"),
        status: row.get("status"),
        result_summary: row.get("result_summary"),
        result_json: row.get("result_json"),
        error: row.get("error"),
        session_id: row.get("session_id"),
        run_id: row.get("run_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        completed_at: row.get("completed_at"),
    })
}

fn task_to_response(task: WorkspaceTaskRow) -> Result<WorkspaceTaskResponse, String> {
    Ok(WorkspaceTaskResponse {
        id: task.id,
        workspace_id: task.workspace_id,
        created_by_workspace_agent_id: task.created_by_workspace_agent_id,
        assigned_to_workspace_agent_id: task.assigned_to_workspace_agent_id,
        assigned_agent_definition_id: task.assigned_agent_definition_id,
        title: task.title,
        instructions: task.instructions,
        status: task.status,
        result_summary: task.result_summary,
        result_json: task
            .result_json
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|e| format!("Failed to parse task result JSON: {}", e))?,
        error: task.error,
        session_id: task.session_id,
        run_id: task.run_id,
        created_at: task.created_at,
        updated_at: task.updated_at,
        completed_at: task.completed_at,
    })
}

async fn emit_task_attention_event(
    app: &tauri::AppHandle,
    pool: &DbPool,
    workspace_id: &str,
    task_id: &str,
) -> Result<(), String> {
    let task = load_task(pool, workspace_id, task_id).await?;
    if !is_attention_status(&task.status) {
        return Ok(());
    }

    let event = WorkspaceTaskAttentionEvent {
        task_id: task.id,
        workspace_id: task.workspace_id,
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
    matches!(status, "blocked" | "failed" | "needs_user_input")
}

fn classify_worker_status(summary: Option<&str>, fallback: &'static str) -> &'static str {
    let Some(summary) = summary.map(str::trim_start) else {
        return fallback;
    };

    if summary.starts_with("NEEDS_USER_INPUT:") || summary.starts_with("USER_INPUT:") {
        "needs_user_input"
    } else if summary.starts_with("BLOCKED:") {
        "blocked"
    } else {
        fallback
    }
}

async fn latest_assistant_text(pool: &DbPool, session_id: &str) -> Result<Option<String>, String> {
    let messages = repository::list_messages(pool, session_id).await?;
    for message in messages.iter().rev() {
        if message.role != MessageRole::Assistant {
            continue;
        }
        for part in &message.content {
            let ContentPart::Text { text } = part else {
                continue;
            };
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Ok(Some(trimmed.to_string()));
            }
        }
    }
    Ok(None)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // concise_agent_description
    // -----------------------------------------------------------------

    #[test]
    fn concise_agent_description_none_returns_none() {
        assert_eq!(concise_agent_description(None), None);
    }

    #[test]
    fn concise_agent_description_empty_returns_none() {
        assert_eq!(concise_agent_description(Some("".to_string())), None);
    }

    #[test]
    fn concise_agent_description_whitespace_only_returns_none() {
        assert_eq!(
            concise_agent_description(Some("   \n\t  ".to_string())),
            None
        );
    }

    #[test]
    fn concise_agent_description_short_single_line() {
        let input = Some("A short description.".to_string());
        assert_eq!(
            concise_agent_description(input),
            Some("A short description.".to_string())
        );
    }

    #[test]
    fn concise_agent_description_takes_first_non_empty_line() {
        let input = Some("\n\nSecond line is first non-empty.\nThird line.".to_string());
        assert_eq!(
            concise_agent_description(input),
            Some("Second line is first non-empty.".to_string())
        );
    }

    #[test]
    fn concise_agent_description_trims_leading_whitespace() {
        let input = Some("   \t  Trimmed description.".to_string());
        assert_eq!(
            concise_agent_description(input),
            Some("Trimmed description.".to_string())
        );
    }

    #[test]
    fn concise_agent_description_truncates_long_line() {
        let long = "x".repeat(300);
        let result = concise_agent_description(Some(long.clone()));
        let expected = "x".repeat(240) + "...";
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn concise_agent_description_exactly_max_length_passes_through() {
        let exact = "x".repeat(240);
        assert_eq!(concise_agent_description(Some(exact.clone())), Some(exact));
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
    fn is_attention_status_needs_user_input() {
        assert!(is_attention_status("needs_user_input"));
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
    // classify_worker_status
    // -----------------------------------------------------------------

    #[test]
    fn classify_worker_status_none_returns_fallback() {
        assert_eq!(classify_worker_status(None, "completed"), "completed");
    }

    #[test]
    fn classify_worker_status_needs_user_input_prefix() {
        assert_eq!(
            classify_worker_status(Some("NEEDS_USER_INPUT: Please review."), "completed"),
            "needs_user_input"
        );
    }

    #[test]
    fn classify_worker_status_user_input_prefix() {
        assert_eq!(
            classify_worker_status(Some("USER_INPUT: What is your decision?"), "completed"),
            "needs_user_input"
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

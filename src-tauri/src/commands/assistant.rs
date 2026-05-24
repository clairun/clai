use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository;
use crate::assistant::repository::{CreateMessageParams, CreateRunParams, CreateSessionParams};
use crate::assistant::runtime;
use crate::assistant::types::{
    AssistantMessage, AssistantRun, AssistantSession, ContentPart, MessageRole, RunStatus,
    RunTrigger, SessionContext, SessionKind, ToolInvocation,
};
use crate::config::workspace_config;
use crate::db::DbPool;
use crate::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAssistantSessionRequest {
    #[serde(default)]
    pub kind: Option<SessionKind>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub context: SessionContext,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSendMessageResult {
    pub session: AssistantSession,
    pub message: AssistantMessage,
    pub run: AssistantRun,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolCallsRequest {
    pub session_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
}

async fn session_pool(
    state: &AppState,
    session_id: &str,
) -> Result<(DbPool, AssistantSession), String> {
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();
    for locator in locators {
        let pool = state.workspace_db(&locator.id).await?;
        if let Some(session) = repository::get_session(&pool, session_id).await? {
            return Ok((pool, session));
        }
    }

    Err(format!("Assistant session not found: {}", session_id))
}

async fn run_pool(state: &AppState, run_id: &str) -> Result<(DbPool, AssistantRun), String> {
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();
    for locator in locators {
        let pool = state.workspace_db(&locator.id).await?;
        if let Some(run) = repository::get_run(&pool, run_id).await? {
            return Ok((pool, run));
        }
    }

    Err(format!("Assistant run not found: {}", run_id))
}

async fn pool_for_new_session(
    state: &AppState,
    context: &SessionContext,
) -> Result<DbPool, String> {
    let workspace_id = context
        .workspace_id
        .as_deref()
        .or(context.agent_workspace_id.as_deref())
        .ok_or_else(|| {
            "Cannot create assistant session: session context has no workspace_id".to_string()
        })?;
    state.workspace_db(workspace_id).await
}

fn provider_connection(
    state: &AppState,
    connection_id: &str,
) -> Result<crate::assistant::types::ProviderConnection, String> {
    state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get_provider_connection(connection_id)
        .ok_or_else(|| format!("Provider connection not found: {}", connection_id))
}

fn fresh_execution_for_session(
    state: &AppState,
    session: &AssistantSession,
) -> Result<Option<crate::config::ExecutionCapabilityConfig>, String> {
    let Some(agent_id) = session.context.automation_id.as_deref() else {
        return Ok(None);
    };
    let workspace_id = session
        .context
        .workspace_id
        .as_deref()
        .or(session.context.agent_workspace_id.as_deref());
    let Some(workspace_id) = workspace_id else {
        return Ok(None);
    };
    let Some(root) = state.workspace_root(workspace_id) else {
        return Ok(None);
    };
    let config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    Ok(config
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .map(|agent| agent.execution.clone())
        .filter(|execution| execution != &session.context.execution))
}

#[tauri::command]
pub async fn assistant_create_session(
    request: CreateAssistantSessionRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<AssistantSession, String> {
    let target_pool = pool_for_new_session(state.inner(), &request.context).await?;
    let session = repository::create_session(
        &target_pool,
        CreateSessionParams {
            kind: request.kind.unwrap_or(SessionKind::Interactive),
            title: request.title,
            context: request.context,
        },
    )
    .await?;

    emit_event(
        &app,
        &session,
        None,
        AssistantUiEvent::SessionCreated {
            session: Box::new(session.clone()),
        },
    )?;

    Ok(session)
}

#[tauri::command]
pub async fn assistant_get_session(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Option<AssistantSession>, String> {
    match session_pool(state.inner(), &session_id).await {
        Ok((_pool, session)) => Ok(Some(session)),
        Err(message) if message.starts_with("Assistant session not found") => Ok(None),
        Err(message) => Err(message),
    }
}

#[tauri::command]
pub async fn assistant_list_sessions(
    state: State<'_, AppState>,
) -> Result<Vec<AssistantSession>, String> {
    let mut sessions = Vec::new();
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();
    for locator in locators {
        let workspace_pool = state.workspace_db(&locator.id).await?;
        sessions.extend(repository::list_sessions(&workspace_pool).await?);
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    Ok(sessions)
}

#[tauri::command]
pub async fn assistant_delete_session(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let (target_pool, _session) = session_pool(state.inner(), &session_id).await?;
    repository::delete_session(&target_pool, &session_id).await
}

#[tauri::command]
pub async fn assistant_load_session_messages(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<AssistantMessage>, String> {
    let (target_pool, _session) = session_pool(state.inner(), &session_id).await?;
    repository::list_messages(&target_pool, &session_id).await
}

#[tauri::command]
pub async fn assistant_list_runs(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<AssistantRun>, String> {
    let (target_pool, _session) = session_pool(state.inner(), &session_id).await?;
    repository::list_runs(&target_pool, &session_id).await
}

#[tauri::command]
pub async fn assistant_list_tool_calls(
    request: ListToolCallsRequest,
    state: State<'_, AppState>,
) -> Result<Vec<ToolInvocation>, String> {
    let (target_pool, _session) = session_pool(state.inner(), &request.session_id).await?;
    repository::list_tool_calls(&target_pool, &request.session_id, request.run_id.as_deref()).await
}

#[tauri::command]
pub async fn assistant_send_message(
    session_id: String,
    message: String,
    connection_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AssistantSendMessageResult, String> {
    let (target_pool, mut session) = session_pool(state.inner(), &session_id).await?;

    // If tied to a workspace agent (manager), sync execution config with the
    // latest workspace_agents row so config changes take effect immediately.
    // Phase 1.4: the row's inline `execution` column is the source of truth.
    let needs_execution_update = fresh_execution_for_session(state.inner(), &session)?;
    if let Some(fresh_execution) = needs_execution_update {
        session.context.execution = fresh_execution;
        session.updated_at = chrono::Utc::now().timestamp_millis();
        session = repository::update_session(&target_pool, &session).await?;
    }

    let assistant_message = repository::create_message(
        &target_pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::User,
            content: vec![ContentPart::Text { text: message }],
            provider_metadata: None,
        },
    )
    .await?;

    let connection = provider_connection(state.inner(), &connection_id)?;

    let run = repository::create_run(
        &target_pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: connection_id.clone(),
            provider_id: connection.provider_id.clone(),
            model_id: connection.model_id.clone(),
            usage: None,
            error: None,
        },
    )
    .await?;

    emit_event(
        &app,
        &session,
        Some(&run.id),
        AssistantUiEvent::MessageCreated {
            message: assistant_message.clone(),
        },
    )?;
    emit_event(
        &app,
        &session,
        Some(&run.id),
        AssistantUiEvent::RunQueued { run: run.clone() },
    )?;

    spawn_run_task(
        target_pool.clone(),
        app.clone(),
        session.id.clone(),
        run.id.clone(),
        RunTrigger::UserMessage,
        connection_id,
    );

    Ok(AssistantSendMessageResult {
        session,
        message: assistant_message,
        run,
    })
}

#[tauri::command]
pub async fn assistant_retry_run(
    run_id: String,
    connection_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AssistantRun, String> {
    let (target_pool, previous_run) = run_pool(state.inner(), &run_id).await?;

    let session = repository::get_session(&target_pool, &previous_run.session_id)
        .await?
        .ok_or_else(|| format!("Assistant session not found: {}", previous_run.session_id))?;

    let connection = provider_connection(state.inner(), &connection_id)?;

    let run = repository::create_run(
        &target_pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::Retry,
            connection_id: connection_id.clone(),
            provider_id: connection.provider_id.clone(),
            model_id: connection.model_id.clone(),
            usage: None,
            error: None,
        },
    )
    .await?;

    emit_event(
        &app,
        &session,
        Some(&run.id),
        AssistantUiEvent::RunQueued { run: run.clone() },
    )?;

    spawn_run_task(
        target_pool.clone(),
        app,
        session.id,
        run.id.clone(),
        RunTrigger::Retry,
        connection_id,
    );

    Ok(run)
}

#[tauri::command]
pub async fn assistant_cancel_run(
    run_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AssistantRun, String> {
    let (target_pool, run) = run_pool(state.inner(), &run_id).await?;

    if matches!(
        run.status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    ) {
        return Ok(run);
    }

    if runtime::cancel_run(&run_id) {
        return Ok(run);
    }

    let session = repository::get_session(&target_pool, &run.session_id)
        .await?
        .ok_or_else(|| format!("Assistant session not found: {}", run.session_id))?;

    let cancelled =
        repository::update_run_status(&target_pool, &run_id, RunStatus::Cancelled, None).await?;

    emit_event(
        &app,
        &session,
        Some(&run_id),
        AssistantUiEvent::RunCancelled {
            run: cancelled.clone(),
        },
    )?;

    Ok(cancelled)
}

fn spawn_run_task(
    pool: DbPool,
    app: AppHandle,
    session_id: String,
    run_id: String,
    trigger: RunTrigger,
    connection_id: String,
) {
    let cancel_token = runtime::register_run(&run_id);
    tauri::async_runtime::spawn(async move {
        let deps = AssistantDeps { pool, app };
        let input = RunTurnInput {
            session_id,
            run_id: Some(run_id.clone()),
            trigger,
            connection_id,
            cancel_token,
            inter_agent_call_depth: None,
        };
        if let Err(e) = engine::run_session_turn(&deps, input).await {
            tracing::error!("Assistant engine error for run {}: {}", run_id, e);
        }
        runtime::unregister_run(&run_id);
    });
}

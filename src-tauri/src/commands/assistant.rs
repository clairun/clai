use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::assistant::compaction;
use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository;
use crate::assistant::repository::{CreateRunParams, CreateSessionParams};
use crate::assistant::runtime;
use crate::assistant::tools::ask_user::{self, AskUserAnswer};
use crate::assistant::types::{
    AssistantCompaction, AssistantMessage, AssistantMessageCursor, AssistantMessagePage,
    AssistantRun, AssistantSession, CompactionTrigger, ContentPart, RunStatus, RunTrigger,
    SessionContext, SessionKind, ToolInvocation,
};
use crate::config::workspace_config;
use crate::db::DbPool;
use crate::AppState;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAssistantSessionRequest {
    #[serde(default)]
    pub kind: Option<SessionKind>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub context: SessionContext,
    #[serde(default)]
    pub parent_session_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSendMessageResult {
    pub session: AssistantSession,
    pub message: AssistantMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<AssistantRun>,
    pub queued: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCompactionResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<AssistantCompaction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_message: Option<AssistantMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolCallsRequest {
    pub session_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionMessagesPageRequest {
    pub session_id: String,
    #[serde(default)]
    pub before: Option<AssistantMessageCursor>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub include_ancestors: bool,
}

const DEFAULT_MESSAGE_PAGE_LIMIT: u32 = 100;
const MAX_MESSAGE_PAGE_LIMIT: u32 = 500;

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
    if let Some(parent_session_id) = request.parent_session_id.as_deref() {
        if repository::get_session(&target_pool, parent_session_id)
            .await?
            .is_none()
        {
            return Err(format!(
                "Parent assistant session not found in this workspace: {}",
                parent_session_id
            ));
        }
    }
    let session = repository::create_session(
        &target_pool,
        CreateSessionParams {
            kind: request.kind.unwrap_or(SessionKind::Interactive),
            title: request.title,
            context: request.context,
        },
    )
    .await?;

    if let Some(parent_session_id) = request.parent_session_id.as_deref() {
        repository::create_session_rotation_link(&target_pool, &session.id, parent_session_id)
            .await?;
    }

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
    let (target_pool, session) = session_pool(state.inner(), &session_id).await?;
    // Hard clear: the schema cascades messages/runs/tool calls/compactions.
    // Refuse while a run is in flight — the engine would keep writing rows
    // for (and emitting events about) a session that no longer exists.
    if repository::session_has_active_run(&target_pool, &session_id).await? {
        return Err(
            "Wait for the current assistant run to finish before clearing the conversation."
                .to_string(),
        );
    }

    // Collect this session's image paths *before* the cascade (the DB
    // cascade kills the messages that point at them). Then delete the
    // session, then sweep the files from `.clai/images/`. Scoped to
    // this session's image refs and validated through
    // `is_store_relative_path`, so we never touch files referenced by
    // other sessions sharing the same workspace root, and a hostile
    // crafted `path` can only ever match a real store file (already
    // validated at send time anyway).
    let image_paths = collect_session_image_paths(&target_pool, &session_id).await?;
    let deleted = repository::delete_session(&target_pool, &session_id).await?;
    if deleted {
        sweep_session_images(state.inner(), &session, image_paths).await;
    }
    Ok(deleted)
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
pub async fn assistant_load_session_messages_page(
    request: LoadSessionMessagesPageRequest,
    state: State<'_, AppState>,
) -> Result<AssistantMessagePage, String> {
    let (target_pool, _session) = session_pool(state.inner(), &request.session_id).await?;
    let limit = request
        .limit
        .unwrap_or(DEFAULT_MESSAGE_PAGE_LIMIT)
        .clamp(1, MAX_MESSAGE_PAGE_LIMIT) as usize;
    let mut remaining = limit;
    let mut cursor = request.before;
    let mut current_session_id = cursor
        .as_ref()
        .map(|cursor| cursor.session_id.clone())
        .unwrap_or_else(|| request.session_id.clone());
    let mut segments: Vec<Vec<AssistantMessage>> = Vec::new();
    let mut next_cursor: Option<AssistantMessageCursor> = None;
    let mut has_more = false;

    while remaining > 0 {
        let before = cursor
            .as_ref()
            .filter(|cursor| cursor.session_id == current_session_id)
            .map(|cursor| (cursor.created_at, cursor.message_id.as_str()));
        let mut newest_first = repository::list_messages_before(
            &target_pool,
            &current_session_id,
            before,
            remaining as i64 + 1,
        )
        .await?;

        if newest_first.len() > remaining {
            newest_first.truncate(remaining);
            has_more = true;
        }

        let mut segment = newest_first;
        segment.reverse();
        if let Some(oldest) = segment.first() {
            next_cursor = Some(AssistantMessageCursor {
                session_id: oldest.session_id.clone(),
                created_at: oldest.created_at,
                message_id: oldest.id.clone(),
            });
        }
        remaining = remaining.saturating_sub(segment.len());
        if !segment.is_empty() {
            segments.push(segment);
        }

        if has_more || !request.include_ancestors {
            break;
        }

        if remaining == 0 {
            if repository::parent_session_id(&target_pool, &current_session_id)
                .await?
                .is_some()
            {
                has_more = true;
            }
            break;
        }

        match repository::parent_session_id(&target_pool, &current_session_id).await? {
            Some(parent_session_id) => {
                current_session_id = parent_session_id;
                cursor = None;
            }
            None => {
                next_cursor = None;
                break;
            }
        }
    }

    let mut messages = Vec::new();
    for segment in segments.into_iter().rev() {
        messages.extend(segment);
    }
    if messages.is_empty() || !has_more {
        next_cursor = None;
    }

    let mut tool_call_ids: Vec<String> = Vec::new();
    for message in &messages {
        for part in &message.content {
            let tool_call_id = match part {
                ContentPart::ToolUse { tool_call_id, .. }
                | ContentPart::ToolResult { tool_call_id, .. } => tool_call_id,
                _ => continue,
            };
            if !tool_call_ids
                .iter()
                .any(|existing| existing == tool_call_id)
            {
                tool_call_ids.push(tool_call_id.clone());
            }
        }
    }
    let tool_calls = repository::list_tool_calls_by_ids(&target_pool, &tool_call_ids).await?;

    // Counted from the *requested* session (not the cursor's), so the total
    // always covers the full conversation regardless of how deep into the
    // chain pagination has walked.
    let total_count = repository::count_session_chain_messages(
        &target_pool,
        &request.session_id,
        request.include_ancestors,
    )
    .await?;

    Ok(AssistantMessagePage {
        messages,
        tool_calls,
        next_cursor,
        has_more,
        total_count,
    })
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

/// Validate the attachments on a user send: only `ContentPart::Image` parts may
/// ride a user message, so the frontend can't smuggle tool/assistant content in.
/// Walk a session's messages and return the set of image-store
/// relative paths referenced by its `ContentPart::Image` parts. Used
/// by clear to know which on-disk files became orphaned by the DB
/// cascade.
async fn collect_session_image_paths(
    pool: &DbPool,
    session_id: &str,
) -> Result<HashSet<String>, String> {
    let messages = repository::list_messages(pool, session_id).await?;
    let mut paths = HashSet::new();
    for message in messages {
        for part in message.content {
            if let ContentPart::Image { path, .. } = part {
                // Defensive: only ever record paths the store would
                // emit, even though send-time validation already
                // rejected anything else.
                if crate::assistant::image_store::is_store_relative_path(&path) {
                    paths.insert(path);
                }
            }
        }
    }
    Ok(paths)
}

/// Pure helper: turn the workspace root + a set of store-relative
/// image paths into the absolute file paths on disk. The `path`s
/// come from `ContentPart::Image.path` and already start with
/// `.clai/images/<uuid>.<ext>` (validated by
/// `image_store::is_store_relative_path`), so we join them directly
/// under the root — *not* under `<root>/.clai/images/`, which would
/// double the prefix and miss every file. Kept pure so the join
/// logic is testable without spinning up a DB.
fn resolve_session_image_files(root: &Path, paths: &HashSet<String>) -> Vec<PathBuf> {
    paths.iter().map(|p| root.join(p)).collect()
}

/// Delete each collected image file from `<workspace>/.clai/images/`.
/// Resolves the workspace root from the session context; a missing
/// root, missing file, or any other per-file error is logged and
/// skipped — never blocks the user-facing clear result.
async fn sweep_session_images(
    state: &AppState,
    session: &AssistantSession,
    paths: HashSet<String>,
) {
    if paths.is_empty() {
        return;
    }
    let Some(workspace_id) = session
        .context
        .workspace_id
        .as_deref()
        .or(session.context.agent_workspace_id.as_deref())
    else {
        return;
    };
    let Some(root): Option<PathBuf> = state.workspace_root(workspace_id) else {
        return;
    };
    for file in resolve_session_image_files(&root, &paths) {
        match tokio::fs::remove_file(&file).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    session_id = %session.id,
                    file = %file.display(),
                    error = %error,
                    "Clear: failed to remove orphaned image file; ignoring"
                );
            }
        }
    }
}

fn validate_send_images(images: &[ContentPart]) -> Result<(), String> {
    for part in images {
        match part {
            // The path must be a store-owned reference (`.clai/images/<uuid>.<ext>`).
            // Without this check a crafted send could carry an absolute or `..`
            // path, which the send path would `root.join` + base64 + ship to the
            // model — an arbitrary-local-file read/exfiltration hole.
            ContentPart::Image { path, .. } => {
                if !crate::assistant::image_store::is_store_relative_path(path) {
                    return Err(format!(
                        "assistant_send_message: image attachment path is not a workspace                          image-store reference (expected .clai/images/<uuid>.<ext>): {:?}",
                        path
                    ));
                }
            }
            // Only image parts may ride a user send, so the frontend can't
            // smuggle tool/assistant content into a user message.
            bad => {
                return Err(format!(
                    "assistant_send_message only accepts image attachments, got: {:?}",
                    bad
                ));
            }
        }
    }
    Ok(())
}

/// Whether the given connection's active model accepts image input. Drives the
/// composer's paste/attach affordance — a single source of truth so the UI gate
/// can't drift from the backend send-filter. See
/// [`crate::assistant::providers::connection_supports_images`].
#[tauri::command]
pub async fn assistant_connection_supports_images(
    connection_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let connection = provider_connection(state.inner(), &connection_id)?;
    Ok(crate::assistant::providers::connection_supports_images(
        &connection,
    ))
}

#[tauri::command]
pub async fn assistant_send_message(
    session_id: String,
    message: String,
    connection_id: String,
    images: Option<Vec<ContentPart>>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AssistantSendMessageResult, String> {
    // Only image parts may ride a user send; reject anything else so the
    // frontend can't smuggle tool/assistant content into a user message.
    let images = images.unwrap_or_default();
    validate_send_images(&images)?;
    let (target_pool, mut session) = session_pool(state.inner(), &session_id).await?;
    let connection = provider_connection(state.inner(), &connection_id)?;
    let active_run = repository::get_active_run(&target_pool, &session.id).await?;
    let has_pending_queue = !repository::list_pending_queued_messages(&target_pool, &session.id)
        .await?
        .is_empty();

    if active_run.is_none() {
        // If tied to a workspace agent (manager), sync execution config with the
        // latest workspace_agents row so config changes take effect immediately.
        // Phase 1.4: the row's inline `execution` column is the source of truth.
        let needs_execution_update = fresh_execution_for_session(state.inner(), &session)?;
        if let Some(fresh_execution) = needs_execution_update {
            session.context.execution = fresh_execution;
            session.updated_at = chrono::Utc::now().timestamp_millis();
            session = repository::update_session(&target_pool, &session).await?;
        }
    }

    let queue_message = active_run.is_some() || has_pending_queue;
    let mut content = vec![ContentPart::Text { text: message }];
    content.extend(images);
    let assistant_message = repository::create_user_message_with_content(
        &target_pool,
        session.id.clone(),
        content,
        queue_message.then_some(connection_id.as_str()),
    )
    .await?;

    if let Some(run) = active_run {
        emit_event(
            &app,
            &session,
            Some(&run.id),
            AssistantUiEvent::MessageCreated {
                message: assistant_message.clone(),
            },
        )?;

        return Ok(AssistantSendMessageResult {
            session,
            message: assistant_message,
            run: Some(run),
            queued: true,
        });
    }

    if has_pending_queue {
        emit_event(
            &app,
            &session,
            None,
            AssistantUiEvent::MessageCreated {
                message: assistant_message.clone(),
            },
        )?;

        let run =
            start_queued_followup_if_idle(target_pool.clone(), app.clone(), session.id.clone())
                .await?;

        return Ok(AssistantSendMessageResult {
            session,
            message: assistant_message,
            run,
            queued: false,
        });
    }

    let run = repository::create_run(
        &target_pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: connection_id.clone(),
            protocol_id: connection.protocol_id.clone(),
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
        Some(assistant_message.id.clone()),
    );

    Ok(AssistantSendMessageResult {
        session,
        message: assistant_message,
        run: Some(run),
        queued: false,
    })
}

#[tauri::command]
pub async fn assistant_compact_session(
    session_id: String,
    connection_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AssistantCompactionResult, String> {
    let (target_pool, mut session) = session_pool(state.inner(), &session_id).await?;
    let connection = provider_connection(state.inner(), &connection_id)?;
    if repository::session_has_active_run(&target_pool, &session.id).await? {
        return Err("Wait for the current assistant run to finish before compacting.".to_string());
    }

    let outcome = compaction::compact_session_history(
        &target_pool,
        &session,
        &connection,
        CompactionTrigger::Manual,
        None,
        true,
    )
    .await?;

    let Some(outcome) = outcome else {
        return Ok(AssistantCompactionResult {
            compaction: None,
            summary_message: None,
        });
    };

    if crate::assistant::providers::is_cli_provider(&connection.protocol_id) {
        compaction::reset_cli_session_for_rotation(&target_pool, &mut session).await?;
    }

    emit_event(
        &app,
        &session,
        None,
        AssistantUiEvent::SessionCompacted {
            compaction: outcome.compaction.clone(),
            summary_message: outcome.summary_message.clone(),
        },
    )?;

    Ok(AssistantCompactionResult {
        compaction: Some(outcome.compaction),
        summary_message: Some(outcome.summary_message),
    })
}

/// Delete a user message that is still waiting in the queue (written while
/// a run was active, not yet picked up). Atomic against delivery: if a run
/// grabbed it in the meantime, this errors and the message stays. Emits
/// `MessageDeleted` on success so every open view drops it.
#[tauri::command]
pub async fn assistant_delete_queued_message(
    session_id: String,
    message_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (target_pool, session) = session_pool(state.inner(), &session_id).await?;
    let deleted =
        repository::delete_pending_queued_message(&target_pool, &session.id, &message_id).await?;
    if !deleted {
        return Err(
            "This message was already picked up by the agent and can no longer be removed."
                .to_string(),
        );
    }
    emit_event(
        &app,
        &session,
        None,
        AssistantUiEvent::MessageDeleted { message_id },
    )?;
    Ok(())
}

/// Edit the text of a user message that is still waiting in the queue
/// (written while a run was active, not yet picked up). Atomic against
/// delivery: if a run grabbed it in the meantime, this errors and the
/// message stays as-is. Emits `AssistantMessageUpdated` on success so every
/// open view swaps in the new text.
#[tauri::command]
pub async fn assistant_edit_queued_message(
    session_id: String,
    message_id: String,
    text: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("A queued message can't be empty — delete it instead.".to_string());
    }
    let (target_pool, session) = session_pool(state.inner(), &session_id).await?;
    let updated =
        repository::update_pending_queued_message(&target_pool, &session.id, &message_id, text)
            .await?;
    let Some(message) = updated else {
        return Err(
            "This message was already picked up by the agent and can no longer be edited."
                .to_string(),
        );
    };
    emit_event(
        &app,
        &session,
        None,
        AssistantUiEvent::AssistantMessageUpdated { message },
    )?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSubmitUserInputRequest {
    /// Matches the `pending_id` carried on the `AskUserRequested` event.
    pub pending_id: String,
    /// The user's answer text. For option-bearing questions this is the
    /// selected option's label (or the "Other" free-text). For free-text
    /// questions it's the textarea contents.
    pub answer: String,
    /// 0-based index into the question's `options` array when the user
    /// picked a structured option (rather than typing free text via
    /// "Other"). Omitted for plain-text questions.
    #[serde(default)]
    pub selected_option_index: Option<usize>,
    /// Multi-select questions: every picked option's 0-based index, in
    /// option order. Omitted for single-select / free-text questions.
    #[serde(default)]
    pub selected_option_indexes: Option<Vec<usize>>,
}

/// Deliver an answer from the FE back to the blocking `ask_user` tool
/// invocation identified by `pending_id`. Errors when no pending entry
/// matches (e.g. the run already ended or the user submitted twice).
#[tauri::command]
pub async fn assistant_submit_user_input(
    request: AssistantSubmitUserInputRequest,
) -> Result<(), String> {
    ask_user::submit_answer(
        &request.pending_id,
        AskUserAnswer {
            text: request.answer,
            selected_option_index: request.selected_option_index,
            selected_option_indexes: request.selected_option_indexes,
        },
    )
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
            protocol_id: connection.protocol_id.clone(),
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
        None,
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

pub(crate) fn spawn_run_task(
    pool: DbPool,
    app: AppHandle,
    session_id: String,
    run_id: String,
    trigger: RunTrigger,
    connection_id: String,
    // Id of the user message that triggered this run (direct send path
    // only) — lets the engine discard it if the run fails before the
    // provider produces anything. Queued-followup runs pass None; their
    // messages are linked via assistant_message_queue.delivered_run_id.
    trigger_message_id: Option<String>,
) {
    let run_registration = runtime::register_run(&run_id);
    let cancel_token = run_registration.token();
    tauri::async_runtime::spawn(async move {
        let _run_registration = run_registration;
        let deps = AssistantDeps {
            pool: pool.clone(),
            app: app.clone(),
        };
        let input = RunTurnInput {
            session_id: session_id.clone(),
            run_id: Some(run_id.clone()),
            trigger,
            connection_id,
            cancel_token,
            inter_agent_call_depth: None,
            trigger_message_id,
        };
        if let Err(e) = engine::run_session_turn(&deps, input).await {
            tracing::error!("Assistant engine error for run {}: {}", run_id, e);
        }
        if let Err(e) =
            start_queued_followup_if_idle(pool.clone(), app.clone(), session_id.clone()).await
        {
            tracing::error!(
                session_id = %session_id,
                error = %e,
                "Failed to start queued assistant follow-up"
            );
        }
    });
}

pub(crate) async fn start_queued_followup_if_idle(
    pool: DbPool,
    app: AppHandle,
    session_id: String,
) -> Result<Option<AssistantRun>, String> {
    if repository::session_has_active_run(&pool, &session_id).await? {
        return Ok(None);
    }

    let pending = repository::list_pending_queued_messages(&pool, &session_id).await?;
    if pending.is_empty() {
        return Ok(None);
    }

    let mut session = repository::get_session(&pool, &session_id)
        .await?
        .ok_or_else(|| format!("Assistant session not found: {}", session_id))?;

    let app_state = app.state::<AppState>();
    if let Some(fresh_execution) = fresh_execution_for_session(app_state.inner(), &session)? {
        session.context.execution = fresh_execution;
        session.updated_at = chrono::Utc::now().timestamp_millis();
        session = repository::update_session(&pool, &session).await?;
    }

    let connection_id = pending[0].connection_id.clone();
    let connection = provider_connection(app_state.inner(), &connection_id)?;
    let run = repository::create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: connection_id.clone(),
            protocol_id: connection.protocol_id.clone(),
            model_id: connection.model_id.clone(),
            usage: None,
            error: None,
        },
    )
    .await?;

    let message_ids: Vec<String> = pending
        .iter()
        .map(|queued| queued.message.id.clone())
        .collect();
    if let Err(error) =
        repository::mark_queued_messages_delivered(&pool, &session.id, &run.id, &message_ids).await
    {
        let _ =
            repository::update_run_status(&pool, &run.id, RunStatus::Failed, Some(&error)).await;
        return Err(error);
    }

    // The pending messages now belong to this follow-up run — clear their
    // "Queued" chips in the FE.
    let _ = emit_event(
        &app,
        &session,
        Some(&run.id),
        AssistantUiEvent::QueuedMessagesDelivered {
            message_ids: message_ids.clone(),
        },
    );

    emit_event(
        &app,
        &session,
        Some(&run.id),
        AssistantUiEvent::RunQueued { run: run.clone() },
    )?;

    spawn_run_task(
        pool,
        app,
        session.id,
        run.id.clone(),
        RunTrigger::UserMessage,
        connection_id,
        // Followup runs find their input via the queue table
        // (delivered_run_id), not a direct trigger message.
        None,
    );

    Ok(Some(run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn image() -> ContentPart {
        ContentPart::Image {
            id: "img-1".into(),
            path: ".clai/images/00000000-0000-4000-8000-000000000000.png".into(),
            media_type: "image/png".into(),
            filename: None,
            width: None,
            height: None,
        }
    }

    fn image_with_path(path: &str) -> ContentPart {
        ContentPart::Image {
            id: "img-x".into(),
            path: path.into(),
            media_type: "image/png".into(),
            filename: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn validate_send_images_accepts_only_images() {
        assert!(validate_send_images(&[]).is_ok());
        assert!(validate_send_images(&[image(), image()]).is_ok());
    }

    #[test]
    fn validate_send_images_rejects_non_store_paths() {
        // Absolute path → the arbitrary-file-read/exfiltration vector.
        let err = validate_send_images(&[image_with_path("/etc/passwd")]).unwrap_err();
        assert!(err.contains("image-store reference"), "got: {err}");
        // Parent-dir traversal → rejected.
        assert!(validate_send_images(&[image_with_path("../../secret.png")]).is_err());
        // Non-UUID stem under the store → rejected.
        assert!(validate_send_images(&[image_with_path(".clai/images/passwd.png")]).is_err());
    }

    #[test]
    fn validate_send_images_rejects_non_image_parts() {
        let err = validate_send_images(&[
            image(),
            ContentPart::Text {
                text: "sneaky".into(),
            },
        ])
        .unwrap_err();
        assert!(err.contains("only accepts image attachments"));

        assert!(validate_send_images(&[ContentPart::ToolResult {
            tool_call_id: "t1".into(),
            payload: json!({}),
            started_at: None,
            completed_at: None,
        }])
        .is_err());
    }

    #[test]
    fn resolve_session_image_files_joins_root_and_relative_paths() {
        use std::path::Path;
        let root = Path::new("/workspaces/foo");
        let mut paths = HashSet::new();
        paths.insert(".clai/images/uuid-a.png".to_string());
        paths.insert(".clai/images/uuid-b.jpg".to_string());
        let files = resolve_session_image_files(root, &paths);
        // Normalize Windows `\` to `/` so the assertion is separator-agnostic
        // (no-op on Unix). `root.join(p)` yields `\` joins on Windows.
        let mut display: Vec<String> = files
            .iter()
            .map(|p| p.display().to_string().replace('\\', "/"))
            .collect();
        display.sort();
        assert_eq!(
            display,
            vec![
                "/workspaces/foo/.clai/images/uuid-a.png".to_string(),
                "/workspaces/foo/.clai/images/uuid-b.jpg".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_session_image_files_is_a_noop_for_empty_set() {
        use std::path::Path;
        let files = resolve_session_image_files(Path::new("/anywhere"), &HashSet::new());
        assert!(files.is_empty());
    }
}

use crate::assistant::cli_session::reset_cli_session_for_rotation;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::assistant::compaction;
use crate::assistant::display;
use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository;
use crate::assistant::repository::CreateRunParams;
use crate::assistant::runtime;
use crate::assistant::tools::ask_user::{self, AskUserAnswer};
use crate::assistant::types::{
    AssistantCompaction, AssistantMessage, AssistantMessageCursor, AssistantMessagePage,
    AssistantRun, AssistantSession, CompactionTrigger, ContentPart, RunStatus, RunTrigger,
};
use crate::db::DbPool;
use crate::AppState;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
pub struct LoadSessionMessagesPageRequest {
    pub workspace_id: String,
    pub session_id: String,
    #[serde(default)]
    pub before: Option<AssistantMessageCursor>,
    #[serde(default)]
    pub limit: Option<u32>,
}

const DEFAULT_MESSAGE_PAGE_LIMIT: u32 = 100;
const MAX_MESSAGE_PAGE_LIMIT: u32 = 500;

async fn session_in_workspace(
    state: &AppState,
    workspace_id: &str,
    session_id: &str,
) -> Result<(DbPool, AssistantSession), String> {
    let pool = state.workspace_db(workspace_id).await?;
    let session = repository::get_session(&pool, session_id)
        .await?
        .ok_or_else(|| format!("Assistant session not found: {}", session_id))?;
    Ok((pool, session))
}

async fn run_in_workspace(
    state: &AppState,
    workspace_id: &str,
    run_id: &str,
) -> Result<(DbPool, AssistantRun), String> {
    let pool = state.workspace_db(workspace_id).await?;
    let run = repository::get_run(&pool, run_id)
        .await?
        .ok_or_else(|| format!("Assistant run not found: {}", run_id))?;
    Ok((pool, run))
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

#[tauri::command]
pub async fn assistant_get_session(
    workspace_id: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Option<AssistantSession>, String> {
    let pool = state.workspace_db(&workspace_id).await?;
    repository::get_session(&pool, &session_id).await
}

#[tauri::command]
pub async fn assistant_delete_session(
    workspace_id: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let (target_pool, _session) =
        session_in_workspace(state.inner(), &workspace_id, &session_id).await?;
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
        sweep_session_images(state.inner(), &workspace_id, &session_id, image_paths).await;
    }
    Ok(deleted)
}

#[tauri::command]
pub async fn assistant_load_session_messages_page(
    request: LoadSessionMessagesPageRequest,
    state: State<'_, AppState>,
) -> Result<AssistantMessagePage, String> {
    let (target_pool, _session) =
        session_in_workspace(state.inner(), &request.workspace_id, &request.session_id).await?;
    let limit = request
        .limit
        .unwrap_or(DEFAULT_MESSAGE_PAGE_LIMIT)
        .clamp(1, MAX_MESSAGE_PAGE_LIMIT) as usize;
    let before = request
        .before
        .as_ref()
        .map(|cursor| (cursor.created_at, cursor.message_id.as_str()));
    let newest_first = repository::list_messages_before(
        &target_pool,
        &request.session_id,
        before,
        limit as i64 + 1,
    )
    .await?;
    let (messages, has_more, next_cursor) = build_message_page(newest_first, limit);

    let mut tool_call_ids: Vec<String> = Vec::new();
    for message in &messages {
        for part in &message.content {
            let (ContentPart::ToolUse { tool_call_id, .. }
            | ContentPart::ToolResult { tool_call_id, .. }) = part
            else {
                continue;
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

    let total_count = repository::count_session_messages(&target_pool, &request.session_id).await?;

    Ok(display::page(AssistantMessagePage {
        messages,
        tool_calls,
        next_cursor,
        has_more,
        total_count,
    }))
}

/// Turns `limit + 1` newest-first rows into an ascending page: the extra row
/// only signals `has_more`, and the cursor points at the oldest returned row.
fn build_message_page(
    mut newest_first: Vec<AssistantMessage>,
    limit: usize,
) -> (Vec<AssistantMessage>, bool, Option<AssistantMessageCursor>) {
    let has_more = newest_first.len() > limit;
    newest_first.truncate(limit);
    newest_first.reverse();
    let next_cursor =
        newest_first
            .first()
            .filter(|_| has_more)
            .map(|oldest| AssistantMessageCursor {
                session_id: oldest.session_id.clone(),
                created_at: oldest.created_at,
                message_id: oldest.id.clone(),
            });
    (newest_first, has_more, next_cursor)
}

/// Returns only the session's newest run plus its active run (if older),
/// newest first — not the run history.
#[tauri::command]
pub async fn assistant_list_runs(
    workspace_id: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<AssistantRun>, String> {
    let (target_pool, _session) =
        session_in_workspace(state.inner(), &workspace_id, &session_id).await?;
    repository::current_runs(&target_pool, &session_id).await
}

#[tauri::command]
pub async fn assistant_get_tool_call_input(
    workspace_id: String,
    session_id: String,
    tool_call_id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let (target_pool, _session) =
        session_in_workspace(state.inner(), &workspace_id, &session_id).await?;
    repository::get_tool_call_for_session(&target_pool, &session_id, &tool_call_id)
        .await?
        .map(|call| call.params)
        .ok_or_else(|| format!("Tool call not found in session: {}", tool_call_id))
}

#[tauri::command]
pub async fn assistant_get_tool_call_result(
    workspace_id: String,
    session_id: String,
    tool_call_id: String,
    state: State<'_, AppState>,
) -> Result<Option<serde_json::Value>, String> {
    let (target_pool, _session) =
        session_in_workspace(state.inner(), &workspace_id, &session_id).await?;
    repository::get_tool_call_for_session(&target_pool, &session_id, &tool_call_id)
        .await?
        .map(|call| call.result)
        .ok_or_else(|| format!("Tool call not found in session: {}", tool_call_id))
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
    workspace_id: &str,
    session_id: &str,
    paths: HashSet<String>,
) {
    if paths.is_empty() {
        return;
    }
    let Some(root): Option<PathBuf> = state.workspace_root(workspace_id) else {
        return;
    };
    for file in resolve_session_image_files(&root, &paths) {
        match tokio::fs::remove_file(&file).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    session_id = %session_id,
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
#[expect(
    clippy::too_many_lines,
    reason = "lint debt: 105 lines against a 100-line budget; split it, do not raise the budget"
)]
pub async fn assistant_send_message(
    workspace_id: String,
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
    let (target_pool, session) =
        session_in_workspace(state.inner(), &workspace_id, &session_id).await?;
    let connection = provider_connection(state.inner(), &connection_id)?;
    let active_run = repository::get_active_run(&target_pool, &session.id).await?;
    let has_pending_queue = !repository::list_pending_queued_messages(&target_pool, &session.id)
        .await?
        .is_empty();

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
            &workspace_id,
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
            &workspace_id,
            &session,
            None,
            AssistantUiEvent::MessageCreated {
                message: assistant_message.clone(),
            },
        )?;

        let run = start_queued_followup_if_idle(
            AssistantDeps {
                workspace_id: workspace_id.clone(),
                pool: target_pool.clone(),
                app: app.clone(),
            },
            session.id.clone(),
        )
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
            error: None,
        },
    )
    .await?;

    emit_event(
        &app,
        &workspace_id,
        &session,
        Some(&run.id),
        AssistantUiEvent::MessageCreated {
            message: assistant_message.clone(),
        },
    )?;
    emit_event(
        &app,
        &workspace_id,
        &session,
        Some(&run.id),
        AssistantUiEvent::RunQueued { run: run.clone() },
    )?;

    spawn_run_task(
        AssistantDeps {
            workspace_id,
            pool: target_pool.clone(),
            app: app.clone(),
        },
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
    workspace_id: String,
    session_id: String,
    connection_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AssistantCompactionResult, String> {
    let (target_pool, mut session) =
        session_in_workspace(state.inner(), &workspace_id, &session_id).await?;
    let connection = provider_connection(state.inner(), &connection_id)?;
    if repository::session_has_active_run(&target_pool, &session.id).await? {
        return Err("Wait for the current assistant run to finish before compacting.".to_string());
    }

    let summary_working_dir = state.workspace_root(&workspace_id);
    // No run owns this session's conversation while it is idle, so the manual
    // command loads it once and compacts through the same core the runs use.
    let mut conversation =
        crate::assistant::compaction_service::load_with_pending(&target_pool, &session.id).await?;
    let outcome = crate::assistant::compaction_service::compact_conversation(
        &target_pool,
        &session,
        &connection,
        summary_working_dir.as_deref(),
        CompactionTrigger::Manual,
        None,
        compaction::VERBATIM_TAIL_MAX_TOKENS,
        &mut conversation,
    )
    .await?;

    let Some(outcome) = outcome else {
        return Ok(AssistantCompactionResult {
            compaction: None,
            summary_message: None,
        });
    };

    if crate::assistant::providers::is_cli_provider(&connection.protocol_id) {
        reset_cli_session_for_rotation(&target_pool, &mut session).await?;
    }

    emit_event(
        &app,
        &workspace_id,
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
    workspace_id: String,
    session_id: String,
    message_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (target_pool, session) =
        session_in_workspace(state.inner(), &workspace_id, &session_id).await?;
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
        &workspace_id,
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
    workspace_id: String,
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
    let (target_pool, session) =
        session_in_workspace(state.inner(), &workspace_id, &session_id).await?;
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
        &workspace_id,
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
pub async fn assistant_cancel_run(
    workspace_id: String,
    run_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AssistantRun, String> {
    let (target_pool, run) = run_in_workspace(&state, &workspace_id, &run_id).await?;

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
        &workspace_id,
        &session,
        Some(&run_id),
        AssistantUiEvent::RunCancelled {
            run: cancelled.clone(),
        },
    )?;

    Ok(cancelled)
}

pub(crate) fn spawn_run_task(
    deps: AssistantDeps,
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
        let input = RunTurnInput {
            session_id: session_id.clone(),
            run_id: Some(run_id.clone()),
            trigger,
            connection_id,
            cancel_token,
            trigger_message_id,
        };
        if let Err(e) = engine::run_session_turn(&deps, input).await {
            tracing::error!("Assistant engine error for run {}: {}", run_id, e);
        }
        if let Err(e) = start_queued_followup_if_idle(deps, session_id.clone()).await {
            tracing::error!(
                session_id = %session_id,
                error = %e,
                "Failed to start queued assistant follow-up"
            );
        }
    });
}

pub(crate) async fn start_queued_followup_if_idle(
    deps: AssistantDeps,
    session_id: String,
) -> Result<Option<AssistantRun>, String> {
    let AssistantDeps {
        workspace_id,
        pool,
        app,
    } = deps;
    if repository::session_has_active_run(&pool, &session_id).await? {
        return Ok(None);
    }

    let pending = repository::list_pending_queued_messages(&pool, &session_id).await?;
    if pending.is_empty() {
        return Ok(None);
    }

    let session = repository::get_session(&pool, &session_id)
        .await?
        .ok_or_else(|| format!("Assistant session not found: {}", session_id))?;

    let app_state = app.state::<AppState>();

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
        &workspace_id,
        &session,
        Some(&run.id),
        AssistantUiEvent::QueuedMessagesDelivered {
            message_ids: message_ids.clone(),
        },
    );

    emit_event(
        &app,
        &workspace_id,
        &session,
        Some(&run.id),
        AssistantUiEvent::RunQueued { run: run.clone() },
    )?;

    spawn_run_task(
        AssistantDeps {
            workspace_id,
            pool,
            app,
        },
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

    #[tokio::test]
    async fn session_and_run_lookups_read_only_the_named_workspace() {
        let temp = tempfile::tempdir().expect("temp dir");
        let (_a_tmp, a) = crate::db::test_support::workspace_pool().await;
        let (_b_tmp, b) = crate::db::test_support::workspace_pool().await;
        let session = repository::create_session(&b, Default::default())
            .await
            .expect("session");
        let run = repository::create_run(
            &b,
            CreateRunParams {
                session_id: session.id.clone(),
                status: RunStatus::Running,
                trigger: RunTrigger::UserMessage,
                connection_id: "conn".to_string(),
                protocol_id: "claude-code".to_string(),
                model_id: String::new(),
                error: None,
            },
        )
        .await
        .expect("run");
        let mut index = crate::workspace_index::WorkspaceIndex::default();
        index.attach_pool("ws-a".to_string(), a);
        index.attach_pool("ws-b".to_string(), b);
        let config_manager = crate::ConfigManager::new_for_tests(
            Default::default(),
            temp.path().join("config.json"),
        );
        let state = AppState::new_for_tests(config_manager, index).expect("app state");

        let missing = session_in_workspace(&state, "ws-a", &session.id).await;
        assert!(missing.is_err(), "workspace A must not find B's session");
        let (_, found) = session_in_workspace(&state, "ws-b", &session.id)
            .await
            .expect("workspace B owns the session");
        assert_eq!(found.id, session.id);

        let missing = run_in_workspace(&state, "ws-a", &run.id).await;
        assert!(missing.is_err(), "workspace A must not find B's run");
        let (_, found) = run_in_workspace(&state, "ws-b", &run.id)
            .await
            .expect("workspace B owns the run");
        assert_eq!(found.id, run.id);
    }

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

    fn message(created_at: i64) -> AssistantMessage {
        AssistantMessage {
            id: format!("msg-{created_at}"),
            session_id: "sess-1".into(),
            role: crate::assistant::types::MessageRole::User,
            content: Vec::new(),
            created_at,
            provider_metadata: None,
        }
    }

    fn ids(page: &[AssistantMessage]) -> Vec<&str> {
        page.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn build_message_page_marks_has_more_from_the_extra_row() {
        let newest_first = vec![message(5), message(4), message(3), message(2)];
        let (page, has_more, cursor) = build_message_page(newest_first, 3);
        assert!(has_more);
        assert_eq!(ids(&page), ["msg-3", "msg-4", "msg-5"]);
        assert_eq!(
            cursor,
            Some(AssistantMessageCursor {
                session_id: "sess-1".into(),
                created_at: 3,
                message_id: "msg-3".into(),
            })
        );
    }

    #[test]
    fn build_message_page_without_extra_row_is_the_last_page() {
        let newest_first = vec![message(3), message(2), message(1)];
        let (page, has_more, cursor) = build_message_page(newest_first, 3);
        assert!(!has_more);
        assert_eq!(cursor, None);
        assert_eq!(ids(&page), ["msg-1", "msg-2", "msg-3"]);
    }

    #[test]
    fn build_message_page_of_empty_session() {
        let (page, has_more, cursor) = build_message_page(Vec::new(), 3);
        assert!(page.is_empty());
        assert!(!has_more);
        assert_eq!(cursor, None);
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

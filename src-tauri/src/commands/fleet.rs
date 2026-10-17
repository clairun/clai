//! `fleet_snapshot`: the Fleet page's startup state for the most recently
//! active workspaces, in one call. Everything after startup arrives through
//! the existing `assistant://event`, `permissions://*` and `path-grants://*`
//! events, so this is read-only and keeps no state of its own.

use std::cmp::Reverse;

use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::assistant::conversation::content_text;
use crate::assistant::repository;
use crate::assistant::tools::ask_user::{self, PendingAsk};
use crate::assistant::types::{AssistantMessage, MessageRole};
use crate::assistant::workspace_agents::workspace_agent_display_names;
use crate::assistant::workspace_tasks_repo::{self, WorkspaceTaskResponse};
use crate::commands::path_grants::PathGrantRequest;
use crate::commands::permissions::PermissionRequest;
use crate::db::DbPool;
use crate::workspace_index::WorkspaceLocator;
use crate::AppState;

const DEFAULT_WORKSPACE_LIMIT: u32 = 8;
const MAX_WORKSPACE_LIMIT: u32 = 32;
const OPEN_TASK_LIMIT: i64 = 20;
const FINISHED_TASK_LIMIT: i64 = 10;
/// How many of the newest main-session rows to search for assistant text.
const LAST_MESSAGE_SCAN: i64 = 10;
const LAST_MESSAGE_MAX_CHARS: usize = 600;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct FleetWorkspace {
    pub workspace_id: String,
    pub title: String,
    pub starred: bool,
    /// Newest of any session touch and the workspace config's `updated_at`.
    pub last_activity_at: i64,
    /// The manager's conversation; `None` until someone talks to it.
    pub main_session_id: Option<String>,
    /// The main session's queued or running run.
    pub active_run_id: Option<String>,
    /// Unfinished tasks plus the most recently finished ones, newest first.
    pub tasks: Vec<WorkspaceTaskResponse>,
    pub pending_permissions: Vec<PermissionRequest>,
    pub pending_path_grants: Vec<PathGrantRequest>,
    pub pending_asks: Vec<PendingAsk>,
    pub last_assistant_message: Option<FleetMessagePreview>,
}

/// The text of the main session's newest assistant reply, cut to a preview.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct FleetMessagePreview {
    pub message_id: String,
    pub text: String,
    pub truncated: bool,
    pub created_at: i64,
}

/// The `limit` most recently active workspaces (default 8, max 32), most
/// recent first.
#[tauri::command]
pub async fn fleet_snapshot(
    limit: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<FleetWorkspace>, String> {
    snapshot(state.inner(), limit).await
}

async fn snapshot(state: &AppState, limit: Option<u32>) -> Result<Vec<FleetWorkspace>, String> {
    let limit = limit
        .unwrap_or(DEFAULT_WORKSPACE_LIMIT)
        .clamp(1, MAX_WORKSPACE_LIMIT) as usize;
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();

    let mut ranked = Vec::with_capacity(locators.len());
    for locator in locators {
        let pool = state.workspace_db(&locator.id).await?;
        let last_activity_at = repository::last_session_activity(&pool)
            .await?
            .unwrap_or(0)
            .max(locator.updated_at);
        ranked.push((last_activity_at, locator, pool));
    }
    ranked.sort_by_key(|(last_activity_at, ..)| Reverse(*last_activity_at));
    ranked.truncate(limit);

    let mut workspaces = Vec::with_capacity(ranked.len());
    for (last_activity_at, locator, pool) in ranked {
        workspaces.push(workspace_snapshot(state, locator, &pool, last_activity_at).await?);
    }
    Ok(workspaces)
}

async fn workspace_snapshot(
    state: &AppState,
    locator: WorkspaceLocator,
    pool: &DbPool,
    last_activity_at: i64,
) -> Result<FleetWorkspace, String> {
    let main_session = repository::find_manager_session(pool, &locator.default_agent_id).await?;
    let (active_run_id, last_assistant_message) = match &main_session {
        Some(session) => (
            repository::active_run_id(pool, &session.id).await?,
            last_assistant_message(pool, &session.id).await?,
        ),
        None => (None, None),
    };

    let rows = workspace_tasks_repo::list_open_and_recent_tasks(
        pool,
        OPEN_TASK_LIMIT,
        FINISHED_TASK_LIMIT,
    )
    .await?;
    // Agent names come from config.json; skip the read when there is no task.
    let tasks = if rows.is_empty() {
        Vec::new()
    } else {
        let agent_names = workspace_agent_display_names(state, &locator.id);
        rows.into_iter()
            .map(|row| row.into_response(&locator.id, &agent_names))
            .collect()
    };

    Ok(FleetWorkspace {
        pending_permissions: state
            .pending_approvals
            .list_for_workspace(&locator.id)
            .await,
        pending_path_grants: state
            .pending_path_grants
            .list_for_workspace(&locator.id)
            .await,
        pending_asks: ask_user::pending_for_workspace(&locator.id),
        workspace_id: locator.id,
        title: locator.title,
        starred: locator.starred_at > 0,
        last_activity_at,
        main_session_id: main_session.map(|session| session.id),
        active_run_id,
        tasks,
        last_assistant_message,
    })
}

async fn last_assistant_message(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<FleetMessagePreview>, String> {
    let newest_first =
        repository::list_messages_before(pool, session_id, None, LAST_MESSAGE_SCAN).await?;
    Ok(newest_first
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .find_map(preview))
}

/// `None` for a reply with no text (tool calls or thinking only).
fn preview(message: &AssistantMessage) -> Option<FleetMessagePreview> {
    let text = content_text(&message.content);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let truncated = text.chars().count() > LAST_MESSAGE_MAX_CHARS;
    Some(FleetMessagePreview {
        message_id: message.id.clone(),
        text: text.chars().take(LAST_MESSAGE_MAX_CHARS).collect(),
        truncated,
        created_at: message.created_at,
    })
}

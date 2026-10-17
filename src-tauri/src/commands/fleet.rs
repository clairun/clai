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
    /// When any session (main, task or other) was last touched; 0 when the
    /// workspace has no session yet.
    pub last_activity_at: i64,
    /// The manager's conversation; `None` until someone talks to it.
    pub main_session_id: Option<String>,
    /// Queued or running runs in any session of the workspace, not only the
    /// main one; empty when nothing runs.
    pub active_run_ids: Vec<String>,
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

    // A workspace whose database can't be opened or read is skipped, so one
    // broken workspace doesn't blank the whole page.
    let mut ranked = Vec::with_capacity(locators.len());
    for locator in locators {
        let activity = match state.workspace_db(&locator.id).await {
            Ok(pool) => repository::last_session_activity(&pool)
                .await
                .map(|at| (at.unwrap_or(0), pool)),
            Err(e) => Err(e),
        };
        match activity {
            Ok((last_activity_at, pool)) => ranked.push((last_activity_at, locator, pool)),
            Err(e) => {
                tracing::warn!(workspace_id = %locator.id, error = %e, "Fleet skips workspace");
            }
        }
    }
    ranked.sort_by_key(|(last_activity_at, ..)| Reverse(*last_activity_at));
    ranked.truncate(limit);

    let mut workspaces = Vec::with_capacity(ranked.len());
    for (last_activity_at, locator, pool) in ranked {
        let id = locator.id.clone();
        match workspace_snapshot(state, locator, &pool, last_activity_at).await {
            Ok(workspace) => workspaces.push(workspace),
            Err(e) => tracing::warn!(workspace_id = %id, error = %e, "Fleet skips workspace"),
        }
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
    let last_assistant_message = match &main_session {
        Some(session) => last_assistant_message(pool, &session.id).await?,
        None => None,
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
        active_run_ids: repository::workspace_active_run_ids(pool).await?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::{
        AssistantRun, ContentPart, RunStatus, RunTrigger, SessionContext,
    };
    use crate::config::{workspace_config, AppConfig, WorkspaceConfig};
    use crate::workspace_index::WorkspaceIndex;

    const BUSY: &str = "11111111-1111-4111-8111-111111111111";
    const IDLE: &str = "22222222-2222-4222-8222-222222222222";
    const BROKEN: &str = "44444444-4444-4444-8444-444444444444";
    const MANAGER: &str = "33333333-3333-4333-8333-333333333333";

    fn state_with(workspaces: &[&str], temp: &tempfile::TempDir) -> AppState {
        let parent = temp.path().join("workspaces");
        let mut index = WorkspaceIndex::default();
        for (n, id) in workspaces.iter().enumerate() {
            let root = parent.join(id);
            // Config edits aren't activity: IDLE's are the newest of all.
            let edited_at = if *id == IDLE { i64::MAX / 2 } else { 1 };
            let config = WorkspaceConfig::new(
                id.to_string(),
                format!("ws {n}"),
                edited_at,
                MANAGER.to_string(),
            );
            workspace_config::save(&root, &config).expect("seed workspace");
            index.insert_config(root, &config);
        }
        let app_config = AppConfig {
            workspace_dirs: vec![parent],
            ..AppConfig::default()
        };
        let config_manager =
            crate::ConfigManager::new_for_tests(app_config, temp.path().join("config.json"));
        AppState::new_for_tests(config_manager, index).expect("app state")
    }

    async fn insert_task(pool: &DbPool, id: &str, status: &str, updated_at: i64) {
        sqlx::query(
            "INSERT INTO workspace_tasks (id, assigned_to_workspace_agent_id, \
             assigned_agent_definition_id, title, instructions, status, depth, created_at, \
             updated_at) VALUES (?, ?, 'def', ?, '', ?, 0, ?, ?)",
        )
        .bind(id)
        .bind(MANAGER)
        .bind(format!("task {id}"))
        .bind(status)
        .bind(updated_at)
        .bind(updated_at)
        .execute(pool)
        .await
        .expect("insert task");
    }

    async fn assistant_message(pool: &DbPool, session_id: &str, content: Vec<ContentPart>) {
        repository::create_message(
            pool,
            repository::CreateMessageParams {
                session_id: session_id.to_string(),
                role: MessageRole::Assistant,
                content,
                provider_metadata: None,
                run_id: None,
            },
        )
        .await
        .expect("message");
    }

    async fn start_run(pool: &DbPool, session_id: &str, status: RunStatus) -> AssistantRun {
        repository::create_run(
            pool,
            repository::CreateRunParams {
                session_id: session_id.to_string(),
                status,
                trigger: RunTrigger::UserMessage,
                connection_id: "conn".to_string(),
                protocol_id: "proto".to_string(),
                model_id: "model".to_string(),
                error: None,
            },
        )
        .await
        .expect("run")
    }

    #[tokio::test]
    async fn snapshot_ranks_by_session_activity_and_bounds_its_payload() {
        let temp = tempfile::tempdir().expect("temp dir");
        let state = state_with(&[BUSY, IDLE, BROKEN], &temp);
        // A directory where the database file belongs: BROKEN can't open.
        let broken_root = temp.path().join("workspaces").join(BROKEN);
        std::fs::create_dir_all(workspace_config::data_path(&broken_root)).expect("block db");
        let pool = state.workspace_db(BUSY).await.expect("db");

        let session = repository::create_session(
            &pool,
            SessionContext {
                automation_id: Some(MANAGER.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("session");
        let long_reply = "é".repeat(LAST_MESSAGE_MAX_CHARS + 5);
        assistant_message(
            &pool,
            &session.id,
            vec![ContentPart::Text {
                text: format!("  {long_reply}\n"),
            }],
        )
        .await;
        // A newer text-less reply is skipped, not shown as an empty preview.
        assistant_message(&pool, &session.id, Vec::new()).await;
        let run = start_run(&pool, &session.id, RunStatus::Running).await;
        let other_session = repository::create_session(&pool, SessionContext::default())
            .await
            .expect("other session");
        let other_run = start_run(&pool, &other_session.id, RunStatus::Queued).await;
        for n in 0..(OPEN_TASK_LIMIT + 3) {
            insert_task(&pool, &format!("open-{n}"), "running", 100 + n).await;
        }
        for n in 0..(FINISHED_TASK_LIMIT + 3) {
            insert_task(&pool, &format!("done-{n}"), "completed", 200 + n).await;
        }

        let snapshot = snapshot(&state, None).await.expect("snapshot");
        assert_eq!(
            snapshot
                .iter()
                .map(|w| w.workspace_id.as_str())
                .collect::<Vec<_>>(),
            vec![BUSY, IDLE],
            "session activity ranks, config edits don't, and BROKEN is skipped"
        );
        let busy = &snapshot[0];
        assert_eq!(busy.main_session_id.as_deref(), Some(session.id.as_str()));
        let mut run_ids = busy.active_run_ids.clone();
        run_ids.sort();
        let mut expected = vec![run.id.clone(), other_run.id.clone()];
        expected.sort();
        assert_eq!(run_ids, expected, "runs in every session count");
        assert!(busy.last_activity_at >= session.created_at);

        let open = busy
            .tasks
            .iter()
            .filter(|t| !t.status.is_terminal())
            .count();
        assert_eq!(open as i64, OPEN_TASK_LIMIT);
        assert_eq!(
            busy.tasks.len() as i64,
            OPEN_TASK_LIMIT + FINISHED_TASK_LIMIT
        );
        assert_eq!(
            busy.tasks[0].id,
            format!("done-{}", FINISHED_TASK_LIMIT + 2)
        );
        assert_eq!(busy.tasks[0].assigned_agent_display_name, "Manager");

        let preview = busy.last_assistant_message.as_ref().expect("preview");
        assert!(preview.truncated);
        assert_eq!(preview.text.chars().count(), LAST_MESSAGE_MAX_CHARS);
        assert!(preview.text.starts_with('é'));

        let idle = &snapshot[1];
        assert_eq!(idle.last_activity_at, 0);
        assert!(idle.main_session_id.is_none() && idle.active_run_ids.is_empty());
        assert!(idle.tasks.is_empty() && idle.last_assistant_message.is_none());

        let top = super::snapshot(&state, Some(1)).await.expect("snapshot");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].workspace_id, BUSY);
    }
}

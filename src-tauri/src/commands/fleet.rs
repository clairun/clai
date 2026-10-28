//! `fleet_snapshot`: the Fleet page's startup state for the most recently
//! active workspaces, in one call. Everything after startup arrives through
//! the existing `assistant://event`, `permissions://*` and `path-grants://*`
//! events, so this is read-only and keeps no state of its own.

use std::cmp::Reverse;

use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::assistant::repository;
use crate::assistant::tools::ask_user::{self, PendingAsk};
use crate::assistant::types::{AgentRefSnapshot, AssistantMessage, AssistantRun, ContentPart};
use crate::assistant::workspace_agents::{agent_display_names, load_workspace_agent_rows};
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
const RECENT_MESSAGE_LIMIT: i64 = 12;
/// How much of a message's text the snapshot carries; the Fleet fetches the
/// rest (`assistant_get_message`) when it shows the message whole. At least
/// the Fleet's `NOTE_CHARS` (fleetModel.ts), so a seeded preview isn't
/// clipped shorter than a live one.
const PREVIEW_CHARS: usize = 600;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct FleetWorkspace {
    pub workspace_id: String,
    pub title: String,
    /// When any session (main, task or other) was last touched; 0 when the
    /// workspace has no session yet.
    pub last_activity_at: i64,
    /// When the user last opened the workspace; 0 when never opened.
    pub last_opened_at: i64,
    /// The manager's conversation; `None` until someone talks to it.
    pub main_session_id: Option<String>,
    /// The main session's newest run, including its failure when one occurred.
    pub last_main_run: Option<AssistantRun>,
    /// Non-terminal runs (queued, running or waiting for a tool) in any
    /// session of the workspace, not only the main one; empty when nothing
    /// runs.
    pub active_run_ids: Vec<String>,
    /// Unfinished tasks plus the most recently finished ones, newest first.
    pub tasks: Vec<WorkspaceTaskResponse>,
    pub pending_permissions: Vec<PermissionRequest>,
    pub pending_path_grants: Vec<PathGrantRequest>,
    pub pending_asks: Vec<PendingAsk>,
    /// The main session's newest assistant reply with text.
    pub last_assistant_message: Option<FleetHighlight>,
    /// The newest user and assistant messages with text or an image across
    /// every session, oldest first; tool rows and tool-only replies are left
    /// out.
    pub recent_messages: Vec<FleetHighlight>,
    /// The enabled agents, Main first, then teammates in roster order.
    pub agents: Vec<AgentRefSnapshot>,
}

/// A message of the workspace's conversation, cut to a preview, with the
/// run that wrote it.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct FleetHighlight {
    pub message: AssistantMessage,
    pub run_id: Option<String>,
    /// The message's text was cut: the whole message has to be fetched.
    pub truncated: bool,
}

impl FleetHighlight {
    fn new(mut message: AssistantMessage, run_id: Option<String>) -> Self {
        let (content, truncated) = preview(message.content, PREVIEW_CHARS);
        message.content = content;
        Self {
            message,
            run_id,
            truncated,
        }
    }
}

/// The text parts up to `budget` characters, every image part, and whether
/// text was cut. An image part is a reference (its bytes stay on disk), so
/// all are kept, past the cut too. Of the other parts (tool calls, thinking),
/// which the Fleet doesn't show, each run between two kept parts keeps its
/// first, emptied: it still breaks the text where the message did.
fn preview(parts: Vec<ContentPart>, mut budget: usize) -> (Vec<ContentPart>, bool) {
    let mut kept = Vec::new();
    let mut gap = None;
    let mut cut = false;
    for part in parts {
        let part = match part {
            ContentPart::Text { text } if text.is_empty() => continue,
            ContentPart::Text { .. } if cut || budget == 0 => {
                cut = true;
                continue;
            }
            ContentPart::Text { mut text } => {
                if let Some((at, _)) = text.char_indices().nth(budget) {
                    text.truncate(at);
                    cut = true;
                } else {
                    budget -= text.chars().count();
                }
                ContentPart::Text { text }
            }
            image @ ContentPart::Image { .. } => image,
            other => {
                gap.get_or_insert_with(|| emptied(other));
                continue;
            }
        };
        if let Some(gap) = gap.take().filter(|_| !kept.is_empty()) {
            kept.push(gap);
        }
        kept.push(part);
    }
    (kept, cut)
}

fn emptied(part: ContentPart) -> ContentPart {
    match part {
        ContentPart::Thinking { .. } => ContentPart::Thinking {
            text: String::new(),
            signature: None,
        },
        ContentPart::ToolUse {
            tool_call_id,
            tool_name,
            ..
        } => ContentPart::ToolUse {
            tool_call_id,
            tool_name,
            arguments: serde_json::Value::Null,
        },
        ContentPart::ToolResult { tool_call_id, .. } => ContentPart::ToolResult {
            tool_call_id,
            payload: serde_json::Value::Null,
            started_at: None,
            completed_at: None,
        },
        other => other,
    }
}

/// The `limit` most recently active workspaces (default 8, max 32), most
/// recent first, followed by any older workspace with a pending approval,
/// path grant or question, or that is `keep` (the Fleet's selected one).
#[tauri::command]
pub async fn fleet_snapshot(
    limit: Option<u32>,
    keep: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<FleetWorkspace>, String> {
    snapshot(state.inner(), limit, keep.as_deref()).await
}

async fn snapshot(
    state: &AppState,
    limit: Option<u32>,
    keep: Option<&str>,
) -> Result<Vec<FleetWorkspace>, String> {
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
        let activity = async {
            let pool = state.workspace_db(&locator.id).await?;
            let at = repository::last_session_activity(&pool).await?;
            Ok::<_, String>((at.unwrap_or(0), pool))
        }
        .await;
        match activity {
            Ok((last_activity_at, pool)) => ranked.push((last_activity_at, locator, pool)),
            Err(e) => {
                tracing::warn!(workspace_id = %locator.id, error = %e, "Fleet skips workspace");
            }
        }
    }
    ranked.sort_by_key(|(last_activity_at, ..)| Reverse(*last_activity_at));
    // A workspace waiting on the user stays past `limit`: waiting doesn't
    // bump its activity, and the Fleet mutes the toasts that would otherwise
    // surface the request. So does the selected one, however old.
    let approvals = state.pending_approvals.counts_snapshot().await;
    let path_grants = state.pending_path_grants.counts_snapshot().await;
    let waits = |id: &str| {
        approvals.contains_key(id)
            || path_grants.contains_key(id)
            || !ask_user::pending_for_workspace(id).is_empty()
    };
    let older = ranked.split_off(limit.min(ranked.len()));
    ranked.extend(
        older
            .into_iter()
            .filter(|(_, locator, _)| keep == Some(locator.id.as_str()) || waits(&locator.id)),
    );

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
    let last_main_run = match &main_session {
        Some(session) => repository::latest_run(pool, &session.id).await?,
        None => None,
    };
    let last_assistant_message = match &main_session {
        Some(session) => repository::last_completed_assistant_text(pool, &session.id)
            .await?
            .map(|(message, run_id)| FleetHighlight::new(message, run_id)),
        None => None,
    };
    let recent_messages = repository::recent_conversation_messages(pool, RECENT_MESSAGE_LIMIT)
        .await?
        .into_iter()
        .map(|(message, run_id)| FleetHighlight::new(message, run_id))
        .collect();

    let rows = workspace_tasks_repo::list_open_and_recent_tasks(
        pool,
        OPEN_TASK_LIMIT,
        FINISHED_TASK_LIMIT,
    )
    .await?;
    let roster = load_workspace_agent_rows(state, &locator.id).unwrap_or_default();
    let agent_names = agent_display_names(&roster);
    let tasks = rows
        .into_iter()
        .map(|row| row.into_response(&locator.id, &agent_names))
        .collect();
    let agents = roster
        .iter()
        .filter(|agent| agent.enabled)
        .map(AgentRefSnapshot::from)
        .collect();

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
        last_activity_at,
        last_opened_at: locator.last_opened_at,
        main_session_id: main_session.map(|session| session.id),
        last_main_run,
        active_run_ids: repository::workspace_active_run_ids(pool).await?,
        tasks,
        last_assistant_message,
        recent_messages,
        agents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::{
        AssistantRun, ContentPart, MessageOrigin, MessageRole, RunStatus, RunTrigger,
        SessionContext,
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
            let mut config = WorkspaceConfig::new(
                id.to_string(),
                format!("ws {n}"),
                edited_at,
                MANAGER.to_string(),
            );
            if *id == BUSY {
                config.last_opened_at = 42;
            }
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

    fn only_text(highlight: &FleetHighlight) -> &str {
        match &highlight.message.content[..] {
            [ContentPart::Text { text }] => text,
            other => panic!("expected one text part, got {other:?}"),
        }
    }

    async fn assistant_message(pool: &DbPool, session_id: &str, content: Vec<ContentPart>) {
        message(pool, session_id, MessageRole::Assistant, content, None).await;
    }

    async fn message(
        pool: &DbPool,
        session_id: &str,
        role: MessageRole,
        content: Vec<ContentPart>,
        run_id: Option<String>,
    ) {
        repository::create_message(
            pool,
            repository::CreateMessageParams {
                session_id: session_id.to_string(),
                role,
                content,
                provider_metadata: None,
                run_id,
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
        let long_reply = "é".repeat(1000);
        assistant_message(
            &pool,
            &session.id,
            vec![
                ContentPart::Text {
                    text: long_reply.clone(),
                },
                ContentPart::ToolUse {
                    tool_call_id: "c".into(),
                    tool_name: "bash_exec".into(),
                    arguments: serde_json::json!({ "command": "ls", "stdin": long_reply }),
                },
            ],
        )
        .await;
        // A newer text-less reply is skipped, not shown as an empty message.
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

        let snapshot = snapshot(&state, None, None).await.expect("snapshot");
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
        assert_eq!(
            busy.last_main_run.as_ref().map(|run| &run.id),
            Some(&run.id)
        );
        let mut run_ids = busy.active_run_ids.clone();
        run_ids.sort();
        let mut expected = vec![run.id.clone(), other_run.id.clone()];
        expected.sort();
        assert_eq!(run_ids, expected, "runs in every session count");
        assert!(busy.last_activity_at >= session.created_at);
        assert_eq!(busy.last_opened_at, 42);

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

        // A long reply travels as its first PREVIEW_CHARS characters, flagged.
        let last = &busy.last_assistant_message.as_ref().expect("last reply");
        assert_eq!(only_text(last), "é".repeat(PREVIEW_CHARS));
        assert!(last.truncated);

        let idle = &snapshot[1];
        assert_eq!((idle.last_activity_at, idle.last_opened_at), (0, 0));
        assert!(idle.main_session_id.is_none() && idle.active_run_ids.is_empty());
        assert!(idle.tasks.is_empty() && idle.last_assistant_message.is_none());

        let top = super::snapshot(&state, Some(1), None)
            .await
            .expect("snapshot");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].workspace_id, BUSY);
    }

    #[test]
    fn preview_keeps_text_and_one_emptied_break_per_run_of_other_parts() {
        let text = |text: &str| ContentPart::Text { text: text.into() };
        let call = |id: &str| ContentPart::ToolUse {
            tool_call_id: id.into(),
            tool_name: "bash_exec".into(),
            arguments: serde_json::json!({ "command": "x".repeat(300) }),
        };
        let thinking = ContentPart::Thinking {
            text: "hmm".repeat(1000),
            signature: Some("sig".into()),
        };
        let parts = vec![
            thinking.clone(),
            call("c0"),
            text("Before."),
            text(""),
            call("c1"),
            thinking,
            call("c2"),
            text("After."),
            call("c3"),
        ];
        // ContentPart has no PartialEq: compare as sent.
        let sent = |(parts, cut): (Vec<ContentPart>, bool)| {
            (serde_json::to_value(parts).expect("parts"), cut)
        };
        let empty_call = ContentPart::ToolUse {
            tool_call_id: "c1".into(),
            tool_name: "bash_exec".into(),
            arguments: serde_json::Value::Null,
        };
        assert_eq!(
            sent(preview(parts.clone(), PREVIEW_CHARS)),
            sent((
                vec![text("Before."), empty_call.clone(), text("After.")],
                false
            ))
        );
        assert_eq!(
            sent(preview(parts.clone(), "Before.".len() + 2)),
            sent((vec![text("Before."), empty_call, text("Af")], true))
        );
        assert_eq!(
            sent(preview(parts, "Before.".len())),
            sent((vec![text("Before.")], true))
        );
    }

    #[tokio::test]
    async fn snapshot_restores_main_run_failure() {
        let temp = tempfile::tempdir().expect("temp dir");
        let state = state_with(&[BUSY], &temp);
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
        let run = start_run(&pool, &session.id, RunStatus::Running).await;
        repository::update_run_status(
            &pool,
            &run.id,
            RunStatus::Failed,
            Some("Usage limit reached"),
        )
        .await
        .expect("fail run");

        let workspaces = snapshot(&state, None, None).await.expect("snapshot");
        let last = workspaces[0].last_main_run.as_ref().expect("last main run");
        assert_eq!(last.id, run.id);
        assert_eq!(last.status, RunStatus::Failed);
        assert_eq!(last.error.as_deref(), Some("Usage limit reached"));
    }

    /// A CLI run stores one assistant row, then a row per tool result: the
    /// reply's text sits behind however many tools the run called.
    #[tokio::test]
    async fn last_message_is_found_past_tool_rows_and_skips_runs_in_flight() {
        let temp = tempfile::tempdir().expect("temp dir");
        let state = state_with(&[BUSY], &temp);
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
        let text = |text: &str| {
            vec![ContentPart::Text {
                text: text.to_string(),
            }]
        };
        let done = start_run(&pool, &session.id, RunStatus::Completed).await;
        message(
            &pool,
            &session.id,
            MessageRole::Assistant,
            text("Done."),
            Some(done.id.clone()),
        )
        .await;
        for _ in 0..30 {
            message(&pool, &session.id, MessageRole::Tool, Vec::new(), None).await;
        }
        let run = start_run(&pool, &session.id, RunStatus::Running).await;
        let in_flight = Some(run.id.clone());
        message(
            &pool,
            &session.id,
            MessageRole::Assistant,
            text("Half a sen"),
            in_flight,
        )
        .await;

        let snapshot = snapshot(&state, None, None).await.expect("snapshot");
        let last = snapshot[0]
            .last_assistant_message
            .as_ref()
            .expect("last reply");
        assert_eq!(only_text(last), "Done.");
        assert_eq!(last.run_id, Some(done.id));
    }

    /// A state where BUSY has session activity and `waiting` has none, so
    /// `limit = 1` keeps only BUSY unless `waiting` waits on the user.
    async fn busy_and(waiting: &str, temp: &tempfile::TempDir) -> AppState {
        let state = state_with(&[BUSY, waiting], temp);
        let pool = state.workspace_db(BUSY).await.expect("db");
        repository::create_session(&pool, SessionContext::default())
            .await
            .expect("session");
        state
    }

    async fn ids_at_limit_one(state: &AppState) -> Vec<String> {
        let top = snapshot(state, Some(1), None).await.expect("snapshot");
        top.into_iter().map(|w| w.workspace_id).collect()
    }

    // Each test waits on its own workspace: pending questions live in a
    // process-wide map shared by parallel tests.
    #[tokio::test]
    async fn pending_approval_keeps_workspace_past_limit() {
        const WAITING: &str = "55555555-5555-4555-8555-555555555555";
        let temp = tempfile::tempdir().expect("temp dir");
        let state = busy_and(WAITING, &temp).await;
        let approval = PermissionRequest {
            persists_to_shared_agent: false,
            also_affects_workspaces: Vec::new(),
            request_id: "approval".to_string(),
            workspace_id: Some(WAITING.to_string()),
            agent_id: None,
            agent_name: None,
            command: "cmd".to_string(),
            segments: Vec::new(),
        };
        let _waiting = state
            .pending_approvals
            .register(approval, "run".to_string())
            .await;
        assert_eq!(ids_at_limit_one(&state).await, [BUSY, WAITING]);
    }

    #[tokio::test]
    async fn pending_question_keeps_workspace_past_limit() {
        const WAITING: &str = "66666666-6666-4666-8666-666666666666";
        let temp = tempfile::tempdir().expect("temp dir");
        let state = busy_and(WAITING, &temp).await;
        let _ask = ask_user::register_for_test(PendingAsk {
            pending_id: "fleet-ask".to_string(),
            workspace_id: WAITING.to_string(),
            session_id: "session".to_string(),
            question: "?".to_string(),
            options: None,
            multi_select: false,
            extra_context: None,
            requested_at: 0,
        });
        assert_eq!(ids_at_limit_one(&state).await, [BUSY, WAITING]);
    }

    #[tokio::test]
    async fn pending_path_grant_keeps_workspace_past_limit() {
        const WAITING: &str = "77777777-7777-4777-8777-777777777777";
        let temp = tempfile::tempdir().expect("temp dir");
        let state = busy_and(WAITING, &temp).await;
        let grant = PathGrantRequest {
            request_id: "grant".to_string(),
            workspace_id: Some(WAITING.to_string()),
            agent_id: None,
            agent_name: None,
            requested_path: "/tmp".to_string(),
            requested_access: crate::config::FilesystemPathAccess::ReadOnly,
            reason: String::new(),
        };
        let _waiting = state
            .pending_path_grants
            .register(grant, "run".to_string())
            .await;
        let top = snapshot(&state, Some(1), None).await.expect("snapshot");
        assert_eq!(top.len(), 2);
        assert_eq!(top[1].workspace_id, WAITING);
        assert_eq!(top[1].pending_path_grants.len(), 1);
    }

    #[tokio::test]
    async fn kept_workspace_stays_past_limit() {
        const KEPT: &str = "88888888-8888-4888-8888-888888888888";
        let temp = tempfile::tempdir().expect("temp dir");
        let state = busy_and(KEPT, &temp).await;
        assert_eq!(ids_at_limit_one(&state).await, [BUSY]);
        let top = snapshot(&state, Some(1), Some(KEPT))
            .await
            .expect("snapshot");
        let ids: Vec<_> = top.into_iter().map(|w| w.workspace_id).collect();
        assert_eq!(ids, [BUSY, KEPT]);
    }

    /// A row with an explicit time and origin; `create_message` stamps now.
    async fn row_at(
        pool: &DbPool,
        session_id: &str,
        role: &str,
        content: Vec<ContentPart>,
        origin: &MessageOrigin,
        (created_at, run_id): (i64, Option<&str>),
    ) {
        sqlx::query(
            "INSERT INTO assistant_messages \
             (id, session_id, role, content_json, origin_json, run_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(session_id)
        .bind(role)
        .bind(serde_json::to_string(&content).expect("content"))
        .bind(serde_json::to_string(origin).expect("origin"))
        .bind(run_id)
        .bind(created_at)
        .execute(pool)
        .await
        .expect("insert message");
    }

    #[tokio::test]
    async fn recent_messages_span_sessions_and_skip_tools_and_runs_in_flight() {
        let temp = tempfile::tempdir().expect("temp dir");
        let state = state_with(&[BUSY], &temp);
        let pool = state.workspace_db(BUSY).await.expect("db");
        let main = repository::create_session(
            &pool,
            SessionContext {
                automation_id: Some(MANAGER.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("session");
        let task = repository::create_session(&pool, SessionContext::default())
            .await
            .expect("task session");
        let text = |text: &str| {
            vec![ContentPart::Text {
                text: text.to_string(),
            }]
        };
        let user = MessageOrigin::User;
        let manager = MessageOrigin::Agent {
            agent: Box::new(crate::assistant::types::AgentRefSnapshot {
                workspace_agent_id: MANAGER.to_string(),
                agent_definition_id: None,
                display_name: "Manager".to_string(),
                is_default: true,
                avatar: None,
            }),
            session_id: Some(main.id.clone()),
            run_id: None,
            reply_address: None,
        };
        for n in 0..RECENT_MESSAGE_LIMIT {
            row_at(
                &pool,
                &main.id,
                "user",
                text(&format!("old {n}")),
                &user,
                (n, None),
            )
            .await;
        }
        let tool_call = vec![ContentPart::ToolUse {
            tool_call_id: "call".to_string(),
            tool_name: "bash".to_string(),
            arguments: serde_json::Value::Null,
        }];
        let run = start_run(&pool, &task.id, RunStatus::Running).await;
        let rows = [
            (&main.id, "user", text("hello"), &user),
            (&task.id, "user", text("do X"), &manager),
            (&task.id, "assistant", tool_call, &user),
            (&task.id, "tool", text("tool output"), &user),
            (&main.id, "assistant", text("hi there"), &user),
        ];
        for (n, (session, role, content, origin)) in rows.into_iter().enumerate() {
            row_at(
                &pool,
                session,
                role,
                content,
                origin,
                (100 + n as i64, None),
            )
            .await;
        }
        let in_flight = (200, Some(run.id.as_str()));
        row_at(
            &pool,
            &task.id,
            "assistant",
            text("Half a sen"),
            &user,
            in_flight,
        )
        .await;

        let snapshot = snapshot(&state, None, None).await.expect("snapshot");
        let recent = &snapshot[0].recent_messages;
        let texts: Vec<_> = recent.iter().map(only_text).collect();
        let kept_old = RECENT_MESSAGE_LIMIT as usize - 3;
        assert_eq!(texts.len(), RECENT_MESSAGE_LIMIT as usize);
        assert_eq!(
            texts[0],
            format!("old {}", RECENT_MESSAGE_LIMIT as usize - kept_old)
        );
        assert_eq!(texts[kept_old..], ["hello", "do X", "hi there"]);
        let delegated = &recent[kept_old + 1].message;
        assert_eq!(delegated.session_id, task.id);
        assert_eq!(delegated.origin, manager);
        assert_eq!(recent[kept_old + 2].message.role, MessageRole::Assistant);
    }

    fn image(id: &str) -> ContentPart {
        ContentPart::Image {
            id: id.to_string(),
            path: format!(".clai/images/{id}.png"),
            media_type: "image/png".to_string(),
            filename: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn preview_keeps_every_image_in_place_past_the_cut() {
        let text = |text: &str| ContentPart::Text { text: text.into() };
        let parts = vec![
            image("a"),
            text("Look at this."),
            image("b"),
            text("More."),
            image("c"),
        ];
        let sent = |(parts, cut): (Vec<ContentPart>, bool)| {
            (serde_json::to_value(parts).expect("parts"), cut)
        };
        assert_eq!(
            sent(preview(parts.clone(), PREVIEW_CHARS)),
            sent((parts.clone(), false))
        );
        assert_eq!(
            sent(preview(parts, "Look".len())),
            sent((vec![image("a"), text("Look"), image("b"), image("c")], true))
        );
    }

    #[tokio::test]
    async fn recent_messages_keep_an_image_only_message_and_its_references() {
        let temp = tempfile::tempdir().expect("temp dir");
        let state = state_with(&[BUSY], &temp);
        let pool = state.workspace_db(BUSY).await.expect("db");
        let main = repository::create_session(
            &pool,
            SessionContext {
                automation_id: Some(MANAGER.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("session");
        let shot = vec![image("shot")];
        row_at(
            &pool,
            &main.id,
            "user",
            shot.clone(),
            &MessageOrigin::User,
            (100, None),
        )
        .await;

        let snapshot = snapshot(&state, None, None).await.expect("snapshot");
        let recent = &snapshot[0].recent_messages;
        assert_eq!(recent.len(), 1);
        assert_eq!(
            serde_json::to_value(&recent[0].message.content).expect("content"),
            serde_json::to_value(&shot).expect("shot")
        );
        assert!(!recent[0].truncated);
    }

    #[tokio::test]
    async fn agents_list_enabled_faces_main_first() {
        use crate::config::global_agents::AgentDefinition;
        use crate::config::workspace_config::{
            AgentAvatarRef, WorkspaceAgent, WorkspaceAssignment,
        };

        let temp = tempfile::tempdir().expect("temp dir");
        let state = state_with(&[IDLE], &temp);
        let face = AgentAvatarRef {
            seed: "reviewer-face".to_string(),
            generator_version: 1,
        };
        let definition = |id: &str, avatar: Option<AgentAvatarRef>| {
            let mut behavior = WorkspaceAgent::new_manager(id.to_string(), 1);
            behavior.name = id.to_string();
            behavior.avatar = avatar;
            AgentDefinition {
                id: id.to_string(),
                revision: 1,
                archived: false,
                behavior,
            }
        };
        state
            .config_manager
            .lock()
            .expect("config lock")
            .update(|config| {
                config
                    .agent_definitions
                    .push(definition("Reviewer", Some(face.clone())));
                config.agent_definitions.push(definition("Writer", None));
            })
            .expect("seed definitions");
        // The disabled one joined first: only `enabled` keeps it off the strip.
        let assignment = |id: &str, definition: &str, enabled, created_at| WorkspaceAssignment {
            id: id.to_string(),
            agent_definition_id: definition.to_string(),
            enabled,
            context: String::new(),
            filesystem_grants: Vec::new(),
            created_at,
            updated_at: created_at,
        };
        state
            .update_workspace_config(IDLE, |config| {
                config
                    .assignments
                    .push(assignment("wa-writer", "Writer", false, 2));
                config
                    .assignments
                    .push(assignment("wa-reviewer", "Reviewer", true, 3));
                Ok(())
            })
            .expect("seed assignments");

        let snapshot = snapshot(&state, None, None).await.expect("snapshot");

        let agents = &snapshot[0].agents;
        assert_eq!(
            agents
                .iter()
                .map(|a| (a.workspace_agent_id.as_str(), a.is_default))
                .collect::<Vec<_>>(),
            vec![(MANAGER, true), ("wa-reviewer", false)]
        );
        assert_eq!(agents[1].display_name, "Reviewer");
        assert_eq!(agents[1].agent_definition_id.as_deref(), Some("Reviewer"));
        assert_eq!(
            agents[1].avatar,
            Some(face),
            "the face comes from the roster"
        );
    }
}

//! The `workspace_tasks` table: one row shape, one column list, and the one
//! response every reader (the UI and the agents' task tools) gets.

use crate::assistant::repository::now_ms;
use crate::assistant::signals;
use crate::assistant::types::{AssistantMessage, AssistantRun};
use crate::db::{enum_text, parse_enum_text, DbPool};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};
use std::collections::HashMap;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum TaskStatus {
    Queued,
    Running,
    Stopping,
    #[serde(alias = "input_required")]
    Stopped,
    Completed,
    Failed,
}

impl TaskStatus {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Completed | Self::Failed)
    }
}

/// Why a `failed` task failed; set exactly when the status is `failed`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum FailureReason {
    /// The worker answered `BLOCKED:`.
    Blocked,
    Error,
    Restart,
    ConnectionUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceTaskResponse {
    pub id: String,
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_workspace_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_display_name: Option<String>,
    pub assigned_to_workspace_agent_id: String,
    pub assigned_agent_definition_id: String,
    pub assigned_agent_display_name: String,
    pub title: String,
    pub instructions: String,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<FailureReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    pub depth: i64,
    pub cycle_no: i64,
    /// Set when a stop was requested for the current cycle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_requested_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
}

/// What a message sent to a task did.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceTaskSendResponse {
    pub task: WorkspaceTaskResponse,
    pub message: AssistantMessage,
    /// It waits for the task's current run; `false` when a new cycle starts.
    pub queued: bool,
    /// The run that took it, when one was started or is active.
    pub run: Option<AssistantRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceTaskRow {
    pub(crate) id: String,
    pub(crate) created_by_workspace_agent_id: Option<String>,
    pub(crate) assigned_to_workspace_agent_id: String,
    pub(crate) assigned_agent_definition_id: String,
    pub(crate) title: String,
    pub(crate) instructions: String,
    pub(crate) status: TaskStatus,
    pub(crate) failure_reason: Option<FailureReason>,
    pub(crate) result_summary: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) parent_run_id: Option<String>,
    pub(crate) parent_task_id: Option<String>,
    pub(crate) depth: i64,
    pub(crate) cycle_no: i64,
    pub(crate) stop_requested_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) completed_at: Option<i64>,
}

const TASK_COLUMNS: &str = "id, created_by_workspace_agent_id, assigned_to_workspace_agent_id, \
     assigned_agent_definition_id, title, instructions, status, failure_reason, result_summary, \
     error, session_id, run_id, parent_session_id, parent_run_id, parent_task_id, depth, \
     cycle_no, stop_requested_at, created_at, updated_at, completed_at";

impl WorkspaceTaskRow {
    fn from_sql(row: &sqlx::sqlite::SqliteRow) -> Result<Self, String> {
        Ok(Self {
            id: row.get("id"),
            created_by_workspace_agent_id: row.get("created_by_workspace_agent_id"),
            assigned_to_workspace_agent_id: row.get("assigned_to_workspace_agent_id"),
            assigned_agent_definition_id: row.get("assigned_agent_definition_id"),
            title: row.get("title"),
            instructions: row.get("instructions"),
            status: parse_enum_text(row.get::<String, _>("status"), "task status")?,
            failure_reason: row
                .get::<Option<String>, _>("failure_reason")
                .map(|reason| parse_enum_text(reason, "task failure reason"))
                .transpose()?,
            result_summary: row.get("result_summary"),
            error: row.get("error"),
            session_id: row.get("session_id"),
            run_id: row.get("run_id"),
            parent_session_id: row.get("parent_session_id"),
            parent_run_id: row.get("parent_run_id"),
            parent_task_id: row.get("parent_task_id"),
            depth: row.get("depth"),
            cycle_no: row.get("cycle_no"),
            stop_requested_at: row.get("stop_requested_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            completed_at: row.get("completed_at"),
        })
    }

    /// `agent_names` maps workspace-agent id → display name for the current
    /// roster. An assignee removed from the roster falls back to its definition
    /// id so the task still renders with something identifying; a removed
    /// creator has nothing to fall back to and gets no name.
    pub(crate) fn into_response(
        self,
        workspace_id: &str,
        agent_names: &HashMap<String, String>,
    ) -> WorkspaceTaskResponse {
        let assigned_agent_display_name = agent_names
            .get(&self.assigned_to_workspace_agent_id)
            .cloned()
            .unwrap_or_else(|| self.assigned_agent_definition_id.clone());
        let created_by_display_name = self
            .created_by_workspace_agent_id
            .as_ref()
            .and_then(|id| agent_names.get(id).cloned());

        WorkspaceTaskResponse {
            id: self.id,
            workspace_id: workspace_id.to_string(),
            created_by_workspace_agent_id: self.created_by_workspace_agent_id,
            created_by_display_name,
            assigned_to_workspace_agent_id: self.assigned_to_workspace_agent_id,
            assigned_agent_definition_id: self.assigned_agent_definition_id,
            assigned_agent_display_name,
            title: self.title,
            instructions: self.instructions,
            status: self.status,
            failure_reason: self.failure_reason,
            result_summary: self.result_summary,
            error: self.error,
            session_id: self.session_id,
            run_id: self.run_id,
            parent_session_id: self.parent_session_id,
            parent_run_id: self.parent_run_id,
            parent_task_id: self.parent_task_id,
            depth: self.depth,
            cycle_no: self.cycle_no,
            stop_requested_at: self.stop_requested_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            completed_at: self.completed_at,
        }
    }
}

pub(crate) async fn load_task(
    conn: impl sqlx::SqliteExecutor<'_>,
    task_id: &str,
) -> Result<Option<WorkspaceTaskRow>, String> {
    let row = sqlx::query(&format!(
        "SELECT {TASK_COLUMNS} FROM workspace_tasks WHERE id = ?"
    ))
    .bind(task_id)
    .fetch_optional(conn)
    .await
    .map_err(|e| format!("Failed to load workspace task: {}", e))?;
    row.as_ref().map(WorkspaceTaskRow::from_sql).transpose()
}

/// The task that owns `session_id`, if the session is a task session.
pub(crate) async fn load_task_for_session(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<WorkspaceTaskRow>, String> {
    let row = sqlx::query(&format!(
        "SELECT {TASK_COLUMNS} FROM workspace_tasks WHERE session_id = ?"
    ))
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load workspace task for session: {}", e))?;
    row.as_ref().map(WorkspaceTaskRow::from_sql).transpose()
}

/// The most recently touched tasks, newest first.
pub(crate) async fn list_recent_tasks(
    pool: &DbPool,
    limit: i64,
) -> Result<Vec<WorkspaceTaskRow>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {TASK_COLUMNS} FROM workspace_tasks \
         ORDER BY updated_at DESC, created_at DESC LIMIT ?"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load workspace tasks: {}", e))?;
    rows.iter().map(WorkspaceTaskRow::from_sql).collect()
}

/// Where a task was assigned from: the caller's session and run, and the
/// task that session belongs to, if any.
#[derive(Debug, Clone, Default)]
pub(crate) struct TaskLineage {
    pub(crate) parent_session_id: Option<String>,
    pub(crate) parent_run_id: Option<String>,
    pub(crate) parent_task_id: Option<String>,
    pub(crate) depth: i64,
}

pub(crate) struct NewTask<'a> {
    pub(crate) id: &'a str,
    pub(crate) created_by_workspace_agent_id: Option<&'a str>,
    pub(crate) assigned_to_workspace_agent_id: &'a str,
    pub(crate) assigned_agent_definition_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) instructions: &'a str,
    pub(crate) lineage: TaskLineage,
}

/// Tasks nest at most this deep: a root task has depth 0.
pub(crate) const MAX_TASK_DEPTH: i64 = 4;

/// Inserts a `queued` task, or with `error` one that failed for want of a
/// provider connection, unless it would break a delegation limit.
pub(crate) async fn create_task_row(
    conn: &mut SqliteConnection,
    task: &NewTask<'_>,
    error: Option<&str>,
) -> Result<(), String> {
    ensure_within_limits(conn, task).await?;
    let now = now_ms();
    let failed = error.is_some();
    sqlx::query(
        r#"
        INSERT INTO workspace_tasks (
            id, created_by_workspace_agent_id, assigned_to_workspace_agent_id,
            assigned_agent_definition_id, title, instructions, status, failure_reason, error,
            parent_session_id, parent_run_id, parent_task_id, depth, created_at, updated_at,
            completed_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(task.id)
    .bind(task.created_by_workspace_agent_id)
    .bind(task.assigned_to_workspace_agent_id)
    .bind(task.assigned_agent_definition_id)
    .bind(task.title)
    .bind(task.instructions)
    .bind(if failed { "failed" } else { "queued" })
    .bind(failed.then_some("connection_unavailable"))
    .bind(error)
    .bind(&task.lineage.parent_session_id)
    .bind(&task.lineage.parent_run_id)
    .bind(&task.lineage.parent_task_id)
    .bind(task.lineage.depth)
    .bind(now)
    .bind(now)
    .bind(failed.then_some(now))
    .execute(conn)
    .await
    .map_err(|e| format!("Failed to create workspace task: {}", e))?;
    Ok(())
}

async fn ensure_within_limits(
    conn: &mut SqliteConnection,
    task: &NewTask<'_>,
) -> Result<(), String> {
    if task.lineage.depth >= MAX_TASK_DEPTH {
        return Err(format!(
            "depth_exceeded: tasks nest at most {MAX_TASK_DEPTH} levels deep; do this work yourself."
        ));
    }
    if let Some(creator) = task.created_by_workspace_agent_id {
        signals::ensure_sender_budget(conn, creator).await?;
    }
    Ok(())
}

/// The lineage of a task assigned from `session_id`/`run_id`.
pub(crate) async fn lineage_for_caller(
    pool: &DbPool,
    session_id: &str,
    run_id: &str,
) -> Result<TaskLineage, String> {
    let parent = load_task_for_session(pool, session_id).await?;
    Ok(TaskLineage {
        parent_session_id: Some(session_id.to_string()),
        parent_run_id: Some(run_id.to_string()),
        depth: parent.as_ref().map_or(0, |task| task.depth + 1),
        parent_task_id: parent.map(|task| task.id),
    })
}

/// Opens the next cycle of the task owning `session_id` for `run_id`, the
/// run the dispatcher just claimed its input for. Only a finished task opens
/// one: a new task, a completed or failed one, or a stopped one that `run_id`
/// claimed a message for that was sent after the stop. A task parked by
/// [`park_for_subtasks`] continues its cycle instead. `None` when the
/// session is not a task session or the task is not finished. Call inside
/// the claim transaction, after the claim.
pub(crate) async fn open_cycle(
    conn: &mut SqliteConnection,
    session_id: &str,
    run_id: &str,
) -> Result<Option<WorkspaceTaskRow>, String> {
    let row = sqlx::query(&format!(
        r#"
        UPDATE workspace_tasks
        SET status = 'queued', run_id = ?,
            cycle_no = cycle_no + (status NOT IN ('queued', 'running') OR cycle_no = 0),
            failure_reason = NULL, result_summary = NULL, error = NULL, completed_at = NULL,
            stop_requested_at = NULL,
            updated_at = ?
        WHERE session_id = ?
          AND (status IN ('completed', 'failed')
               OR (status IN ('queued', 'running') AND run_id IS NULL)
               OR (status = 'stopped' AND EXISTS (
                   SELECT 1 FROM session_signals s
                   WHERE s.claimed_run_id = ? AND s.kind IN ('user_message', 'agent_message')
                     AND s.created_at > COALESCE(workspace_tasks.stop_requested_at, workspace_tasks.updated_at))))
        RETURNING {TASK_COLUMNS}
        "#
    ))
    .bind(run_id)
    .bind(now_ms())
    .bind(session_id)
    .bind(run_id)
    .fetch_optional(conn)
    .await
    .map_err(|e| format!("Failed to open task cycle: {}", e))?;
    row.as_ref().map(WorkspaceTaskRow::from_sql).transpose()
}

/// Keeps the cycle of `run_id`, whose run just ended, open while the task
/// still waits on its own subtasks: one is unfinished, or its event has not
/// been delivered here yet. The task stays `running` with no run until that
/// event wakes it and [`open_cycle`] continues the cycle, so its parent gets
/// one result, after the subtasks. Returns the parked task; `None` when there
/// is nothing to wait for or the cycle is not running.
pub(crate) async fn park_for_subtasks(
    conn: &mut SqliteConnection,
    task_id: &str,
    run_id: &str,
) -> Result<Option<WorkspaceTaskRow>, String> {
    let row = sqlx::query(&format!(
        r#"
        UPDATE workspace_tasks SET run_id = NULL, updated_at = ?
        WHERE id = ? AND run_id = ? AND status = 'running'
          AND (EXISTS (SELECT 1 FROM workspace_tasks AS child
                       WHERE child.parent_session_id = workspace_tasks.session_id
                         AND child.status IN ('queued', 'running', 'stopping'))
               OR EXISTS (SELECT 1 FROM session_signals AS s
                          WHERE s.target_session_id = workspace_tasks.session_id
                            AND s.status = 'pending'
                            AND s.kind IN ('task_result', 'task_failed')))
        RETURNING {TASK_COLUMNS}
        "#
    ))
    .bind(now_ms())
    .bind(task_id)
    .bind(run_id)
    .fetch_optional(conn)
    .await
    .map_err(|e| format!("Failed to park workspace task cycle: {}", e))?;
    row.as_ref().map(WorkspaceTaskRow::from_sql).transpose()
}

/// `queued → running` for the cycle of `run_id`.
pub(crate) async fn mark_running(
    pool: &DbPool,
    task_id: &str,
    run_id: &str,
) -> Result<Option<WorkspaceTaskRow>, String> {
    let row = sqlx::query(&format!(
        r#"
        UPDATE workspace_tasks SET status = 'running', updated_at = ?
        WHERE id = ? AND run_id = ? AND status = 'queued'
        RETURNING {TASK_COLUMNS}
        "#
    ))
    .bind(now_ms())
    .bind(task_id)
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to mark workspace task running: {}", e))?;
    row.as_ref().map(WorkspaceTaskRow::from_sql).transpose()
}

/// The terminal state of one cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CycleOutcome {
    pub(crate) status: TaskStatus,
    pub(crate) failure_reason: Option<FailureReason>,
    pub(crate) error: Option<String>,
    pub(crate) result_summary: Option<String>,
}

/// Ends the cycle of `run_id` (`None`: a cycle that never got a run) with
/// `outcome`, or as `stopped` when a stop was requested, however the run
/// ended. `None`, writing nothing, when that cycle has already ended or a
/// newer one has started.
pub(crate) async fn finish_cycle(
    conn: &mut SqliteConnection,
    task_id: &str,
    run_id: Option<&str>,
    outcome: &CycleOutcome,
) -> Result<Option<WorkspaceTaskRow>, String> {
    let now = now_ms();
    let row = sqlx::query(&format!(
        r#"
        UPDATE workspace_tasks
        SET status = CASE WHEN stop_requested_at IS NULL THEN ? ELSE 'stopped' END,
            failure_reason = CASE WHEN stop_requested_at IS NULL THEN ? END,
            error = CASE WHEN stop_requested_at IS NULL THEN ? END,
            result_summary = ?, updated_at = ?, completed_at = ?
        WHERE id = ? AND run_id IS ?
          AND status IN ('queued', 'running', 'stopping')
        RETURNING {TASK_COLUMNS}
        "#
    ))
    .bind(enum_text(&outcome.status)?)
    .bind(outcome.failure_reason.as_ref().map(enum_text).transpose()?)
    .bind(&outcome.error)
    .bind(&outcome.result_summary)
    .bind(now)
    .bind(now)
    .bind(task_id)
    .bind(run_id)
    .fetch_optional(conn)
    .await
    .map_err(|e| format!("Failed to finish workspace task cycle: {}", e))?;
    row.as_ref().map(WorkspaceTaskRow::from_sql).transpose()
}

/// `queued | running → stopping`, recording the request.
/// Returns the task as it now is and whether this call changed it; a task
/// already stopping or terminal is returned unchanged. `None` when the task
/// does not exist.
pub(crate) async fn mark_stopping(
    pool: &DbPool,
    task_id: &str,
) -> Result<Option<(WorkspaceTaskRow, bool)>, String> {
    let now = now_ms();
    let row = sqlx::query(&format!(
        r#"
        UPDATE workspace_tasks
        SET status = 'stopping', stop_requested_at = ?, updated_at = ?
        WHERE id = ? AND status IN ('queued', 'running')
        RETURNING {TASK_COLUMNS}
        "#
    ))
    .bind(now)
    .bind(now)
    .bind(task_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to request task stop: {}", e))?;
    match row {
        Some(row) => Ok(Some((WorkspaceTaskRow::from_sql(&row)?, true))),
        None => Ok(load_task(pool, task_id).await?.map(|task| (task, false))),
    }
}

/// Every task below `task_id` in the delegation tree, parents first.
pub(crate) async fn descendant_ids(pool: &DbPool, task_id: &str) -> Result<Vec<String>, String> {
    sqlx::query_scalar(
        r#"
        WITH RECURSIVE tree(id, depth) AS (
            SELECT id, depth FROM workspace_tasks WHERE parent_task_id = ?
            UNION ALL
            SELECT child.id, child.depth FROM workspace_tasks AS child
            JOIN tree ON child.parent_task_id = tree.id
        )
        SELECT id FROM tree ORDER BY depth
        "#,
    )
    .bind(task_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load subtasks: {}", e))
}

/// Restart recovery for tasks the previous process left mid-cycle: a task
/// whose run never started is `queued` again without a run, for the next
/// kick; one with no session can never be dispatched and fails; a task
/// parked for its subtasks keeps waiting; every other unfinished cycle ends
/// `stopped`. Returns the stopped tasks. Run before
/// `recover_stale_runs`, which closes the runs this reads.
pub(crate) async fn stop_interrupted(
    conn: &mut SqliteConnection,
) -> Result<Vec<WorkspaceTaskRow>, String> {
    let now = now_ms();
    sqlx::query(
        r#"
        UPDATE workspace_tasks
        SET status = 'failed', failure_reason = 'restart',
            error = COALESCE(error, 'task never started: the app restarted before it was dispatched'),
            updated_at = ?, completed_at = ?
        WHERE status = 'queued' AND session_id IS NULL
        "#,
    )
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|e| format!("Failed to fail undispatched workspace tasks: {}", e))?;

    sqlx::query(
        r#"
        UPDATE workspace_tasks SET run_id = NULL
        WHERE status = 'queued'
          AND run_id IN (SELECT id FROM assistant_runs WHERE status = 'queued')
        "#,
    )
    .execute(&mut *conn)
    .await
    .map_err(|e| format!("Failed to requeue unstarted workspace tasks: {}", e))?;

    let rows = sqlx::query(&format!(
        r#"
        UPDATE workspace_tasks
        SET status = 'stopped', error = 'task interrupted by app restart',
            stop_requested_at = COALESCE(stop_requested_at, ?), updated_at = ?, completed_at = ?
        WHERE status IN ('input_required', 'stopping')
           OR (status IN ('queued', 'running') AND run_id IS NOT NULL)
        RETURNING {TASK_COLUMNS}
        "#
    ))
    .bind(now)
    .bind(now)
    .bind(now)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| format!("Failed to stop interrupted workspace tasks: {}", e))?;
    rows.iter().map(WorkspaceTaskRow::from_sql).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_task, workspace_pool};

    #[test]
    fn legacy_input_required_origin_status_decodes_as_stopped() {
        let status: TaskStatus = serde_json::from_str("\"input_required\"").unwrap();
        assert_eq!(status, TaskStatus::Stopped);
        assert_eq!(serde_json::to_string(&status).unwrap(), "\"stopped\"");
    }

    fn names(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
            .collect()
    }

    async fn task(pool: &DbPool, id: &str) -> WorkspaceTaskRow {
        load_task(pool, id)
            .await
            .expect("query failed")
            .unwrap_or_else(|| panic!("{id} should exist"))
    }

    async fn insert(pool: &DbPool, task: NewTask<'_>, error: Option<&str>) {
        let mut conn = pool.acquire().await.expect("connection");
        create_task_row(&mut conn, &task, error)
            .await
            .expect("insert");
    }

    fn new_task<'a>(id: &'a str, creator: Option<&'a str>, assignee: &'a str) -> NewTask<'a> {
        NewTask {
            id,
            created_by_workspace_agent_id: creator,
            assigned_to_workspace_agent_id: assignee,
            assigned_agent_definition_id: "def-1",
            title: "T",
            instructions: "Do it",
            lineage: TaskLineage::default(),
        }
    }

    #[tokio::test]
    async fn created_task_round_trips_through_load() {
        let (_tmp, pool) = workspace_pool().await;
        insert(
            &pool,
            new_task("task-1", Some("wa-mgr"), "wa-1"),
            Some("boom"),
        )
        .await;

        let row = task(&pool, "task-1").await;

        assert_eq!(row.created_by_workspace_agent_id.as_deref(), Some("wa-mgr"));
        assert_eq!(row.assigned_to_workspace_agent_id, "wa-1");
        assert_eq!(row.assigned_agent_definition_id, "def-1");
        assert_eq!(
            (row.title.as_str(), row.instructions.as_str()),
            ("T", "Do it")
        );
        assert_eq!(row.status, TaskStatus::Failed);
        assert_eq!(
            row.failure_reason,
            Some(FailureReason::ConnectionUnavailable)
        );
        assert_eq!(row.error.as_deref(), Some("boom"));
        assert!(row.completed_at.is_some());
    }

    #[tokio::test]
    async fn load_task_returns_none_for_unknown_id() {
        let (_tmp, pool) = workspace_pool().await;
        insert_task(&pool, "task-1", "completed", None, None).await;

        assert_eq!(load_task(&pool, "task-404").await.expect("query"), None);
    }

    #[tokio::test]
    async fn list_and_single_load_read_the_same_row() {
        let (_tmp, pool) = workspace_pool().await;
        insert_task(&pool, "task-1", "completed", Some("sess-1"), None).await;

        let listed = list_recent_tasks(&pool, 50).await.expect("list");

        assert_eq!(listed, vec![task(&pool, "task-1").await]);
    }

    #[tokio::test]
    async fn list_recent_tasks_is_newest_first_and_capped() {
        let (_tmp, pool) = workspace_pool().await;
        for id in ["old", "new", "mid"] {
            insert_task(&pool, id, "queued", None, None).await;
        }
        for (id, updated_at) in [("old", 1), ("new", 3), ("mid", 2)] {
            sqlx::query("UPDATE workspace_tasks SET updated_at = ? WHERE id = ?")
                .bind(updated_at)
                .bind(id)
                .execute(&pool)
                .await
                .expect("touch");
        }

        let ids: Vec<_> = list_recent_tasks(&pool, 2)
            .await
            .expect("list")
            .into_iter()
            .map(|row| row.id)
            .collect();

        assert_eq!(ids, ["new", "mid"]);
    }

    #[tokio::test]
    async fn response_names_agents_from_the_roster() {
        let (_tmp, pool) = workspace_pool().await;
        insert(&pool, new_task("task-1", Some("wa-mgr"), "wa-1"), None).await;

        let response = task(&pool, "task-1")
            .await
            .into_response("ws-1", &names(&[("wa-mgr", "Manager"), ("wa-1", "Rust")]));

        assert_eq!(response.workspace_id, "ws-1");
        assert_eq!(response.assigned_agent_display_name, "Rust");
        assert_eq!(response.created_by_display_name.as_deref(), Some("Manager"));
    }

    #[tokio::test]
    async fn response_for_agents_off_the_roster() {
        let (_tmp, pool) = workspace_pool().await;
        insert(
            &pool,
            new_task("task-1", Some("wa-removed"), "wa-gone"),
            None,
        )
        .await;

        let response = task(&pool, "task-1")
            .await
            .into_response("ws-1", &names(&[]));

        // The assignee falls back to its definition id; the creator keeps its
        // raw id but gets no name.
        assert_eq!(response.assigned_to_workspace_agent_id, "wa-gone");
        assert_eq!(response.assigned_agent_display_name, "def-1");
        assert_eq!(
            response.created_by_workspace_agent_id.as_deref(),
            Some("wa-removed")
        );
        assert_eq!(response.created_by_display_name, None);
    }

    #[tokio::test]
    async fn task_updated_payload_is_camel_case() {
        let (_tmp, pool) = workspace_pool().await;
        insert(
            &pool,
            new_task("task-1", Some("wa-mgr"), "wa-1"),
            Some("no connection"),
        )
        .await;
        let task = task(&pool, "task-1")
            .await
            .into_response("ws-1", &names(&[]));

        let event = crate::assistant::events::AssistantUiEvent::TaskUpdated {
            task: Box::new(task),
        };
        let json = serde_json::to_value(&event).expect("serialize");

        assert_eq!(json["type"], "task_updated");
        let task = &json["payload"]["task"];
        assert_eq!(task["id"], "task-1");
        assert_eq!(task["status"], "failed");
        assert_eq!(task["failureReason"], "connection_unavailable");
        assert_eq!(task["cycleNo"], 0);
        assert_eq!(task["depth"], 0);
        assert_eq!(task["assignedToWorkspaceAgentId"], "wa-1");
        assert!(task.get("failure_reason").is_none());
    }
}

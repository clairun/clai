//! The `workspace_tasks` table: one row shape, one column list, and the one
//! response every reader (the UI and the agents' task tools) gets.

use crate::db::DbPool;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
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
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_acknowledged_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceTaskRow {
    pub(crate) id: String,
    pub(crate) created_by_workspace_agent_id: Option<String>,
    pub(crate) assigned_to_workspace_agent_id: String,
    pub(crate) assigned_agent_definition_id: String,
    pub(crate) title: String,
    pub(crate) instructions: String,
    pub(crate) status: String,
    pub(crate) result_summary: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) completed_at: Option<i64>,
    pub(crate) attention_acknowledged_at: Option<i64>,
}

const TASK_COLUMNS: &str = "id, created_by_workspace_agent_id, assigned_to_workspace_agent_id, \
     assigned_agent_definition_id, title, instructions, status, result_summary, error, \
     session_id, run_id, created_at, updated_at, completed_at, attention_acknowledged_at";

impl WorkspaceTaskRow {
    fn from_sql(row: &sqlx::sqlite::SqliteRow) -> Self {
        Self {
            id: row.get("id"),
            created_by_workspace_agent_id: row.get("created_by_workspace_agent_id"),
            assigned_to_workspace_agent_id: row.get("assigned_to_workspace_agent_id"),
            assigned_agent_definition_id: row.get("assigned_agent_definition_id"),
            title: row.get("title"),
            instructions: row.get("instructions"),
            status: row.get("status"),
            result_summary: row.get("result_summary"),
            error: row.get("error"),
            session_id: row.get("session_id"),
            run_id: row.get("run_id"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            completed_at: row.get("completed_at"),
            attention_acknowledged_at: row.get("attention_acknowledged_at"),
        }
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
            result_summary: self.result_summary,
            error: self.error,
            session_id: self.session_id,
            run_id: self.run_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
            completed_at: self.completed_at,
            attention_acknowledged_at: self.attention_acknowledged_at,
        }
    }
}

pub(crate) async fn load_task(
    pool: &DbPool,
    task_id: &str,
) -> Result<Option<WorkspaceTaskRow>, String> {
    let row = sqlx::query(&format!(
        "SELECT {TASK_COLUMNS} FROM workspace_tasks WHERE id = ?"
    ))
    .bind(task_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load workspace task: {}", e))?;
    Ok(row.as_ref().map(WorkspaceTaskRow::from_sql))
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
    Ok(rows.iter().map(WorkspaceTaskRow::from_sql).collect())
}

pub(crate) async fn create_task_row(
    pool: &DbPool,
    task_id: &str,
    created_by_workspace_agent_id: Option<&str>,
    assigned_to_workspace_agent_id: &str,
    assigned_agent_definition_id: &str,
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
    .bind(assigned_to_workspace_agent_id)
    .bind(assigned_agent_definition_id)
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
pub(crate) async fn update_task_status(
    pool: &DbPool,
    task_id: &str,
    status: &str,
    session_id: Option<&str>,
    run_id: Option<&str>,
    error: Option<&str>,
    result_summary: Option<&str>,
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
    .bind(now)
    .bind(if terminal { Some(now) } else { None })
    .bind(task_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update workspace task: {}", e))?;

    Ok(())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_task, workspace_pool};

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

    #[tokio::test]
    async fn created_task_round_trips_through_load() {
        let (_tmp, pool) = workspace_pool().await;
        create_task_row(
            &pool,
            "task-1",
            Some("wa-mgr"),
            "wa-1",
            "def-1",
            "T",
            "Do it",
        )
        .await
        .expect("insert");
        update_task_status(
            &pool,
            "task-1",
            "failed",
            Some("s"),
            Some("r"),
            Some("boom"),
            None,
            true,
        )
        .await
        .expect("update");

        let row = task(&pool, "task-1").await;

        assert_eq!(row.created_by_workspace_agent_id.as_deref(), Some("wa-mgr"));
        assert_eq!(row.assigned_to_workspace_agent_id, "wa-1");
        assert_eq!(row.assigned_agent_definition_id, "def-1");
        assert_eq!(
            (row.title.as_str(), row.instructions.as_str()),
            ("T", "Do it")
        );
        assert_eq!(row.status, "failed");
        assert_eq!(row.error.as_deref(), Some("boom"));
        assert_eq!(row.session_id.as_deref(), Some("s"));
        assert_eq!(row.run_id.as_deref(), Some("r"));
        assert!(row.completed_at.is_some());
        assert_eq!(row.attention_acknowledged_at, None);
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
        sqlx::query("UPDATE workspace_tasks SET attention_acknowledged_at = 7 WHERE id = 'task-1'")
            .execute(&pool)
            .await
            .expect("ack");

        let listed = list_recent_tasks(&pool, 50).await.expect("list");

        assert_eq!(listed, vec![task(&pool, "task-1").await]);
        assert_eq!(listed[0].attention_acknowledged_at, Some(7));
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
        create_task_row(&pool, "task-1", Some("wa-mgr"), "wa-1", "def-1", "T", "I")
            .await
            .expect("insert");

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
        create_task_row(
            &pool,
            "task-1",
            Some("wa-removed"),
            "wa-gone",
            "def-1",
            "T",
            "I",
        )
        .await
        .expect("insert");

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
}

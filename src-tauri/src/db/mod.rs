//! Database layer.
//!
//! There is only one kind of pool: a **per-workspace pool**
//! (`<root>/.clai/data.sqlite`), one per workspace, holding that
//! workspace's sessions/messages/runs/tool_calls plus its
//! delegated-task queue. Workspace identity is implicit by which DB
//! you connected to — there are no `workspace_id` columns.
//!
//! Schema is managed entirely by `sqlx::migrate!`. Every schema change
//! is a new numbered `.sql` file dropped into `migrations/workspace/`.
//! The macro embeds them at compile time and tracks applied versions
//! per-DB via the `_sqlx_migrations` table. Calling `run` is idempotent:
//! already-applied versions are skipped, pending ones are applied in
//! order inside a transaction.
//!
//! Workspaces idle across app updates catch up automatically the next
//! time they are opened — startup eager fan-out walks every indexed
//! workspace and calls `init_workspace_db`, and the lazy-open path goes
//! through the same function. Both apply pending migrations.

use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::path::Path;

/// SQLite connection pool, the only persistent storage handle outside
/// the OS keyring and `.clai/config.json` files.
pub type DbPool = Pool<Sqlite>;

/// Open (creating if missing) a workspace's `data.sqlite`, apply any
/// pending migrations, then run recovery sweeps so the UI never shows
/// orphaned `running` rows from a previous crashed app process.
pub async fn init_workspace_db(workspace_root: &Path) -> Result<DbPool, String> {
    let db_path = crate::config::workspace_config::data_path(workspace_root);
    let parent = db_path
        .parent()
        .ok_or_else(|| "Could not determine workspace DB directory".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create workspace DB directory: {}", e))?;

    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .map_err(|e| format!("Failed to connect to workspace SQLite database: {}", e))?;

    sqlx::migrate!("./migrations/workspace")
        .run(&pool)
        .await
        .map_err(|e| format!("Workspace DB migration failed: {}", e))?;

    sweep_orphaned_running_state(&pool).await?;
    crate::assistant::repository::recover_stale_runs(&pool).await?;

    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .map_err(|e| format!("Failed to enable workspace foreign keys: {}", e))?;

    Ok(pool)
}

/// Startup recovery: mark `workspace_tasks` rows stuck in `running` as
/// `failed`. They are orphans from a previous app process that died,
/// was killed by a rebuild, or otherwise didn't finalize. Without this,
/// the rows pile up as forever-"RUNNING" in the UI (the agent session
/// that owned them no longer exists, nothing can resolve them).
///
/// `assistant_runs` and `assistant_tool_calls` are NOT touched here —
/// `crate::assistant::repository::recover_stale_runs` already handles those
/// at workspace-DB open, and its SQL uses the JSON-quoted enum format
/// the column actually stores (e.g. `'"running"'`, not `'running'`).
pub async fn sweep_orphaned_running_state(pool: &DbPool) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();

    let tasks = sqlx::query(
        r#"
        UPDATE workspace_tasks
        SET status = 'failed',
            error = COALESCE(error, 'task interrupted by app restart'),
            updated_at = ?,
            completed_at = ?
        WHERE status = 'running'
        "#,
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to sweep orphaned workspace_tasks: {}", e))?;
    if tasks.rows_affected() > 0 {
        tracing::info!(
            "Marked {} workspace_tasks as failed (orphaned 'running' state from previous app session)",
            tasks.rows_affected()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spins up a per-workspace pool in a tempdir. Runs the embedded
    /// workspace migrations so the schema matches production.
    async fn create_workspace_test_pool() -> (tempfile::TempDir, DbPool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init_workspace_db(tmp.path()).await.unwrap();
        (tmp, pool)
    }

    async fn insert_sweep_task(pool: &DbPool, id: &str, status: &str, error: Option<&str>) {
        sqlx::query(
            r#"
            INSERT INTO workspace_tasks
                (id, created_by_workspace_agent_id, assigned_to_workspace_agent_id,
                 assigned_agent_definition_id, title, instructions, status, error,
                 created_at, updated_at)
            VALUES (?, NULL, 'agent-1', 'agent-1', 'Title', 'Do it', ?, ?, 1, 1)
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(error)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn workspace_init_creates_expected_tables() {
        let (_tmp, pool) = create_workspace_test_pool().await;

        for table in [
            "assistant_sessions",
            "assistant_messages",
            "assistant_runs",
            "assistant_tool_calls",
            "workspace_tasks",
        ] {
            let exists: bool = sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
            )
            .bind(table)
            .fetch_optional(&pool)
            .await
            .unwrap()
            .is_some();
            assert!(
                exists,
                "expected table `{}` to be created by migrations",
                table
            );
        }
    }

    #[tokio::test]
    async fn workspace_init_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let _pool = init_workspace_db(tmp.path()).await.unwrap();
        // Second open of the same workspace must succeed and not double-
        // apply migrations. sqlx::migrate! tracks via _sqlx_migrations.
        let _pool = init_workspace_db(tmp.path()).await.unwrap();
    }

    #[tokio::test]
    async fn workspace_tasks_has_no_workspace_id_column() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        let columns: Vec<String> = sqlx::query_scalar::<_, String>(
            "SELECT name FROM pragma_table_info('workspace_tasks')",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            !columns.iter().any(|c| c == "workspace_id"),
            "workspace_id column should be implicit-by-DB; columns: {:?}",
            columns
        );
    }

    #[tokio::test]
    async fn sweep_marks_running_rows_as_failed() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        insert_sweep_task(&pool, "t1", "running", None).await;

        sweep_orphaned_running_state(&pool).await.unwrap();

        let row: (String, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT status, error, completed_at FROM workspace_tasks WHERE id = 't1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(row.1.as_deref(), Some("task interrupted by app restart"));
        assert!(row.2.is_some(), "completed_at must be stamped");
    }

    #[tokio::test]
    async fn sweep_preserves_existing_error_via_coalesce() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        insert_sweep_task(&pool, "t1", "running", Some("custom failure reason")).await;

        sweep_orphaned_running_state(&pool).await.unwrap();

        let error: Option<String> =
            sqlx::query_scalar("SELECT error FROM workspace_tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(error.as_deref(), Some("custom failure reason"));
    }

    #[tokio::test]
    async fn sweep_leaves_non_running_rows_untouched() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        insert_sweep_task(&pool, "done", "completed", None).await;
        insert_sweep_task(&pool, "fail", "failed", Some("original error")).await;
        insert_sweep_task(&pool, "pending", "pending", None).await;

        sweep_orphaned_running_state(&pool).await.unwrap();

        let rows: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT id, status, error FROM workspace_tasks ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, "completed");
        assert_eq!(rows[1].1, "failed");
        assert_eq!(rows[1].2.as_deref(), Some("original error"));
        assert_eq!(rows[2].1, "pending");
    }
}

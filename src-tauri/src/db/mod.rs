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

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Pool, Sqlite};
use std::path::Path;
use std::time::Duration;

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

    // Connection options applied to *every* pooled connection (sqlx runs
    // them on connect), unlike a one-off `pool.execute(PRAGMA …)` which only
    // touches a single connection.
    //
    // WAL + synchronous=NORMAL is the fix for the high system iowait we were
    // seeing: the SQLite default (rollback journal, synchronous=FULL) forces
    // an fsync — and on ext4 a journal commit — on *every* write. Under the
    // app's frequent small writes (amplified when several agents write the
    // same workspace data.sqlite at once) that drove %wa very high despite
    // tiny byte volume. WAL fsyncs only at checkpoint, not per commit.
    let connect_options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        // WAL still serializes writers; wait briefly instead of erroring with
        // SQLITE_BUSY when concurrent agents write the same workspace DB.
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(connect_options)
        .await
        .map_err(|e| format!("Failed to connect to workspace SQLite database: {}", e))?;

    sqlx::migrate!("./migrations/workspace")
        .run(&pool)
        .await
        .map_err(|e| format!("Workspace DB migration failed: {}", e))?;

    sweep_orphaned_task_state(&pool).await?;
    crate::assistant::repository::recover_stale_runs(&pool).await?;

    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .map_err(|e| format!("Failed to enable workspace foreign keys: {}", e))?;

    Ok(pool)
}

/// Startup recovery: mark `workspace_tasks` rows stuck in a non-terminal
/// state (`queued` or `running`) as `failed`. They are orphans from a
/// previous app process that died, was killed by a rebuild, or otherwise
/// didn't finalize. Without this, the rows pile up as forever-"RUNNING" or
/// forever-"QUEUED" in the UI, and a manager agent polling
/// `workspace_getTaskResult` never sees a terminal status, so it waits on a
/// task nothing can ever resolve: the run and the agent session that owned
/// the task are gone with the previous process.
///
/// `queued` is a real orphan state, not just a transient one. A task row is
/// inserted as `queued` before its provider connection, session, message and
/// run rows exist (`assistant::tools::workspace_tasks::assign_task`), and it
/// only flips to `running` inside the spawned task; quitting anywhere in that
/// window strands it. There is no dispatcher that picks `queued` rows back up
/// after a restart, so failing them is the only honest outcome.
///
/// Runs on every workspace-DB open, and a workspace is USUALLY opened once per
/// process (pools are cached in the workspace index), but that is not
/// guaranteed: `AppState::workspace_db` inits outside the index lock, so during
/// the startup window the eager fan-out and a lazy open can both open the same
/// workspace and both sweep. A task created in between would be failed while
/// live. The exposure is the same one `recover_stale_runs` (called on the next
/// line, and strictly wider — it fails `assistant_runs` too) already carries, so
/// do not build on an at-most-once assumption here; fix the double open if it
/// ever matters.
///
/// `assistant_runs` and `assistant_tool_calls` are NOT touched here —
/// `crate::assistant::repository::recover_stale_runs` already handles those
/// at workspace-DB open, and its SQL uses the JSON-quoted enum format
/// the column actually stores (e.g. `'"running"'`, not `'running'`).
pub async fn sweep_orphaned_task_state(pool: &DbPool) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();

    // SQLite evaluates every SET expression against the row's ORIGINAL
    // values, so the CASE still sees the pre-sweep status.
    let tasks = sqlx::query(
        r#"
        UPDATE workspace_tasks
        SET status = 'failed',
            error = COALESCE(
                error,
                CASE status
                    WHEN 'queued' THEN 'task never started: the app restarted before it was dispatched'
                    ELSE 'task interrupted by app restart'
                END
            ),
            updated_at = ?,
            completed_at = ?
        WHERE status IN ('queued', 'running')
        "#,
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to sweep orphaned workspace_tasks: {}", e))?;
    if tasks.rows_affected() > 0 {
        tracing::info!(
            "Marked {} workspace_tasks as failed (orphaned 'queued'/'running' state from previous app session)",
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

        sweep_orphaned_task_state(&pool).await.unwrap();

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

        sweep_orphaned_task_state(&pool).await.unwrap();

        let error: Option<String> =
            sqlx::query_scalar("SELECT error FROM workspace_tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(error.as_deref(), Some("custom failure reason"));
    }

    /// A task row is inserted as `queued` before its session and run exist and
    /// only flips to `running` inside the spawned task, so quitting in that
    /// window leaves a `queued` orphan nothing will ever dispatch.
    #[tokio::test]
    async fn sweep_marks_queued_rows_as_failed_with_a_never_started_reason() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        insert_sweep_task(&pool, "t1", "queued", None).await;

        sweep_orphaned_task_state(&pool).await.unwrap();

        let row: (String, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT status, error, completed_at FROM workspace_tasks WHERE id = 't1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(
            row.1.as_deref(),
            Some("task never started: the app restarted before it was dispatched")
        );
        assert!(row.2.is_some(), "completed_at must be stamped");
    }

    /// The reason is per-row: the CASE in the UPDATE must read each row's
    /// pre-sweep status, not the `'failed'` it is being set to.
    #[tokio::test]
    async fn sweep_reasons_are_per_row() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        insert_sweep_task(&pool, "a-queued", "queued", None).await;
        insert_sweep_task(&pool, "b-running", "running", None).await;

        sweep_orphaned_task_state(&pool).await.unwrap();

        let rows: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT id, status, error FROM workspace_tasks ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "failed");
        assert_eq!(
            rows[0].2.as_deref(),
            Some("task never started: the app restarted before it was dispatched")
        );
        assert_eq!(rows[1].1, "failed");
        assert_eq!(
            rows[1].2.as_deref(),
            Some("task interrupted by app restart")
        );
    }

    #[tokio::test]
    async fn sweep_leaves_terminal_rows_untouched() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        insert_sweep_task(&pool, "done", "completed", None).await;
        insert_sweep_task(&pool, "fail", "failed", Some("original error")).await;
        insert_sweep_task(&pool, "stuck", "blocked", Some("needs a decision")).await;

        sweep_orphaned_task_state(&pool).await.unwrap();

        let rows: Vec<(String, String, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT id, status, error, completed_at FROM workspace_tasks ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, "completed");
        assert_eq!(rows[1].1, "failed");
        assert_eq!(rows[1].2.as_deref(), Some("original error"));
        assert_eq!(rows[2].1, "blocked");
        assert_eq!(rows[2].2.as_deref(), Some("needs a decision"));
        assert!(
            rows[2].3.is_none(),
            "a blocked task awaiting the user must not be completed by the sweep"
        );
    }

    /// A reason the task already recorded wins over the sweep's generic one,
    /// on the `queued` arm as well as the `running` one: COALESCE is evaluated
    /// before the CASE arm it guards.
    #[tokio::test]
    async fn sweep_preserves_an_existing_error_on_queued_rows_too() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        insert_sweep_task(&pool, "t1", "queued", Some("no active provider connection")).await;

        sweep_orphaned_task_state(&pool).await.unwrap();

        let row: (String, Option<String>) =
            sqlx::query_as("SELECT status, error FROM workspace_tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(row.1.as_deref(), Some("no active provider connection"));
    }

    /// The sweep is idempotent: the rows it already failed are terminal, so a
    /// second pass must not re-stamp `updated_at`/`completed_at` or overwrite
    /// the reason it wrote the first time.
    #[tokio::test]
    async fn a_second_sweep_matches_nothing() {
        let (_tmp, pool) = create_workspace_test_pool().await;
        insert_sweep_task(&pool, "t1", "queued", None).await;
        sweep_orphaned_task_state(&pool).await.unwrap();

        let before: (String, Option<String>, Option<i64>, i64) = sqlx::query_as(
            "SELECT status, error, completed_at, updated_at FROM workspace_tasks WHERE id = 't1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        sweep_orphaned_task_state(&pool).await.unwrap();

        let after: (String, Option<String>, Option<i64>, i64) = sqlx::query_as(
            "SELECT status, error, completed_at, updated_at FROM workspace_tasks WHERE id = 't1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, after);
    }

    /// `init_workspace_db` must run the sweep itself: the eager startup fan-out
    /// and the lazy-open path both go through it and nothing else sweeps.
    #[tokio::test]
    async fn workspace_init_sweeps_orphaned_task_state() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init_workspace_db(tmp.path()).await.unwrap();
        insert_sweep_task(&pool, "t1", "queued", None).await;
        insert_sweep_task(&pool, "t2", "running", None).await;
        drop(pool);

        let pool = init_workspace_db(tmp.path()).await.unwrap();
        let statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM workspace_tasks ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(statuses, vec!["failed".to_string(), "failed".to_string()]);
    }
}

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

use serde::de::DeserializeOwned;
use serde::Serialize;
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
        .busy_timeout(Duration::from_secs(5))
        // SQLite leaves FK enforcement off by default for backward
        // compatibility, and `PRAGMA foreign_keys` is per-connection — which
        // is why this belongs here and not in a one-off `pool.execute`. sqlx
        // happens to default it on, but the schema's ON DELETE CASCADEs are
        // load-bearing (`delete_session` relies on them), so say so rather
        // than inherit a dependency default that could change.
        .foreign_keys(true);

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
/// at workspace-DB open.
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

/// A unit enum stored in a TEXT column: the bare string serde gives it
/// (`RunStatus::Running` → `running`). Pair with [`parse_enum_text`].
pub(crate) fn enum_text<T: Serialize>(value: &T) -> Result<String, String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        Ok(other) => Err(format!("Expected a unit enum, got {other}")),
        Err(e) => Err(format!("Failed to serialize enum: {e}")),
    }
}

pub(crate) fn parse_enum_text<T: DeserializeOwned>(
    text: String,
    context: &str,
) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(text))
        .map_err(|e| format!("Failed to parse {context}: {e}"))
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::test_support::{insert_task, workspace_pool};
    use super::*;

    #[tokio::test]
    async fn workspace_init_creates_expected_tables() {
        let (_tmp, pool) = workspace_pool().await;

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
        let (_tmp, pool) = workspace_pool().await;
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

    /// Dropped columns must stay dropped: nothing reads them, so a migration
    /// that silently fails to drop one would otherwise ship green.
    #[tokio::test]
    async fn dropped_columns_stay_dropped() {
        let (_tmp, pool) = workspace_pool().await;
        for (table, column) in [
            ("assistant_runs", "usage_json"),
            ("assistant_runs", "notices_json"),
            ("assistant_sessions", "kind"),
            ("assistant_sessions", "title"),
            ("workspace_tasks", "user_response"),
            ("workspace_tasks", "user_response_at"),
            ("workspace_tasks", "result_json"),
        ] {
            let present: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?")
                    .bind(table)
                    .bind(column)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(present, 0, "{table}.{column} should be dropped");
        }
    }

    /// A pool with every migration older than `version` applied. Tests seed it
    /// with raw SQL: repository code targets the current schema, not this one.
    async fn pool_migrated_before(version: i64) -> DbPool {
        // One connection: every connection to `:memory:` is its own database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap();
        let mut before = sqlx::migrate!("./migrations/workspace");
        before.migrations = before
            .migrations
            .iter()
            .filter(|migration| migration.version < version)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        before.run(&pool).await.unwrap();
        pool
    }

    async fn run_all_migrations(pool: &DbPool) {
        sqlx::migrate!("./migrations/workspace")
            .run(pool)
            .await
            .unwrap();
    }

    /// Legacy `retry`/`inter_agent_call` rows, quoted or not, must still
    /// deserialize once `20261013000000_drop_dead_run_triggers` has run.
    #[tokio::test]
    async fn dead_run_triggers_are_mapped_to_live_ones() {
        use crate::assistant::repository;
        use crate::assistant::types::RunTrigger;

        let pool = pool_migrated_before(20261013000000).await;
        sqlx::query(
            "INSERT INTO assistant_sessions (id, kind, title, context_json, created_at, updated_at)
             VALUES ('s', '\"interactive\"', NULL, '{}', 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (id, legacy) in [
            ("r1", "\"retry\""),
            ("r2", "retry"),
            ("r3", "\"inter_agent_call\""),
            ("r4", "inter_agent_call"),
        ] {
            sqlx::query(
                "INSERT INTO assistant_runs
                     (id, session_id, status, \"trigger\", connection_id, protocol_id, model_id, started_at)
                 VALUES (?, 's', '\"completed\"', ?, 'c', 'openai', 'm', 1)",
            )
            .bind(id)
            .bind(legacy)
            .execute(&pool)
            .await
            .unwrap();
        }

        run_all_migrations(&pool).await;

        let mut triggers = Vec::new();
        for id in ["r1", "r2", "r3", "r4"] {
            let run = repository::get_run(&pool, id).await.unwrap().unwrap();
            triggers.push(run.trigger);
        }
        assert_eq!(
            triggers,
            [
                RunTrigger::UserMessage,
                RunTrigger::UserMessage,
                RunTrigger::WorkspaceTask,
                RunTrigger::WorkspaceTask,
            ]
        );
    }

    /// `20261014000000_drop_dead_columns` keeps answered tasks out of the
    /// attention count and leaves the rows it rewrites loadable.
    #[tokio::test]
    async fn dead_columns_drop_keeps_answered_tasks_acknowledged() {
        use crate::assistant::repository;

        let pool = pool_migrated_before(20261014000000).await;
        sqlx::query(
            "INSERT INTO assistant_sessions (id, kind, title, context_json, created_at, updated_at)
             VALUES ('s', '\"background_job\"', 'Task: t', '{}', 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO assistant_runs
                 (id, session_id, status, \"trigger\", connection_id, protocol_id, model_id,
                  notices_json, started_at)
             VALUES ('r', 's', '\"completed_with_warnings\"', '\"workspace_task\"', 'c', 'openai',
                     'm', '[{\"kind\":\"command_denied\",\"message\":\"x\",\"timestamp\":1}]', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (id, status, acknowledged_at) in [
            ("answered", "blocked", None),
            ("acknowledged", "failed", Some(7_i64)),
        ] {
            sqlx::query(
                "INSERT INTO workspace_tasks
                     (id, assigned_to_workspace_agent_id, assigned_agent_definition_id, title,
                      instructions, status, result_summary, result_json, created_at, updated_at,
                      attention_acknowledged_at, user_response, user_response_at)
                 VALUES (?, 'wa', 'def', 't', 'i', ?, 'done', '{\"summary\":\"done\"}', 1, 1,
                         ?, 'yes', 5)",
            )
            .bind(id)
            .bind(status)
            .bind(acknowledged_at)
            .execute(&pool)
            .await
            .unwrap();
        }

        run_all_migrations(&pool).await;

        let acknowledged: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT id, attention_acknowledged_at FROM workspace_tasks ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            acknowledged,
            [
                ("acknowledged".to_string(), Some(7)),
                ("answered".to_string(), Some(5)),
            ]
        );
        assert!(repository::get_session(&pool, "s").await.unwrap().is_some());
        assert!(repository::get_run(&pool, "r").await.unwrap().is_some());
    }

    /// `20261015000000_plain_enum_columns` unquotes JSON-string enums, leaves
    /// already-plain values alone, and keeps every row loadable.
    #[tokio::test]
    async fn quoted_enum_columns_become_plain_text() {
        use crate::assistant::repository;
        use crate::assistant::types::{
            CompactionStatus, MessageRole, RunStatus, RunTrigger, ToolCallStatus,
        };

        let pool = pool_migrated_before(20261015000000).await;
        for statement in [
            "INSERT INTO assistant_sessions (id, context_json, created_at, updated_at)
             VALUES ('s', '{}', 1, 1)",
            "INSERT INTO assistant_messages (id, session_id, role, content_json, created_at)
             VALUES ('m1', 's', '\"user\"', '[]', 1), ('m2', 's', 'assistant', '[]', 2)",
            "INSERT INTO assistant_runs
                 (id, session_id, status, \"trigger\", connection_id, protocol_id, model_id, started_at)
             VALUES ('r1', 's', '\"running\"', '\"scheduled\"', 'c', 'openai', 'm', 1),
                    ('r2', 's', 'completed', 'user_message', 'c', 'openai', 'm', 2)",
            "INSERT INTO assistant_tool_calls
                 (id, run_id, session_id, tool_name, params_json, status, started_at)
             VALUES ('t1', 'r1', 's', 'x', '{}', '\"pending\"', 1),
                    ('t2', 'r2', 's', 'x', '{}', 'completed', 2)",
            "INSERT INTO assistant_compactions
                 (id, session_id, \"trigger\", strategy, status, source_to_message_id,
                  summary_message_id, protocol_id, model_id, created_at, completed_at)
             VALUES ('c1', 's', '\"automatic\"', '\"local_summary\"', '\"completed\"', 'm1', 'm2',
                     'openai', 'm', 1, 1)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }

        run_all_migrations(&pool).await;

        let stored: Vec<String> = sqlx::query_scalar(
            "SELECT role FROM assistant_messages
             UNION ALL SELECT status || ' ' || \"trigger\" FROM assistant_runs
             UNION ALL SELECT status FROM assistant_tool_calls
             UNION ALL SELECT \"trigger\" || ' ' || strategy || ' ' || status
                       FROM assistant_compactions",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            stored,
            [
                "user",
                "assistant",
                "running scheduled",
                "completed user_message",
                "pending",
                "completed",
                "automatic local_summary completed",
            ]
        );

        let roles: Vec<MessageRole> = repository::list_messages(&pool, "s")
            .await
            .unwrap()
            .into_iter()
            .map(|message| message.role)
            .collect();
        assert_eq!(roles, [MessageRole::User, MessageRole::Assistant]);
        let run = repository::get_run(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(
            (run.status, run.trigger),
            (RunStatus::Running, RunTrigger::Scheduled)
        );
        assert!(repository::session_has_active_run(&pool, "s")
            .await
            .unwrap());
        let tool_call = repository::get_tool_call_for_session(&pool, "s", "t1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tool_call.status, ToolCallStatus::Pending);
        let compaction = repository::latest_completed_compaction(&pool, "s")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(compaction.status, CompactionStatus::Completed);

        let quoted = sqlx::query(
            "INSERT INTO assistant_compactions
                 (id, session_id, \"trigger\", strategy, status, protocol_id, model_id, created_at)
             VALUES ('c2', 's', 'manual', 'local_summary', '\"completed\"', 'openai', 'm', 2)",
        )
        .execute(&pool)
        .await;
        assert!(
            quoted.is_err(),
            "the rebuilt CHECK must reject quoted values"
        );
    }

    /// The same migration rebuilds `assistant_compactions`; the rebuilt table
    /// keeps its indexes and foreign keys.
    #[tokio::test]
    async fn plain_enum_rebuild_keeps_compaction_indexes_and_foreign_keys() {
        let pool = pool_migrated_before(20261015000000).await;
        let compaction_foreign_keys = || async {
            sqlx::query_as::<_, (String, String, String, String, String)>(
                "SELECT \"table\", \"from\", \"to\", on_update, on_delete
                 FROM pragma_foreign_key_list('assistant_compactions')
                 ORDER BY \"from\"",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        };
        let original_foreign_keys = compaction_foreign_keys().await;
        assert_eq!(original_foreign_keys.len(), 5);

        run_all_migrations(&pool).await;

        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND tbl_name = 'assistant_compactions' AND sql IS NOT NULL
             ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            indexes,
            [
                "idx_assistant_compactions_session_created",
                "idx_assistant_compactions_session_status",
            ]
        );
        assert_eq!(compaction_foreign_keys().await, original_foreign_keys);
    }

    #[tokio::test]
    async fn sweep_marks_running_rows_as_failed() {
        let (_tmp, pool) = workspace_pool().await;
        insert_task(&pool, "t1", "running", None, None).await;

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
        let (_tmp, pool) = workspace_pool().await;
        insert_task(&pool, "t1", "running", None, Some("custom failure reason")).await;

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
        let (_tmp, pool) = workspace_pool().await;
        insert_task(&pool, "t1", "queued", None, None).await;

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
        let (_tmp, pool) = workspace_pool().await;
        insert_task(&pool, "a-queued", "queued", None, None).await;
        insert_task(&pool, "b-running", "running", None, None).await;

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
    async fn sweep_leaves_completed_failed_and_blocked_rows_untouched() {
        let (_tmp, pool) = workspace_pool().await;
        insert_task(&pool, "done", "completed", None, None).await;
        insert_task(&pool, "fail", "failed", None, Some("original error")).await;
        insert_task(&pool, "stuck", "blocked", None, Some("needs a decision")).await;

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
        let (_tmp, pool) = workspace_pool().await;
        insert_task(
            &pool,
            "t1",
            "queued",
            None,
            Some("no active provider connection"),
        )
        .await;

        sweep_orphaned_task_state(&pool).await.unwrap();

        let row: (String, Option<String>) =
            sqlx::query_as("SELECT status, error FROM workspace_tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(row.1.as_deref(), Some("no active provider connection"));
    }

    /// The sweep is idempotent: a row it already failed no longer matches the
    /// WHERE clause, so a second pass must not re-stamp
    /// `updated_at`/`completed_at` or overwrite the reason it wrote.
    #[tokio::test]
    async fn a_second_sweep_changes_nothing() {
        let (_tmp, pool) = workspace_pool().await;
        insert_task(&pool, "t1", "queued", None, None).await;
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
        insert_task(&pool, "t1", "queued", None, None).await;
        insert_task(&pool, "t2", "running", None, None).await;
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

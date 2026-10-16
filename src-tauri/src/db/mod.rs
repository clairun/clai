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

use crate::assistant::workspace_agents::WorkspaceAgentRow;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// SQLite connection pool, the only persistent storage handle outside
/// the OS keyring and `.clai/config.json` files.
pub type DbPool = Pool<Sqlite>;

/// Per database file: serializes opens and records whether this process
/// already ran recovery on it.
type OpenGate = Arc<tokio::sync::Mutex<bool>>;

static OPEN_GATES: OnceLock<Mutex<HashMap<PathBuf, OpenGate>>> = OnceLock::new();

fn open_gate(db_path: &Path) -> OpenGate {
    OPEN_GATES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(db_path.to_path_buf())
        .or_default()
        .clone()
}

/// Open (creating if missing) a workspace's `data.sqlite`, apply any
/// pending migrations, then run recovery sweeps so the UI never shows
/// orphaned `running` rows from a previous crashed app process.
///
/// Opens of the same file are serialized, and recovery runs only on the
/// first open in this process: the eager startup fan-out and a lazy open can
/// both open a workspace, and a second sweep would fail runs and tasks this
/// process already has in flight. `roster` names the agents in the result
/// events recovery posts.
pub(crate) async fn init_workspace_db(
    workspace_root: &Path,
    roster: &[WorkspaceAgentRow],
) -> Result<DbPool, String> {
    let db_path = crate::config::workspace_config::data_path(workspace_root);
    let parent = db_path
        .parent()
        .ok_or_else(|| "Could not determine workspace DB directory".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create workspace DB directory: {}", e))?;
    let gate = open_gate(&db_path);
    let mut recovered = gate.lock().await;

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

    if !*recovered {
        crate::assistant::tools::workspace_tasks::recover_interrupted_tasks(&pool, roster).await?;
        crate::assistant::repository::recover_stale_runs(&pool).await?;
        *recovered = true;
    }

    Ok(pool)
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
        let _pool = init_workspace_db(tmp.path(), &[]).await.unwrap();
        // Second open of the same workspace must succeed and not double-
        // apply migrations. sqlx::migrate! tracks via _sqlx_migrations.
        let _pool = init_workspace_db(tmp.path(), &[]).await.unwrap();
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

    /// `20261016001000_message_origin_task_lineage` marks the creator's
    /// instruction — the first user row of a task session — and nothing else.
    #[tokio::test]
    async fn backfill_marks_first_instruction_only() {
        let pool = pool_migrated_before(20261016001000).await;
        for session in ["task-session", "manager-session"] {
            sqlx::query(
                "INSERT INTO assistant_sessions (id, context_json, created_at, updated_at)
                 VALUES (?, '{}', 1, 1)",
            )
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO workspace_tasks
                 (id, created_by_workspace_agent_id, assigned_to_workspace_agent_id,
                  assigned_agent_definition_id, title, instructions, status, session_id,
                  created_at, updated_at)
             VALUES ('t', 'wa-mgr', 'wa-1', 'def-1', 'T', 'I', 'completed', 'task-session', 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (id, session, role, created_at) in [
            ("instruction", "task-session", "user", 1),
            ("reply", "task-session", "assistant", 2),
            ("follow-up", "task-session", "user", 3),
            ("manager-user", "manager-session", "user", 1),
        ] {
            sqlx::query(
                "INSERT INTO assistant_messages (id, session_id, role, content_json, created_at)
                 VALUES (?, ?, ?, '[]', ?)",
            )
            .bind(id)
            .bind(session)
            .bind(role)
            .bind(created_at)
            .execute(&pool)
            .await
            .unwrap();
        }

        run_all_migrations(&pool).await;

        let origins: Vec<(String, String)> =
            sqlx::query_as("SELECT id, origin_json FROM assistant_messages ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        let user = r#"{"kind":"user"}"#;
        for (id, origin) in &origins {
            if id == "instruction" {
                let origin: crate::assistant::types::MessageOrigin =
                    serde_json::from_str(origin).unwrap();
                let crate::assistant::types::MessageOrigin::Agent { agent, .. } = origin else {
                    panic!("the instruction must be marked as the creator's: {origin:?}");
                };
                assert_eq!(agent.workspace_agent_id, "wa-mgr");
                assert_eq!(agent.display_name, "wa-mgr");
                assert!(!agent.is_default);
            } else {
                assert_eq!(origin, user, "{id} must stay a user row");
            }
        }
    }

    /// The same migration folds `blocked` into `failed{blocked}`, fails tasks
    /// left in flight by the old code, and gives every failed row a reason.
    #[tokio::test]
    async fn migration_backfills_blocked_rows_and_labels_restart_failures() {
        let pool = pool_migrated_before(20261016001000).await;
        for (id, status, error) in [
            ("blocked", "blocked", Some("BLOCKED: need a key")),
            ("restart", "failed", Some("task interrupted by app restart")),
            (
                "never-ran",
                "failed",
                Some("task never started: the app restarted before it was dispatched"),
            ),
            ("error", "failed", Some("provider exploded")),
            ("done", "completed", None),
            ("in-flight-queued", "queued", None),
            ("in-flight-running", "running", None),
        ] {
            insert_task(&pool, id, status, None, error).await;
        }

        run_all_migrations(&pool).await;

        let rows: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT id, status, failure_reason FROM workspace_tasks ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        let expected = [
            ("blocked", "failed", Some("blocked")),
            ("done", "completed", None),
            ("error", "failed", Some("error")),
            ("in-flight-queued", "failed", Some("restart")),
            ("in-flight-running", "failed", Some("restart")),
            ("never-ran", "failed", Some("restart")),
            ("restart", "failed", Some("restart")),
        ];
        let rows: Vec<(&str, &str, Option<&str>)> = rows
            .iter()
            .map(|(id, status, reason)| (id.as_str(), status.as_str(), reason.as_deref()))
            .collect();
        assert_eq!(rows, expected);
    }

    /// `20261014000000_drop_dead_columns` leaves the rows it rewrites loadable.
    #[tokio::test]
    async fn dead_columns_drop_keeps_rows_loadable() {
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
        sqlx::query(
            "INSERT INTO workspace_tasks
                 (id, assigned_to_workspace_agent_id, assigned_agent_definition_id, title,
                  instructions, status, result_summary, result_json, created_at, updated_at,
                  user_response, user_response_at)
             VALUES ('answered', 'wa', 'def', 't', 'i', 'blocked', 'done',
                     '{\"summary\":\"done\"}', 1, 1, 'yes', 5)",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_all_migrations(&pool).await;

        assert!(repository::get_session(&pool, "s").await.unwrap().is_some());
        assert!(repository::get_run(&pool, "r").await.unwrap().is_some());
        assert!(
            crate::assistant::workspace_tasks_repo::load_task(&pool, "answered")
                .await
                .unwrap()
                .is_some()
        );
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
        assert!(repository::active_run_id(&pool, "s")
            .await
            .unwrap()
            .is_some());
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

    /// A workspace DB as a previous process left it: migrated, never
    /// recovered by this one.
    async fn previous_process_db(root: &Path) -> DbPool {
        let db_path = crate::config::workspace_config::data_path(root);
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        run_all_migrations(&pool).await;
        pool
    }

    /// `init_workspace_db` must run the sweep itself: the eager startup fan-out
    /// and the lazy-open path both go through it and nothing else sweeps.
    #[tokio::test]
    async fn workspace_init_sweeps_orphaned_task_state() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = previous_process_db(tmp.path()).await;
        insert_task(&pool, "t1", "queued", None, None).await;
        insert_task(&pool, "t2", "running", None, None).await;
        sqlx::query("UPDATE workspace_tasks SET run_id = 'run-2' WHERE id = 't2'")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let pool = init_workspace_db(tmp.path(), &[]).await.unwrap();
        let statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM workspace_tasks ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        // Queued with no session: never dispatchable. Running: interrupted.
        assert_eq!(statuses, vec!["failed".to_string(), "stopped".to_string()]);
    }

    /// Without the gate, concurrent opens of a fresh file race each other's
    /// migrations.
    #[tokio::test]
    async fn init_gate_serializes_concurrent_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let mut opens = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let root = tmp.path().to_path_buf();
            opens.spawn(async move { init_workspace_db(&root, &[]).await });
        }
        while let Some(opened) = opens.join_next().await {
            opened.unwrap().expect("every concurrent open must succeed");
        }
    }

    /// The eager startup open and a lazy open of the same workspace: the
    /// second must not fail the run and task the first one's process owns.
    #[tokio::test]
    async fn startup_open_does_not_sweep_live_run() {
        use crate::assistant::repository::{self, CreateRunParams};
        use crate::assistant::types::{RunStatus, RunTrigger, SessionContext};

        let tmp = tempfile::tempdir().unwrap();
        let pool = init_workspace_db(tmp.path(), &[]).await.unwrap();
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();
        let run = repository::create_run(
            &pool,
            CreateRunParams {
                session_id: session.id.clone(),
                status: RunStatus::Running,
                trigger: RunTrigger::UserMessage,
                connection_id: "c".into(),
                protocol_id: "openai".into(),
                model_id: "m".into(),
                error: None,
            },
        )
        .await
        .unwrap();
        insert_task(&pool, "t1", "running", Some(&session.id), None).await;

        let second = init_workspace_db(tmp.path(), &[]).await.unwrap();

        let stored = repository::get_run(&second, &run.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, RunStatus::Running);
        let status: String = sqlx::query_scalar("SELECT status FROM workspace_tasks")
            .fetch_one(&second)
            .await
            .unwrap();
        assert_eq!(status, "running");
    }

    #[tokio::test]
    async fn migrated_queue_rows_become_user_message_signals() {
        let pool = pool_migrated_before(20261016000000).await;
        for statement in [
            "INSERT INTO assistant_sessions (id, context_json, created_at, updated_at)
             VALUES ('s', '{}', 1, 1)",
            "INSERT INTO assistant_messages (id, session_id, role, content_json, created_at)
             VALUES ('m1', 's', 'user', '[]', 1), ('m2', 's', 'user', '[]', 2)",
            "INSERT INTO assistant_runs
                 (id, session_id, status, \"trigger\", connection_id, protocol_id, model_id, started_at)
             VALUES ('r', 's', 'completed', 'user_message', 'c', 'openai', 'm', 1)",
            "INSERT INTO assistant_message_queue
                 (message_id, session_id, connection_id, status, queued_at, delivered_run_id, delivered_at)
             VALUES ('m1', 's', 'c1', 'pending', 5, NULL, NULL),
                    ('m2', 's', 'c2', 'delivered', 6, 'r', 9)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }

        run_all_migrations(&pool).await;

        let signals: Vec<String> = sqlx::query_scalar(
            "SELECT id || ' ' || target_session_id || ' ' || kind || ' '
                    || message_id || ' ' || connection_id || ' ' || status || ' '
                    || COALESCE(consumed_run_id, '-') || ' ' || created_at
             FROM session_signals ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            signals,
            [
                "q:m1 s user_message m1 c1 pending - 5",
                "q:m2 s user_message m2 c2 consumed r 6",
            ]
        );
        let queue_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'assistant_message_queue'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(queue_left, 0);
    }

    /// The migration closes all but the newest active run of a session so
    /// the one-active-run index can be created.
    #[tokio::test]
    async fn session_signals_migration_keeps_only_the_newest_active_run() {
        let pool = pool_migrated_before(20261016000000).await;
        for statement in [
            "INSERT INTO assistant_sessions (id, context_json, created_at, updated_at)
             VALUES ('s', '{}', 1, 1)",
            "INSERT INTO assistant_runs
                 (id, session_id, status, \"trigger\", connection_id, protocol_id, model_id, started_at)
             VALUES ('old', 's', 'running', 'user_message', 'c', 'openai', 'm', 1),
                    ('new', 's', 'queued', 'user_message', 'c', 'openai', 'm', 2)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }

        run_all_migrations(&pool).await;

        let runs: Vec<(String, String, Option<i64>)> =
            sqlx::query_as("SELECT id, status, completed_at FROM assistant_runs ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            runs,
            vec![
                ("new".to_string(), "queued".to_string(), None),
                ("old".to_string(), "failed".to_string(), Some(1)),
            ]
        );
    }

    #[tokio::test]
    async fn active_run_index_matches_active_run_statuses() {
        let (_tmp, pool) = workspace_pool().await;
        let sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_assistant_runs_one_active_per_session'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let predicate = sql
            .split_once("WHERE status IN ")
            .map(|(_, statuses)| statuses.trim())
            .expect("the index is partial on run status");
        assert_eq!(predicate, crate::assistant::repository::ACTIVE_RUN_STATUSES);
    }
}

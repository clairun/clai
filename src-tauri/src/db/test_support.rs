//! Shared test fixtures for code that talks to a workspace database.
//!
//! Every test pool here is the **production** schema: a real
//! `<tempdir>/.clai/data.sqlite` opened through [`init_workspace_db`], which
//! applies the embedded `migrations/workspace/` files. Hand-writing
//! `CREATE TABLE` statements in a test module is the thing this module
//! exists to prevent — a copied schema silently drifts from the migrations
//! (missing foreign keys, phantom columns) and the tests keep passing
//! against a database production never had.

use crate::db::{init_workspace_db, DbPool};
use tempfile::TempDir;

/// Opens a fresh workspace database in a tempdir and returns it with the
/// directory that owns it.
///
/// Keep the returned [`TempDir`] alive for as long as the pool is used: it
/// deletes the directory — and the SQLite file inside it — when dropped.
///
/// The pool comes back **post-recovery**: `init_workspace_db` runs
/// `sweep_orphaned_task_state` and `recover_stale_runs` at open time. That is
/// harmless on the empty database a test starts from, but do not re-open the
/// same path with fixture data in flight — the second open would fail every
/// `queued`/`running` row the test just seeded.
pub(crate) async fn workspace_pool() -> (TempDir, DbPool) {
    let tmp = tempfile::tempdir().expect("failed to create tempdir for test workspace");
    let pool = init_workspace_db(tmp.path())
        .await
        .expect("failed to initialise test workspace DB");
    (tmp, pool)
}

/// Inserts a `workspace_tasks` row, filling every NOT NULL column the real
/// schema declares. One builder for all test modules: when the table gains a
/// column, exactly one place needs to change.
pub(crate) async fn insert_task(
    pool: &DbPool,
    id: &str,
    status: &str,
    session_id: Option<&str>,
    error: Option<&str>,
) {
    sqlx::query(
        r#"
        INSERT INTO workspace_tasks
            (id, created_by_workspace_agent_id, assigned_to_workspace_agent_id,
             assigned_agent_definition_id, title, instructions, status, error,
             session_id, created_at, updated_at)
        VALUES (?, NULL, 'agent-1', 'agent-1', 'Title', 'Do it', ?, ?, ?, 1, 1)
        "#,
    )
    .bind(id)
    .bind(status)
    .bind(error)
    .bind(session_id)
    .execute(pool)
    .await
    .expect("failed to insert workspace_tasks row");
}

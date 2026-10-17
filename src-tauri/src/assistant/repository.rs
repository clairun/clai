use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};
use uuid::Uuid;

use crate::assistant::types::{
    AssistantCompaction, AssistantMessage, AssistantRun, AssistantSession, CompactionStatus,
    CompactionStrategy, CompactionTrigger, ContentPart, MessageOrigin, MessageRole, RunStatus,
    RunTrigger, SessionContext, ToolCallStatus, ToolInvocation,
};
use crate::db::{enum_text, parse_enum_text, DbPool};

pub struct CreateMessageParams {
    pub session_id: String,
    pub role: MessageRole,
    pub content: Vec<ContentPart>,
    pub provider_metadata: Option<serde_json::Value>,
    /// The run writing the row; `None` for rows written outside a run.
    pub run_id: Option<String>,
}

pub struct CreateRunParams {
    pub session_id: String,
    pub status: RunStatus,
    pub trigger: RunTrigger,
    pub connection_id: String,
    pub protocol_id: String,
    pub model_id: String,
    pub error: Option<String>,
}

pub struct CreateToolCallParams {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub tool_name: String,
    pub params: serde_json::Value,
    pub status: ToolCallStatus,
}

pub struct CreateCompactionParams {
    pub session_id: String,
    pub trigger: CompactionTrigger,
    pub strategy: CompactionStrategy,
    pub source_from_message_id: Option<String>,
    pub source_to_message_id: Option<String>,
    pub created_run_id: Option<String>,
    pub protocol_id: String,
    pub model_id: String,
    pub input_message_count: i64,
}

pub(crate) fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn to_json_string<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("Failed to serialize JSON: {}", e))
}

fn parse_json<T: DeserializeOwned>(value: &str, context: &str) -> Result<T, String> {
    serde_json::from_str(value).map_err(|e| format!("Failed to parse {}: {}", context, e))
}

/// Non-terminal run statuses, for `status IN` clauses. The predicate of
/// `idx_assistant_runs_one_active_per_session` is a SQL copy of this list.
pub(crate) const ACTIVE_RUN_STATUSES: &str = "('queued', 'running', 'waiting_for_tool')";

fn parse_optional_json<T: DeserializeOwned>(
    value: Option<String>,
    context: &str,
) -> Result<Option<T>, String> {
    value.map(|json| parse_json(&json, context)).transpose()
}

fn map_session_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssistantSession, String> {
    Ok(AssistantSession {
        id: row.get("id"),
        context: parse_json::<SessionContext>(
            &row.get::<String, _>("context_json"),
            "session context",
        )?,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

pub(crate) fn map_message_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssistantMessage, String> {
    Ok(AssistantMessage {
        id: row.get("id"),
        session_id: row.get("session_id"),
        role: parse_enum_text::<MessageRole>(row.get::<String, _>("role"), "message role")?,
        content: parse_json::<Vec<ContentPart>>(
            &row.get::<String, _>("content_json"),
            "message content",
        )?,
        created_at: row.get("created_at"),
        provider_metadata: parse_optional_json::<serde_json::Value>(
            row.get("provider_metadata_json"),
            "provider metadata",
        )?,
        origin: parse_json(&row.get::<String, _>("origin_json"), "message origin")?,
    })
}

fn map_run_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssistantRun, String> {
    Ok(AssistantRun {
        id: row.get("id"),
        session_id: row.get("session_id"),
        status: parse_enum_text::<RunStatus>(row.get::<String, _>("status"), "run status")?,
        trigger: parse_enum_text::<RunTrigger>(row.get::<String, _>("trigger"), "run trigger")?,
        connection_id: row.get("connection_id"),
        protocol_id: row.get("protocol_id"),
        model_id: row.get("model_id"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        error: row.get("error"),
    })
}

fn map_compaction_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssistantCompaction, String> {
    Ok(AssistantCompaction {
        id: row.get("id"),
        session_id: row.get("session_id"),
        trigger: parse_enum_text::<CompactionTrigger>(
            row.get::<String, _>("trigger"),
            "compaction trigger",
        )?,
        strategy: parse_enum_text::<CompactionStrategy>(
            row.get::<String, _>("strategy"),
            "compaction strategy",
        )?,
        status: parse_enum_text::<CompactionStatus>(
            row.get::<String, _>("status"),
            "compaction status",
        )?,
        source_from_message_id: row.get("source_from_message_id"),
        source_to_message_id: row.get("source_to_message_id"),
        summary_message_id: row.get("summary_message_id"),
        created_run_id: row.get("created_run_id"),
        protocol_id: row.get("protocol_id"),
        model_id: row.get("model_id"),
        input_message_count: row.get("input_message_count"),
        created_at: row.get("created_at"),
        completed_at: row.get("completed_at"),
        error: row.get("error"),
    })
}

pub async fn create_session(
    pool: &DbPool,
    context: SessionContext,
) -> Result<AssistantSession, String> {
    let now = now_ms();

    let session = AssistantSession {
        id: Uuid::new_v4().to_string(),
        context,
        created_at: now,
        updated_at: now,
    };

    sqlx::query(
        r#"
        INSERT INTO assistant_sessions
            (id, context_json, created_at, updated_at)
        VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(&session.id)
    .bind(to_json_string(&session.context)?)
    .bind(session.created_at)
    .bind(session.updated_at)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant session: {}", e))?;

    Ok(session)
}

/// Creates a session and records its id on the given `workspace_tasks` row.
/// Call inside the assignment's transaction: a concurrent
/// `find_manager_session` then never sees the session unlinked, which would
/// make a manager→manager task session pass for the manager conversation.
pub async fn create_session_and_link_task(
    conn: &mut SqliteConnection,
    context: SessionContext,
    task_id: &str,
) -> Result<AssistantSession, String> {
    let now = now_ms();
    let session = AssistantSession {
        id: Uuid::new_v4().to_string(),
        context,
        created_at: now,
        updated_at: now,
    };

    sqlx::query(
        r#"
        INSERT INTO assistant_sessions
            (id, context_json, created_at, updated_at)
        VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(&session.id)
    .bind(to_json_string(&session.context)?)
    .bind(session.created_at)
    .bind(session.updated_at)
    .execute(&mut *conn)
    .await
    .map_err(|e| format!("Failed to create assistant session: {e}"))?;

    sqlx::query(
        r#"
        UPDATE workspace_tasks SET session_id = ? WHERE id = ?
        "#,
    )
    .bind(&session.id)
    .bind(task_id)
    .execute(&mut *conn)
    .await
    .map_err(|e| format!("Failed to link session to workspace task: {e}"))?;

    Ok(session)
}

pub async fn get_session(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<AssistantSession>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, context_json, created_at, updated_at
        FROM assistant_sessions
        WHERE id = ?
        "#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load assistant session: {}", e))?;

    row.as_ref().map(map_session_row).transpose()
}

/// When any session in this workspace database was last touched (a message,
/// a run start, or an explicit touch), or `None` when it has no sessions.
pub async fn last_session_activity(pool: &DbPool) -> Result<Option<i64>, String> {
    sqlx::query_scalar("SELECT MAX(updated_at) FROM assistant_sessions")
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to read session activity: {}", e))
}

/// Canonical conversation session for a workspace manager: the newest session
/// whose `automationId` is `manager_id`, excluding sessions
/// linked from `workspace_tasks.session_id` (a self-assigned task carries the
/// manager's automation id). `NOT EXISTS` keeps task rows with a NULL
/// `session_id` harmless. Ties on `updated_at` are broken by `id DESC`.
pub async fn find_manager_session(
    pool: &DbPool,
    manager_id: &str,
) -> Result<Option<AssistantSession>, String> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| format!("Failed to acquire manager session connection: {e}"))?;
    find_manager_session_on_connection(&mut conn, manager_id).await
}

pub(crate) async fn find_manager_session_on_connection(
    conn: &mut SqliteConnection,
    manager_id: &str,
) -> Result<Option<AssistantSession>, String> {
    let row = sqlx::query(
        r#"
        SELECT s.id, s.context_json, s.created_at, s.updated_at
        FROM assistant_sessions AS s
        WHERE json_extract(s.context_json, '$.automationId') = ?
          AND NOT EXISTS (
              SELECT 1 FROM workspace_tasks AS t WHERE t.session_id = s.id
          )
        ORDER BY s.updated_at DESC, s.id DESC
        LIMIT 1
        "#,
    )
    .bind(manager_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| format!("Failed to find manager session: {}", e))?;

    row.as_ref().map(map_session_row).transpose()
}

/// The manager's canonical session, created on first use. An existing row is
/// returned untouched: the agent's configuration is resolved per run, so there
/// is nothing to refresh, and a write here could clobber a CLI session id saved
/// by a concurrent run.
pub async fn find_or_create_manager_session(
    pool: &DbPool,
    manager_id: &str,
) -> Result<AssistantSession, String> {
    if let Some(session) = find_manager_session(pool, manager_id).await? {
        return Ok(session);
    }
    create_session(
        pool,
        SessionContext {
            automation_id: Some(manager_id.to_string()),
            ..SessionContext::default()
        },
    )
    .await
}

pub async fn delete_session(pool: &DbPool, session_id: &str) -> Result<bool, String> {
    let result = sqlx::query("DELETE FROM assistant_sessions WHERE id = ?")
        .bind(session_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete assistant session: {}", e))?;

    Ok(result.rows_affected() > 0)
}

pub async fn update_session(
    pool: &DbPool,
    session: &AssistantSession,
) -> Result<AssistantSession, String> {
    sqlx::query(
        r#"
        UPDATE assistant_sessions
        SET context_json = ?, updated_at = ?
        WHERE id = ?
        "#,
    )
    .bind(to_json_string(&session.context)?)
    .bind(session.updated_at)
    .bind(&session.id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update assistant session: {}", e))?;

    Ok(session.clone())
}

pub async fn list_messages(
    pool: &DbPool,
    session_id: &str,
) -> Result<Vec<AssistantMessage>, String> {
    let rows = sqlx::query(
        r#"
        SELECT id, session_id, role, content_json, provider_metadata_json, origin_json, created_at
        FROM assistant_messages
        WHERE session_id = ?
        ORDER BY created_at ASC, rowid ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load assistant messages: {}", e))?;

    rows.iter().map(map_message_row).collect()
}

/// The rows `run` wrote, oldest first. Rows written before messages carried
/// a run id have none; for a run with no attributed rows, those unattributed
/// rows inside the run's time window stand in.
pub async fn list_messages_for_run(
    pool: &DbPool,
    run: &AssistantRun,
) -> Result<Vec<AssistantMessage>, String> {
    let rows = sqlx::query(
        r#"
        SELECT id, session_id, role, content_json, provider_metadata_json, origin_json, created_at
        FROM assistant_messages
        WHERE session_id = ? AND run_id = ?
        ORDER BY created_at ASC, rowid ASC
        "#,
    )
    .bind(&run.session_id)
    .bind(&run.id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load run messages: {}", e))?;
    if !rows.is_empty() {
        return rows.iter().map(map_message_row).collect();
    }

    let rows = sqlx::query(
        r#"
        SELECT id, session_id, role, content_json, provider_metadata_json, origin_json, created_at
        FROM assistant_messages
        WHERE session_id = ? AND run_id IS NULL AND created_at >= ? AND created_at <= ?
        ORDER BY created_at ASC, rowid ASC
        "#,
    )
    .bind(&run.session_id)
    .bind(run.started_at)
    .bind(run.completed_at.unwrap_or_else(now_ms))
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load run messages: {}", e))?;
    rows.iter().map(map_message_row).collect()
}

pub async fn list_messages_before(
    pool: &DbPool,
    session_id: &str,
    before: Option<(i64, &str)>,
    limit: i64,
) -> Result<Vec<AssistantMessage>, String> {
    let rows = if let Some((before_created_at, before_message_id)) = before {
        sqlx::query(
            r#"
            SELECT id, session_id, role, content_json, provider_metadata_json, origin_json, created_at
            FROM assistant_messages
            WHERE session_id = ?
              AND (created_at < ? OR (created_at = ? AND id < ?))
            ORDER BY created_at DESC, id DESC
            LIMIT ?
            "#,
        )
        .bind(session_id)
        .bind(before_created_at)
        .bind(before_created_at)
        .bind(before_message_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to load assistant message page: {}", e))?
    } else {
        sqlx::query(
            r#"
            SELECT id, session_id, role, content_json, provider_metadata_json, origin_json, created_at
            FROM assistant_messages
            WHERE session_id = ?
            ORDER BY created_at DESC, id DESC
            LIMIT ?
            "#,
        )
        .bind(session_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to load assistant message page: {}", e))?
    };

    rows.iter().map(map_message_row).collect()
}

pub async fn count_session_messages(pool: &DbPool, session_id: &str) -> Result<u32, String> {
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM assistant_messages WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to count assistant messages: {}", e))?;

    Ok(count.max(0) as u32)
}

pub async fn get_message(
    pool: &DbPool,
    message_id: &str,
) -> Result<Option<AssistantMessage>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, session_id, role, content_json, provider_metadata_json, origin_json, created_at
        FROM assistant_messages
        WHERE id = ?
        "#,
    )
    .bind(message_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load assistant message: {}", e))?;

    row.as_ref().map(map_message_row).transpose()
}

/// Permanently remove a message row and, in the same transaction, its
/// `user_message` signal. Used to discard run input that never got an
/// answer — a user message (and the empty assistant placeholder) of a run
/// that failed before the provider produced anything.
pub async fn delete_message(pool: &DbPool, message_id: &str) -> Result<(), String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start message delete transaction: {}", e))?;

    sqlx::query("DELETE FROM session_signals WHERE message_id = ? AND kind = 'user_message'")
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to delete message signal: {}", e))?;

    sqlx::query("DELETE FROM assistant_messages WHERE id = ?")
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to delete assistant message: {}", e))?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit message delete transaction: {}", e))?;
    Ok(())
}

pub async fn create_message(
    pool: &DbPool,
    params: CreateMessageParams,
) -> Result<AssistantMessage, String> {
    let message = AssistantMessage {
        id: Uuid::new_v4().to_string(),
        session_id: params.session_id,
        role: params.role,
        content: params.content,
        created_at: now_ms(),
        provider_metadata: params.provider_metadata,
        origin: MessageOrigin::User,
    };

    sqlx::query(
        r#"
        INSERT INTO assistant_messages
            (id, session_id, role, content_json, provider_metadata_json, run_id, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&message.id)
    .bind(&message.session_id)
    .bind(enum_text(&message.role)?)
    .bind(to_json_string(&message.content)?)
    .bind(
        message
            .provider_metadata
            .as_ref()
            .map(to_json_string)
            .transpose()?,
    )
    .bind(params.run_id)
    .bind(message.created_at)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant message: {}", e))?;

    touch_session(pool, &message.session_id).await?;

    Ok(message)
}

/// Text-only convenience wrapper around [`create_user_message_with_content`].
/// Test-only: production code builds content explicitly so it can attach images.
#[cfg(test)]
pub async fn create_user_message(
    pool: &DbPool,
    session_id: String,
    text: String,
    queue_connection_id: Option<&str>,
) -> Result<AssistantMessage, String> {
    create_user_message_with_content(
        pool,
        session_id,
        vec![ContentPart::Text { text }],
        queue_connection_id,
    )
    .await
}

/// Like [`create_user_message`] but with an explicit content vec, so the caller
/// can attach images (or other parts) alongside the text. The text-only helper
/// delegates here.
pub async fn create_user_message_with_content(
    pool: &DbPool,
    session_id: String,
    content: Vec<ContentPart>,
    queue_connection_id: Option<&str>,
) -> Result<AssistantMessage, String> {
    let now = now_ms();
    let message = AssistantMessage {
        id: Uuid::new_v4().to_string(),
        session_id,
        role: MessageRole::User,
        content,
        created_at: now,
        provider_metadata: None,
        origin: MessageOrigin::User,
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start assistant message transaction: {}", e))?;

    sqlx::query(
        r#"
        INSERT INTO assistant_messages
            (id, session_id, role, content_json, provider_metadata_json, created_at)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&message.id)
    .bind(&message.session_id)
    .bind(enum_text(&message.role)?)
    .bind(to_json_string(&message.content)?)
    .bind(Option::<String>::None)
    .bind(message.created_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to create assistant message: {}", e))?;

    if let Some(connection_id) = queue_connection_id {
        crate::assistant::signals::insert_user_message_signal(&mut tx, &message, connection_id)
            .await?;
    }

    sqlx::query("UPDATE assistant_sessions SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(&message.session_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to update assistant session timestamp: {}", e))?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit assistant message transaction: {}", e))?;

    Ok(message)
}

/// Insert a run on `conn` and touch its session. `Ok(None)` when the session
/// already has an active run: `idx_assistant_runs_one_active_per_session`
/// rejects the insert, which makes this the arbiter for "one active run".
pub(crate) async fn insert_run_if_idle(
    conn: &mut SqliteConnection,
    params: CreateRunParams,
) -> Result<Option<AssistantRun>, String> {
    let run = AssistantRun {
        id: Uuid::new_v4().to_string(),
        session_id: params.session_id,
        status: params.status,
        trigger: params.trigger,
        connection_id: params.connection_id,
        protocol_id: params.protocol_id,
        model_id: params.model_id,
        started_at: now_ms(),
        completed_at: None,
        error: params.error,
    };

    let inserted = sqlx::query(
        r#"
        INSERT INTO assistant_runs
            (id, session_id, status, trigger, connection_id, protocol_id, model_id, error, started_at, completed_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&run.id)
    .bind(&run.session_id)
    .bind(enum_text(&run.status)?)
    .bind(enum_text(&run.trigger)?)
    .bind(&run.connection_id)
    .bind(&run.protocol_id)
    .bind(&run.model_id)
    .bind(&run.error)
    .bind(run.started_at)
    .bind(run.completed_at)
    .execute(&mut *conn)
    .await;
    match inserted {
        Err(e)
            if e.as_database_error()
                .is_some_and(|e| e.is_unique_violation()) =>
        {
            return Ok(None)
        }
        Err(e) => return Err(format!("Failed to create assistant run: {}", e)),
        Ok(_) => {}
    }

    sqlx::query("UPDATE assistant_sessions SET updated_at = ? WHERE id = ?")
        .bind(run.started_at)
        .bind(&run.session_id)
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("Failed to update assistant session timestamp: {}", e))?;

    Ok(Some(run))
}

/// See [`insert_run_if_idle`].
pub async fn create_run_if_idle(
    pool: &DbPool,
    params: CreateRunParams,
) -> Result<Option<AssistantRun>, String> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| format!("Failed to acquire database connection: {}", e))?;
    insert_run_if_idle(&mut conn, params).await
}

pub async fn create_run(pool: &DbPool, params: CreateRunParams) -> Result<AssistantRun, String> {
    let session_id = params.session_id.clone();
    create_run_if_idle(pool, params)
        .await?
        .ok_or_else(|| format!("Assistant session {session_id} already has an active run"))
}

/// At most two rows, newest first: the newest run and, if different, the
/// newest non-terminal run. Older history is never returned.
pub async fn current_runs(pool: &DbPool, session_id: &str) -> Result<Vec<AssistantRun>, String> {
    let latest = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, protocol_id, model_id, error, started_at, completed_at
        FROM assistant_runs
        WHERE session_id = ?
        ORDER BY started_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load latest assistant run: {}", e))?;
    let Some(latest) = latest.as_ref().map(map_run_row).transpose()? else {
        return Ok(Vec::new());
    };

    let active = get_active_run(pool, session_id).await?;
    let mut runs = vec![latest];
    if let Some(active) = active.filter(|active| active.id != runs[0].id) {
        runs.push(active);
    }
    Ok(runs)
}

/// The session's non-terminal run (queued, running, or waiting for a tool),
/// if any. Advisory: `insert_run_if_idle` is the authoritative check.
pub async fn active_run_id(pool: &DbPool, session_id: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar(&format!(
        r#"
        SELECT id
        FROM assistant_runs
        WHERE session_id = ?
          AND status IN {ACTIVE_RUN_STATUSES}
        LIMIT 1
        "#
    ))
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to check for active runs: {}", e))
}

/// Whether the workspace database contains any non-terminal run. Workspace
/// deletion uses this as a coarse guard before removing the root directory.
pub async fn workspace_has_active_run(pool: &DbPool) -> Result<bool, String> {
    Ok(!workspace_active_run_ids(pool).await?.is_empty())
}

/// Queued or running runs in any session of this workspace database.
pub async fn workspace_active_run_ids(pool: &DbPool) -> Result<Vec<String>, String> {
    sqlx::query_scalar(&format!(
        r#"
        SELECT id
        FROM assistant_runs
        WHERE status IN {ACTIVE_RUN_STATUSES}
        "#
    ))
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to check workspace for active runs: {}", e))
}

pub async fn get_active_run(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<AssistantRun>, String> {
    let sql = format!(
        r#"
        SELECT id, session_id, status, trigger, connection_id, protocol_id, model_id, error, started_at, completed_at
        FROM assistant_runs
        WHERE session_id = ?
          AND status IN {ACTIVE_RUN_STATUSES}
        ORDER BY started_at DESC, id DESC
        LIMIT 1
        "#
    );
    let row = sqlx::query(&sql)
        .bind(session_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to load active run: {}", e))?;

    row.as_ref().map(map_run_row).transpose()
}

/// The provider connection of the session's most recent run.
pub async fn latest_run_connection_id(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<String>, String> {
    sqlx::query_scalar(
        "SELECT connection_id FROM assistant_runs WHERE session_id = ?
         ORDER BY started_at DESC, rowid DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load latest run connection: {}", e))
}

pub async fn get_run(pool: &DbPool, run_id: &str) -> Result<Option<AssistantRun>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, protocol_id, model_id, error, started_at, completed_at
        FROM assistant_runs
        WHERE id = ?
        "#,
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load assistant run: {}", e))?;

    row.as_ref().map(map_run_row).transpose()
}

pub async fn update_run_status(
    pool: &DbPool,
    run_id: &str,
    status: RunStatus,
    error: Option<&str>,
) -> Result<AssistantRun, String> {
    let completed_at = status.is_terminal().then(now_ms);

    sqlx::query(
        r#"
        UPDATE assistant_runs
        SET status = ?, error = ?, completed_at = COALESCE(?, completed_at)
        WHERE id = ?
        "#,
    )
    .bind(enum_text(&status)?)
    .bind(error)
    .bind(completed_at)
    .bind(run_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update run status: {}", e))?;

    let row = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, protocol_id, model_id, error, started_at, completed_at
        FROM assistant_runs
        WHERE id = ?
        "#,
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to load updated run: {}", e))?;

    let run = map_run_row(&row)?;
    touch_session(pool, &run.session_id).await?;
    Ok(run)
}

pub async fn complete_run(
    pool: &DbPool,
    run_id: &str,
    status: RunStatus,
    error: Option<&str>,
) -> Result<AssistantRun, String> {
    let completed_at = now_ms();

    sqlx::query(
        r#"
        UPDATE assistant_runs
        SET status = ?, error = ?, completed_at = ?
        WHERE id = ?
        "#,
    )
    .bind(enum_text(&status)?)
    .bind(error)
    .bind(completed_at)
    .bind(run_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to complete run: {}", e))?;

    let row = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, protocol_id, model_id, error, started_at, completed_at
        FROM assistant_runs
        WHERE id = ?
        "#,
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to load completed run: {}", e))?;

    let run = map_run_row(&row)?;
    touch_session(pool, &run.session_id).await?;
    Ok(run)
}

pub async fn update_message_content(
    pool: &DbPool,
    message_id: &str,
    content: &[ContentPart],
) -> Result<AssistantMessage, String> {
    sqlx::query(
        r#"
        UPDATE assistant_messages
        SET content_json = ?
        WHERE id = ?
        "#,
    )
    .bind(to_json_string(content)?)
    .bind(message_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update message content: {}", e))?;

    let row = sqlx::query(
        r#"
        SELECT id, session_id, role, content_json, provider_metadata_json, origin_json, created_at
        FROM assistant_messages
        WHERE id = ?
        "#,
    )
    .bind(message_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to load updated message: {}", e))?;

    map_message_row(&row)
}

pub async fn create_compaction(
    pool: &DbPool,
    params: CreateCompactionParams,
) -> Result<AssistantCompaction, String> {
    let now = now_ms();
    let compaction = AssistantCompaction {
        id: Uuid::new_v4().to_string(),
        session_id: params.session_id,
        trigger: params.trigger,
        strategy: params.strategy,
        status: CompactionStatus::Running,
        source_from_message_id: params.source_from_message_id,
        source_to_message_id: params.source_to_message_id,
        summary_message_id: None,
        created_run_id: params.created_run_id,
        protocol_id: params.protocol_id,
        model_id: params.model_id,
        input_message_count: params.input_message_count,
        created_at: now,
        completed_at: None,
        error: None,
    };

    sqlx::query(
        r#"
        INSERT INTO assistant_compactions
            (id, session_id, trigger, strategy, status, source_from_message_id, source_to_message_id,
             summary_message_id, created_run_id, protocol_id, model_id, input_message_count,
             created_at, completed_at, error)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&compaction.id)
    .bind(&compaction.session_id)
    .bind(enum_text(&compaction.trigger)?)
    .bind(enum_text(&compaction.strategy)?)
    .bind(enum_text(&compaction.status)?)
    .bind(&compaction.source_from_message_id)
    .bind(&compaction.source_to_message_id)
    .bind(&compaction.summary_message_id)
    .bind(&compaction.created_run_id)
    .bind(&compaction.protocol_id)
    .bind(&compaction.model_id)
    .bind(compaction.input_message_count)
    .bind(compaction.created_at)
    .bind(compaction.completed_at)
    .bind(&compaction.error)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant compaction: {}", e))?;

    touch_session(pool, &compaction.session_id).await?;
    Ok(compaction)
}

pub async fn complete_compaction(
    pool: &DbPool,
    compaction_id: &str,
    summary_message_id: &str,
) -> Result<AssistantCompaction, String> {
    let completed_at = now_ms();
    sqlx::query(
        r#"
        UPDATE assistant_compactions
        SET status = ?, summary_message_id = ?, completed_at = ?, error = NULL
        WHERE id = ?
        "#,
    )
    .bind(enum_text(&CompactionStatus::Completed)?)
    .bind(summary_message_id)
    .bind(completed_at)
    .bind(compaction_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to complete assistant compaction: {}", e))?;

    let compaction = get_compaction(pool, compaction_id)
        .await?
        .ok_or_else(|| format!("Assistant compaction not found: {}", compaction_id))?;
    touch_session(pool, &compaction.session_id).await?;
    Ok(compaction)
}

pub async fn get_compaction(
    pool: &DbPool,
    compaction_id: &str,
) -> Result<Option<AssistantCompaction>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, session_id, trigger, strategy, status, source_from_message_id,
               source_to_message_id, summary_message_id, created_run_id, protocol_id,
               model_id, input_message_count, created_at, completed_at, error
        FROM assistant_compactions
        WHERE id = ?
        "#,
    )
    .bind(compaction_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load assistant compaction: {}", e))?;

    row.as_ref().map(map_compaction_row).transpose()
}

pub async fn latest_completed_compaction(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<AssistantCompaction>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, session_id, trigger, strategy, status, source_from_message_id,
               source_to_message_id, summary_message_id, created_run_id, protocol_id,
               model_id, input_message_count, created_at, completed_at, error
        FROM assistant_compactions
        WHERE session_id = ?
          AND status = 'completed'
          AND summary_message_id IS NOT NULL
          AND source_to_message_id IS NOT NULL
        ORDER BY completed_at DESC, created_at DESC
        LIMIT 1
        "#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load latest assistant compaction: {}", e))?;

    row.as_ref().map(map_compaction_row).transpose()
}

fn map_tool_call_row(row: &sqlx::sqlite::SqliteRow) -> Result<ToolInvocation, String> {
    Ok(ToolInvocation {
        id: row.get("id"),
        run_id: row.get("run_id"),
        session_id: row.get("session_id"),
        tool_name: row.get("tool_name"),
        params: parse_json(&row.get::<String, _>("params_json"), "tool call params")?,
        status: parse_enum_text::<ToolCallStatus>(
            row.get::<String, _>("status"),
            "tool call status",
        )?,
        result: parse_optional_json(row.get("result_json"), "tool call result")?,
        result_summary: None,
        has_full_result: false,
        has_full_input: false,
        error: row.get("error"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
    })
}

pub async fn create_tool_call(
    pool: &DbPool,
    params: CreateToolCallParams,
) -> Result<ToolInvocation, String> {
    let tc = ToolInvocation {
        id: params.id,
        run_id: params.run_id,
        session_id: params.session_id,
        tool_name: params.tool_name,
        params: params.params,
        status: params.status,
        result: None,
        result_summary: None,
        has_full_result: false,
        has_full_input: false,
        error: None,
        started_at: now_ms(),
        completed_at: None,
    };

    sqlx::query(
        r#"
        INSERT INTO assistant_tool_calls
            (id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&tc.id)
    .bind(&tc.run_id)
    .bind(&tc.session_id)
    .bind(&tc.tool_name)
    .bind(to_json_string(&tc.params)?)
    .bind(enum_text(&tc.status)?)
    .bind::<Option<String>>(None)
    .bind::<Option<String>>(None)
    .bind(tc.started_at)
    .bind::<Option<i64>>(None)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create tool call: {}", e))?;

    Ok(tc)
}

pub async fn update_tool_call(
    pool: &DbPool,
    id: &str,
    status: ToolCallStatus,
    result: Option<&serde_json::Value>,
    error: Option<&str>,
) -> Result<ToolInvocation, String> {
    let is_terminal = matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed);
    let completed_at = if is_terminal { Some(now_ms()) } else { None };

    sqlx::query(
        r#"
        UPDATE assistant_tool_calls
        SET status = ?, result_json = ?, error = ?, completed_at = COALESCE(?, completed_at)
        WHERE id = ?
        "#,
    )
    .bind(enum_text(&status)?)
    .bind(result.map(to_json_string).transpose()?)
    .bind(error)
    .bind(completed_at)
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update tool call: {}", e))?;

    let row = sqlx::query(
        r#"
        SELECT id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at
        FROM assistant_tool_calls
        WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to load updated tool call: {}", e))?;

    map_tool_call_row(&row)
}

pub async fn get_tool_call_for_session(
    pool: &DbPool,
    session_id: &str,
    tool_call_id: &str,
) -> Result<Option<ToolInvocation>, String> {
    let row = sqlx::query(
        "SELECT id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at FROM assistant_tool_calls WHERE session_id = ? AND id = ?",
    )
    .bind(session_id)
    .bind(tool_call_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load assistant tool call: {}", e))?;
    row.as_ref().map(map_tool_call_row).transpose()
}

pub async fn list_tool_calls_by_ids(
    pool: &DbPool,
    tool_call_ids: &[String],
) -> Result<Vec<ToolInvocation>, String> {
    if tool_call_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut query = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at
        FROM assistant_tool_calls
        WHERE id IN (
        "#,
    );
    let mut separated = query.separated(", ");
    for tool_call_id in tool_call_ids {
        separated.push_bind(tool_call_id);
    }
    separated.push_unseparated(") ORDER BY started_at ASC");

    let rows = query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list assistant tool calls by id: {}", e))?;

    rows.iter().map(map_tool_call_row).collect()
}

pub async fn fail_running_tool_calls_for_run(
    pool: &DbPool,
    run_id: &str,
    error: &str,
) -> Result<Vec<ToolInvocation>, String> {
    let rows = sqlx::query(
        r#"
        SELECT id
        FROM assistant_tool_calls
        WHERE run_id = ? AND status IN ('pending', 'running')
        ORDER BY started_at ASC
        "#,
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list running tool calls: {}", e))?;

    let mut updated = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.get("id");
        updated.push(update_tool_call(pool, &id, ToolCallStatus::Failed, None, Some(error)).await?);
    }
    Ok(updated)
}

async fn touch_session(pool: &DbPool, session_id: &str) -> Result<(), String> {
    sqlx::query("UPDATE assistant_sessions SET updated_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(session_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update assistant session timestamp: {}", e))?;

    Ok(())
}

/// Recover stale runs and tool calls left in non-terminal states from a previous
/// crash or restart.  Marks them as failed so they don't appear stuck forever.
pub async fn recover_stale_runs(pool: &DbPool) -> Result<u64, String> {
    let now = now_ms();

    let run_result = sqlx::query(&format!(
        r#"
        UPDATE assistant_runs
        SET status = 'failed', error = 'App restarted while running', completed_at = ?
        WHERE status IN {ACTIVE_RUN_STATUSES}
        "#
    ))
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to recover stale runs: {}", e))?;

    let tc_result = sqlx::query(
        r#"
        UPDATE assistant_tool_calls
        SET status = 'failed', error = 'App restarted while running'
        WHERE status IN ('pending', 'running')
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to recover stale tool calls: {}", e))?;

    let total = run_result.rows_affected() + tc_result.rows_affected();
    if total > 0 {
        tracing::info!(
            "Recovered {} stale runs and {} stale tool calls from previous session",
            run_result.rows_affected(),
            tc_result.rows_affected()
        );
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::workspace_pool;

    async fn insert_session(pool: &DbPool, id: &str) {
        sqlx::query(
            r#"
            INSERT INTO assistant_sessions
                (id, context_json, created_at, updated_at)
            VALUES (?, '{}', 1, 1)
            "#,
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn queue_rows_for(pool: &DbPool, message_id: &str) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_signals WHERE message_id = ?")
            .bind(message_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn delete_message_removes_row_and_signal() {
        let (_tmp, pool) = workspace_pool().await;
        insert_session(&pool, "s1").await;

        // Queued variant: the signal row must go with the message.
        let queued =
            create_user_message(&pool, "s1".to_string(), "hello".to_string(), Some("conn-1"))
                .await
                .unwrap();
        assert_eq!(queue_rows_for(&pool, &queued.id).await, 1);
        assert!(get_message(&pool, &queued.id).await.unwrap().is_some());

        delete_message(&pool, &queued.id).await.unwrap();
        assert!(get_message(&pool, &queued.id).await.unwrap().is_none());
        assert_eq!(queue_rows_for(&pool, &queued.id).await, 0);

        // Non-queued variant deletes cleanly too.
        let direct = create_user_message(&pool, "s1".to_string(), "again".to_string(), None)
            .await
            .unwrap();
        delete_message(&pool, &direct.id).await.unwrap();
        assert!(get_message(&pool, &direct.id).await.unwrap().is_none());

        // Deleting an unknown id is a no-op, not an error.
        delete_message(&pool, "missing").await.unwrap();
    }

    #[test]
    fn enum_text_round_trips_every_stored_enum_without_quotes() {
        fn check<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(values: &[T]) {
            for value in values {
                let text = enum_text(value).unwrap();
                assert!(!text.contains('"'), "{text}");
                assert_eq!(&parse_enum_text::<T>(text, "enum").unwrap(), value);
            }
        }
        check(&[
            MessageRole::System,
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::Tool,
        ]);
        check(&ALL_RUN_STATUSES);
        check(&[
            RunTrigger::UserMessage,
            RunTrigger::Scheduled,
            RunTrigger::ManualAutomation,
            RunTrigger::WorkspaceTask,
            RunTrigger::SignalWake,
        ]);
        check(&[
            ToolCallStatus::Pending,
            ToolCallStatus::Running,
            ToolCallStatus::Completed,
            ToolCallStatus::Failed,
        ]);
        check(&[
            CompactionTrigger::Manual,
            CompactionTrigger::Automatic,
            CompactionTrigger::ErrorRecovery,
        ]);
        check(&[
            CompactionStrategy::LocalSummary,
            CompactionStrategy::SessionRotationSummary,
        ]);
        check(&[
            CompactionStatus::Running,
            CompactionStatus::Completed,
            CompactionStatus::Failed,
        ]);
    }

    const ALL_RUN_STATUSES: [RunStatus; 7] = [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::WaitingForTool,
        RunStatus::Completed,
        RunStatus::CompletedWithWarnings,
        RunStatus::Failed,
        RunStatus::Cancelled,
    ];

    #[test]
    fn active_run_statuses_are_the_non_terminal_ones() {
        let active: Vec<String> = ALL_RUN_STATUSES
            .iter()
            .filter(|status| status.is_active())
            .map(|status| format!("'{}'", enum_text(status).unwrap()))
            .collect();
        assert_eq!(ACTIVE_RUN_STATUSES, format!("({})", active.join(", ")));
    }

    #[tokio::test]
    async fn enum_columns_are_written_as_plain_text() {
        let (_tmp, pool) = workspace_pool().await;
        insert_session(&pool, "s").await;
        let message = create_message(
            &pool,
            CreateMessageParams {
                run_id: None,
                session_id: "s".to_string(),
                role: MessageRole::User,
                content: Vec::new(),
                provider_metadata: None,
            },
        )
        .await
        .unwrap();
        let run = create_run(
            &pool,
            CreateRunParams {
                session_id: "s".to_string(),
                status: RunStatus::Queued,
                trigger: RunTrigger::UserMessage,
                connection_id: "c".to_string(),
                protocol_id: "openai".to_string(),
                model_id: "m".to_string(),
                error: None,
            },
        )
        .await
        .unwrap();
        update_run_status(&pool, &run.id, RunStatus::WaitingForTool, None)
            .await
            .unwrap();
        create_tool_call(
            &pool,
            CreateToolCallParams {
                id: "tc".to_string(),
                run_id: run.id.clone(),
                session_id: "s".to_string(),
                tool_name: "t".to_string(),
                params: serde_json::json!({}),
                status: ToolCallStatus::Pending,
            },
        )
        .await
        .unwrap();
        let compaction = create_compaction(
            &pool,
            CreateCompactionParams {
                session_id: "s".to_string(),
                trigger: CompactionTrigger::Manual,
                strategy: CompactionStrategy::LocalSummary,
                source_from_message_id: None,
                source_to_message_id: None,
                created_run_id: None,
                protocol_id: "openai".to_string(),
                model_id: "m".to_string(),
                input_message_count: 0,
            },
        )
        .await
        .unwrap();
        complete_compaction(&pool, &compaction.id, &message.id)
            .await
            .unwrap();

        let stored: (String, String, String, String) = sqlx::query_as(
            r#"
            SELECT
                (SELECT role FROM assistant_messages),
                (SELECT status || ' ' || "trigger" FROM assistant_runs),
                (SELECT status FROM assistant_tool_calls),
                (SELECT "trigger" || ' ' || strategy || ' ' || status FROM assistant_compactions)
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            stored,
            (
                "user".to_string(),
                "waiting_for_tool user_message".to_string(),
                "pending".to_string(),
                "manual local_summary completed".to_string(),
            )
        );
    }

    #[tokio::test]
    async fn latest_run_started_at_ties_break_by_highest_id() {
        let (_tmp, pool) = workspace_pool().await;
        insert_session(&pool, "s").await;
        // Higher id inserted first so insertion order alone cannot pass.
        for (id, status) in [("run-b", "completed"), ("run-a", "running")] {
            sqlx::query(
                r#"
                INSERT INTO assistant_runs
                    (id, session_id, status, "trigger", connection_id, protocol_id, model_id, started_at)
                VALUES (?, 's', ?, 'user_message', 'c', 'openai', 'm', 100)
                "#,
            )
            .bind(id)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        }

        let active = get_active_run(&pool, "s").await.unwrap().unwrap();
        assert_eq!(active.id, "run-a");
        let current: Vec<String> = current_runs(&pool, "s")
            .await
            .unwrap()
            .into_iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(current, vec!["run-b".to_string(), "run-a".to_string()]);
    }
}

use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use crate::assistant::types::{
    AssistantCompaction, AssistantMessage, AssistantRun, AssistantSession, CompactionStatus,
    CompactionStrategy, CompactionTrigger, ContentPart, MessageRole, RunNotice, RunStatus,
    RunTrigger, RunUsage, SessionContext, SessionKind, ToolCallStatus, ToolInvocation,
};
use crate::db::DbPool;

pub struct CreateSessionParams {
    pub kind: SessionKind,
    pub title: Option<String>,
    pub context: SessionContext,
}

pub struct CreateMessageParams {
    pub session_id: String,
    pub role: MessageRole,
    pub content: Vec<ContentPart>,
    pub provider_metadata: Option<serde_json::Value>,
}

pub struct CreateRunParams {
    pub session_id: String,
    pub status: RunStatus,
    pub trigger: RunTrigger,
    pub connection_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub usage: Option<RunUsage>,
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
    pub provider_id: String,
    pub model_id: String,
    pub input_message_count: i64,
}

#[derive(Debug, Clone)]
pub struct QueuedUserMessage {
    pub message: AssistantMessage,
    pub connection_id: String,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn to_json_string<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("Failed to serialize JSON: {}", e))
}

fn parse_json<T: DeserializeOwned>(value: &str, context: &str) -> Result<T, String> {
    serde_json::from_str(value).map_err(|e| format!("Failed to parse {}: {}", context, e))
}

fn parse_optional_json<T: DeserializeOwned>(
    value: Option<String>,
    context: &str,
) -> Result<Option<T>, String> {
    value.map(|json| parse_json(&json, context)).transpose()
}

fn map_session_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssistantSession, String> {
    Ok(AssistantSession {
        id: row.get("id"),
        kind: parse_json::<SessionKind>(&row.get::<String, _>("kind"), "session kind")?,
        title: row.get("title"),
        context: parse_json::<SessionContext>(
            &row.get::<String, _>("context_json"),
            "session context",
        )?,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn map_message_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssistantMessage, String> {
    Ok(AssistantMessage {
        id: row.get("id"),
        session_id: row.get("session_id"),
        role: parse_json::<MessageRole>(&row.get::<String, _>("role"), "message role")?,
        content: parse_json::<Vec<ContentPart>>(
            &row.get::<String, _>("content_json"),
            "message content",
        )?,
        created_at: row.get("created_at"),
        provider_metadata: parse_optional_json::<serde_json::Value>(
            row.get("provider_metadata_json"),
            "provider metadata",
        )?,
    })
}

fn map_queued_message_row(row: &sqlx::sqlite::SqliteRow) -> Result<QueuedUserMessage, String> {
    Ok(QueuedUserMessage {
        message: map_message_row(row)?,
        connection_id: row.get("connection_id"),
    })
}

fn map_run_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssistantRun, String> {
    Ok(AssistantRun {
        id: row.get("id"),
        session_id: row.get("session_id"),
        status: parse_json::<RunStatus>(&row.get::<String, _>("status"), "run status")?,
        trigger: parse_json::<RunTrigger>(&row.get::<String, _>("trigger"), "run trigger")?,
        connection_id: row.get("connection_id"),
        provider_id: row.get("provider_id"),
        model_id: row.get("model_id"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        usage: parse_optional_json::<RunUsage>(row.get("usage_json"), "run usage")?,
        error: row.get("error"),
        notices: parse_optional_json::<Vec<RunNotice>>(row.get("notices_json"), "run notices")?
            .unwrap_or_default(),
    })
}

fn map_compaction_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssistantCompaction, String> {
    Ok(AssistantCompaction {
        id: row.get("id"),
        session_id: row.get("session_id"),
        trigger: parse_json::<CompactionTrigger>(
            &row.get::<String, _>("trigger"),
            "compaction trigger",
        )?,
        strategy: parse_json::<CompactionStrategy>(
            &row.get::<String, _>("strategy"),
            "compaction strategy",
        )?,
        status: parse_json::<CompactionStatus>(
            &row.get::<String, _>("status"),
            "compaction status",
        )?,
        source_from_message_id: row.get("source_from_message_id"),
        source_to_message_id: row.get("source_to_message_id"),
        summary_message_id: row.get("summary_message_id"),
        created_run_id: row.get("created_run_id"),
        provider_id: row.get("provider_id"),
        model_id: row.get("model_id"),
        input_message_count: row.get("input_message_count"),
        created_at: row.get("created_at"),
        completed_at: row.get("completed_at"),
        error: row.get("error"),
    })
}

pub async fn create_session(
    pool: &DbPool,
    params: CreateSessionParams,
) -> Result<AssistantSession, String> {
    let now = now_ms();

    let session = AssistantSession {
        id: Uuid::new_v4().to_string(),
        kind: params.kind,
        title: params.title,
        context: params.context,
        created_at: now,
        updated_at: now,
    };

    sqlx::query(
        r#"
        INSERT INTO assistant_sessions
            (id, kind, title, context_json, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&session.id)
    .bind(to_json_string(&session.kind)?)
    .bind(&session.title)
    .bind(to_json_string(&session.context)?)
    .bind(session.created_at)
    .bind(session.updated_at)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant session: {}", e))?;

    Ok(session)
}

/// Create a session AND record its id on the given `workspace_tasks` row, in a
/// single SQLite transaction.
///
/// `assignTask` creates the task row first (with `session_id = NULL`), then the
/// `BackgroundJob` session that runs the task, then later UPDATEs the task row
/// to stamp the session id. Between the session INSERT and that UPDATE there is
/// a window where the new session is visible to `list_non_task_sessions` but
/// has no `workspace_tasks.session_id` link, so the resolver would treat it as
/// the most-recent non-task session and briefly hijack the conversation view
/// for self-assigned tasks (manager→manager). Bundling the two writes into one
/// transaction closes that window: the session only becomes visible to a
/// concurrent resolver after the task row already points at it, so the anti-join
/// excludes it.
pub async fn create_session_and_link_task(
    pool: &DbPool,
    params: CreateSessionParams,
    task_id: &str,
) -> Result<AssistantSession, String> {
    let now = now_ms();
    let session = AssistantSession {
        id: Uuid::new_v4().to_string(),
        kind: params.kind,
        title: params.title,
        context: params.context,
        created_at: now,
        updated_at: now,
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start session+task link transaction: {e}"))?;

    sqlx::query(
        r#"
        INSERT INTO assistant_sessions
            (id, kind, title, context_json, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&session.id)
    .bind(to_json_string(&session.kind)?)
    .bind(&session.title)
    .bind(to_json_string(&session.context)?)
    .bind(session.created_at)
    .bind(session.updated_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to create assistant session: {e}"))?;

    sqlx::query(
        r#"
        UPDATE workspace_tasks SET session_id = ? WHERE id = ?
        "#,
    )
    .bind(&session.id)
    .bind(task_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to link session to workspace task: {e}"))?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit session+task link transaction: {e}"))?;

    Ok(session)
}

pub async fn get_session(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<AssistantSession>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, kind, title, context_json, created_at, updated_at
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

pub async fn list_sessions(pool: &DbPool) -> Result<Vec<AssistantSession>, String> {
    let rows = sqlx::query(
        r#"
        SELECT id, kind, title, context_json, created_at, updated_at
        FROM assistant_sessions
        ORDER BY updated_at DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list assistant sessions: {}", e))?;

    rows.iter().map(map_session_row).collect()
}

/// Sessions that are NOT task delegations.
///
/// `workspace_assignTask` runs each delegated task in its own `BackgroundJob`
/// session whose id is recorded in `workspace_tasks.session_id`. Such sessions
/// must never be mistaken for a workspace's canonical conversation — and when a
/// manager assigns a task to *itself* the task session even carries the
/// manager's own `automation_id`, so an `automation_id` filter alone would not
/// exclude it. Excluding task-linked sessions here, in SQL, keeps resolution
/// independent of task volume (the unbounded set of task rows is never loaded)
/// and leaves exactly the candidate conversations: the interactive chat plus
/// any non-task `BackgroundJob` session (e.g. a scheduled-run conversation that
/// older builds forked into its own row).
///
/// The `session_id IS NOT NULL` guard in the subquery is required: a `NULL` in
/// a `NOT IN (...)` set makes the whole predicate `NULL` (never true) in SQL,
/// which would otherwise return zero rows whenever any task has no session yet.
pub async fn list_non_task_sessions(pool: &DbPool) -> Result<Vec<AssistantSession>, String> {
    let rows = sqlx::query(
        r#"
        SELECT id, kind, title, context_json, created_at, updated_at
        FROM assistant_sessions
        WHERE id NOT IN (
            SELECT session_id FROM workspace_tasks WHERE session_id IS NOT NULL
        )
        ORDER BY updated_at DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list non-task assistant sessions: {}", e))?;

    rows.iter().map(map_session_row).collect()
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
        SET title = ?, context_json = ?, updated_at = ?
        WHERE id = ?
        "#,
    )
    .bind(&session.title)
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
        SELECT id, session_id, role, content_json, provider_metadata_json, created_at
        FROM assistant_messages
        WHERE session_id = ?
        ORDER BY created_at ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load assistant messages: {}", e))?;

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
            SELECT id, session_id, role, content_json, provider_metadata_json, created_at
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
            SELECT id, session_id, role, content_json, provider_metadata_json, created_at
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

pub async fn parent_session_id(
    pool: &DbPool,
    child_session_id: &str,
) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, String>(
        r#"
        SELECT parent_session_id
        FROM assistant_session_links
        WHERE child_session_id = ?
        "#,
    )
    .bind(child_session_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load assistant session parent: {}", e))
}

/// Count every message in a session's conversation: the session itself plus —
/// when `include_ancestors` is set — all ancestors reachable through rotation
/// links (compaction/rotation chains). One recursive-CTE round trip; UNION
/// (not UNION ALL) so a malformed link cycle terminates instead of recursing
/// forever.
pub async fn count_session_chain_messages(
    pool: &DbPool,
    session_id: &str,
    include_ancestors: bool,
) -> Result<u32, String> {
    let count = if include_ancestors {
        sqlx::query_scalar::<_, i64>(
            r#"
            WITH RECURSIVE chain(session_id) AS (
                SELECT ?
                UNION
                SELECT l.parent_session_id
                FROM assistant_session_links l
                JOIN chain c ON l.child_session_id = c.session_id
            )
            SELECT COUNT(*)
            FROM assistant_messages
            WHERE session_id IN (SELECT session_id FROM chain)
            "#,
        )
        .bind(session_id)
        .fetch_one(pool)
        .await
    } else {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM assistant_messages WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(pool)
            .await
    }
    .map_err(|e| format!("Failed to count assistant messages: {}", e))?;

    Ok(count.max(0) as u32)
}

pub async fn create_session_rotation_link(
    pool: &DbPool,
    child_session_id: &str,
    parent_session_id: &str,
) -> Result<(), String> {
    sqlx::query(
        r#"
        INSERT INTO assistant_session_links
            (child_session_id, parent_session_id, kind, created_at)
        VALUES (?, ?, 'rotation', ?)
        "#,
    )
    .bind(child_session_id)
    .bind(parent_session_id)
    .bind(now_ms())
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant session rotation link: {}", e))?;

    Ok(())
}

pub async fn get_message(
    pool: &DbPool,
    message_id: &str,
) -> Result<Option<AssistantMessage>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, session_id, role, content_json, provider_metadata_json, created_at
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

/// Permanently remove a message row and its `assistant_message_queue` row
/// (deleted explicitly — the schema's ON DELETE CASCADE only fires on
/// connections where `PRAGMA foreign_keys` happens to be on, which the
/// pool doesn't guarantee). Used to discard run input that never got an
/// answer — a user message (and the empty assistant placeholder) of a run
/// that failed before the provider produced anything.
pub async fn delete_message(pool: &DbPool, message_id: &str) -> Result<(), String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start message delete transaction: {}", e))?;

    sqlx::query("DELETE FROM assistant_message_queue WHERE message_id = ?")
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to delete queued assistant message: {}", e))?;

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

/// Delete a message that is still *pending* in the queue — i.e. the user
/// wrote it while a run was active and no run has picked it up yet.
/// Returns `false` (without touching anything) when the message has
/// already been delivered or was never queued: the queue-row delete is
/// conditioned on `status = 'pending'`, so the check-and-remove is one
/// atomic statement and can't race a concurrent delivery.
pub async fn delete_pending_queued_message(
    pool: &DbPool,
    session_id: &str,
    message_id: &str,
) -> Result<bool, String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start queued message delete transaction: {}", e))?;

    let removed = sqlx::query(
        r#"
        DELETE FROM assistant_message_queue
        WHERE message_id = ? AND session_id = ? AND status = 'pending'
        "#,
    )
    .bind(message_id)
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to delete queued assistant message: {}", e))?;

    if removed.rows_affected() == 0 {
        return Ok(false);
    }

    sqlx::query("DELETE FROM assistant_messages WHERE id = ?")
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to delete assistant message: {}", e))?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit queued message delete transaction: {}", e))?;
    Ok(true)
}

/// Update the text of a message that is still *pending* in the queue — the
/// user wrote it while a run was active and no run has picked it up yet.
/// Returns the updated message, or `None` (touching nothing) when the
/// message has already been delivered or was never queued. The pending
/// check and the content update share one transaction, so an edit can't
/// race a concurrent delivery: a message already handed to a run is frozen.
pub async fn update_pending_queued_message(
    pool: &DbPool,
    session_id: &str,
    message_id: &str,
    text: String,
) -> Result<Option<AssistantMessage>, String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start queued message edit transaction: {}", e))?;

    // Guard: only pending queue rows are editable. Pull `created_at` in the
    // same query so we can return a faithful message without a second read.
    let row = sqlx::query(
        r#"
        SELECT q.status AS status, m.created_at AS created_at
        FROM assistant_message_queue q
        JOIN assistant_messages m ON m.id = q.message_id
        WHERE q.message_id = ? AND q.session_id = ?
        "#,
    )
    .bind(message_id)
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| format!("Failed to read queued assistant message: {}", e))?;

    let Some(row) = row else {
        return Ok(None);
    };
    if row.get::<String, _>("status") != "pending" {
        return Ok(None);
    }

    let message = AssistantMessage {
        id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: MessageRole::User,
        content: vec![ContentPart::Text { text }],
        created_at: row.get("created_at"),
        provider_metadata: None,
    };

    sqlx::query("UPDATE assistant_messages SET content_json = ? WHERE id = ? AND session_id = ?")
        .bind(to_json_string(&message.content)?)
        .bind(message_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to update queued assistant message: {}", e))?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit queued message edit transaction: {}", e))?;
    Ok(Some(message))
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
    };

    sqlx::query(
        r#"
        INSERT INTO assistant_messages
            (id, session_id, role, content_json, provider_metadata_json, created_at)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&message.id)
    .bind(&message.session_id)
    .bind(to_json_string(&message.role)?)
    .bind(to_json_string(&message.content)?)
    .bind(
        message
            .provider_metadata
            .as_ref()
            .map(to_json_string)
            .transpose()?,
    )
    .bind(message.created_at)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant message: {}", e))?;

    touch_session(pool, &message.session_id).await?;

    Ok(message)
}

pub async fn create_user_message(
    pool: &DbPool,
    session_id: String,
    text: String,
    queue_connection_id: Option<&str>,
) -> Result<AssistantMessage, String> {
    let now = now_ms();
    let message = AssistantMessage {
        id: Uuid::new_v4().to_string(),
        session_id,
        role: MessageRole::User,
        content: vec![ContentPart::Text { text }],
        created_at: now,
        provider_metadata: None,
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
    .bind(to_json_string(&message.role)?)
    .bind(to_json_string(&message.content)?)
    .bind(Option::<String>::None)
    .bind(message.created_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to create assistant message: {}", e))?;

    if let Some(connection_id) = queue_connection_id {
        sqlx::query(
            r#"
            INSERT INTO assistant_message_queue
                (message_id, session_id, connection_id, status, queued_at, delivered_run_id, delivered_at)
            VALUES (?, ?, ?, 'pending', ?, NULL, NULL)
            "#,
        )
        .bind(&message.id)
        .bind(&message.session_id)
        .bind(connection_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to queue assistant message: {}", e))?;
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

pub async fn create_run(pool: &DbPool, params: CreateRunParams) -> Result<AssistantRun, String> {
    let run = AssistantRun {
        id: Uuid::new_v4().to_string(),
        session_id: params.session_id,
        status: params.status,
        trigger: params.trigger,
        connection_id: params.connection_id,
        provider_id: params.provider_id,
        model_id: params.model_id,
        started_at: now_ms(),
        completed_at: None,
        usage: params.usage,
        error: params.error,
        notices: Vec::new(),
    };

    sqlx::query(
        r#"
        INSERT INTO assistant_runs
            (id, session_id, status, trigger, connection_id, provider_id, model_id, usage_json, error, notices_json, started_at, completed_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&run.id)
    .bind(&run.session_id)
    .bind(to_json_string(&run.status)?)
    .bind(to_json_string(&run.trigger)?)
    .bind(&run.connection_id)
    .bind(&run.provider_id)
    .bind(&run.model_id)
    .bind(run.usage.as_ref().map(to_json_string).transpose()?)
    .bind(&run.error)
    .bind(Option::<String>::None) // notices_json — empty on creation
    .bind(run.started_at)
    .bind(run.completed_at)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant run: {}", e))?;

    touch_session(pool, &run.session_id).await?;

    Ok(run)
}

pub async fn list_runs(pool: &DbPool, session_id: &str) -> Result<Vec<AssistantRun>, String> {
    let rows = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, provider_id, model_id, usage_json, error, notices_json, started_at, completed_at
        FROM assistant_runs
        WHERE session_id = ?
        ORDER BY started_at DESC
        "#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list assistant runs: {}", e))?;

    rows.iter().map(map_run_row).collect()
}

/// Whether the session currently has a non-terminal run (queued, running,
/// or waiting for a tool). Used by message queueing and follow-up scheduling
/// to preserve one active run per session.
pub async fn session_has_active_run(pool: &DbPool, session_id: &str) -> Result<bool, String> {
    let (count,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM assistant_runs
        WHERE session_id = ?
          AND status IN ('"queued"', '"running"', '"waiting_for_tool"')
        "#,
    )
    .bind(session_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to check for active runs: {}", e))?;

    Ok(count > 0)
}

pub async fn get_active_run(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<AssistantRun>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, provider_id, model_id, usage_json, error, notices_json, started_at, completed_at
        FROM assistant_runs
        WHERE session_id = ?
          AND status IN ('"queued"', '"running"', '"waiting_for_tool"')
        ORDER BY started_at DESC
        LIMIT 1
        "#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load active run: {}", e))?;

    row.as_ref().map(map_run_row).transpose()
}

pub async fn list_pending_queued_messages(
    pool: &DbPool,
    session_id: &str,
) -> Result<Vec<QueuedUserMessage>, String> {
    let rows = sqlx::query(
        r#"
        SELECT m.id, m.session_id, m.role, m.content_json, m.provider_metadata_json, m.created_at,
               q.connection_id
        FROM assistant_message_queue q
        JOIN assistant_messages m ON m.id = q.message_id
        WHERE q.session_id = ? AND q.status = 'pending'
        ORDER BY q.queued_at ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list pending queued messages: {}", e))?;

    rows.iter().map(map_queued_message_row).collect()
}

pub async fn list_delivered_queued_messages_for_run(
    pool: &DbPool,
    session_id: &str,
    run_id: &str,
) -> Result<Vec<QueuedUserMessage>, String> {
    let rows = sqlx::query(
        r#"
        SELECT m.id, m.session_id, m.role, m.content_json, m.provider_metadata_json, m.created_at,
               q.connection_id
        FROM assistant_message_queue q
        JOIN assistant_messages m ON m.id = q.message_id
        WHERE q.session_id = ?
          AND q.status = 'delivered'
          AND q.delivered_run_id = ?
        ORDER BY q.queued_at ASC
        "#,
    )
    .bind(session_id)
    .bind(run_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list delivered queued messages: {}", e))?;

    rows.iter().map(map_queued_message_row).collect()
}

pub async fn list_pending_queued_message_ids(
    pool: &DbPool,
    session_id: &str,
) -> Result<Vec<String>, String> {
    let rows = sqlx::query(
        r#"
        SELECT message_id
        FROM assistant_message_queue
        WHERE session_id = ? AND status = 'pending'
        ORDER BY queued_at ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list pending queued message ids: {}", e))?;

    Ok(rows.iter().map(|row| row.get("message_id")).collect())
}

pub async fn mark_queued_messages_delivered(
    pool: &DbPool,
    session_id: &str,
    run_id: &str,
    message_ids: &[String],
) -> Result<(), String> {
    if message_ids.is_empty() {
        return Ok(());
    }

    let delivered_at = now_ms();
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start queue delivery transaction: {}", e))?;

    for message_id in message_ids {
        sqlx::query(
            r#"
            UPDATE assistant_message_queue
            SET status = 'delivered',
                delivered_run_id = ?,
                delivered_at = ?
            WHERE session_id = ?
              AND message_id = ?
              AND status = 'pending'
            "#,
        )
        .bind(run_id)
        .bind(delivered_at)
        .bind(session_id)
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to mark queued message delivered: {}", e))?;
    }

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit queue delivery transaction: {}", e))?;

    Ok(())
}

pub async fn get_run(pool: &DbPool, run_id: &str) -> Result<Option<AssistantRun>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, provider_id, model_id, usage_json, error, notices_json, started_at, completed_at
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
    let is_terminal = matches!(
        status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    );
    let completed_at = if is_terminal { Some(now_ms()) } else { None };

    sqlx::query(
        r#"
        UPDATE assistant_runs
        SET status = ?, error = ?, completed_at = COALESCE(?, completed_at)
        WHERE id = ?
        "#,
    )
    .bind(to_json_string(&status)?)
    .bind(error)
    .bind(completed_at)
    .bind(run_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update run status: {}", e))?;

    let row = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, provider_id, model_id, usage_json, error, notices_json, started_at, completed_at
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
    usage: Option<&RunUsage>,
    error: Option<&str>,
    notices: &[RunNotice],
) -> Result<AssistantRun, String> {
    let completed_at = now_ms();
    let notices_json = if notices.is_empty() {
        None
    } else {
        Some(to_json_string(&notices)?)
    };

    sqlx::query(
        r#"
        UPDATE assistant_runs
        SET status = ?, usage_json = COALESCE(?, usage_json), error = ?, notices_json = COALESCE(?, notices_json), completed_at = ?
        WHERE id = ?
        "#,
    )
    .bind(to_json_string(&status)?)
    .bind(usage.map(to_json_string).transpose()?)
    .bind(error)
    .bind(notices_json)
    .bind(completed_at)
    .bind(run_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to complete run: {}", e))?;

    let row = sqlx::query(
        r#"
        SELECT id, session_id, status, trigger, connection_id, provider_id, model_id, usage_json, error, notices_json, started_at, completed_at
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
        SELECT id, session_id, role, content_json, provider_metadata_json, created_at
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
        provider_id: params.provider_id,
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
             summary_message_id, created_run_id, provider_id, model_id, input_message_count,
             created_at, completed_at, error)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&compaction.id)
    .bind(&compaction.session_id)
    .bind(to_json_string(&compaction.trigger)?)
    .bind(to_json_string(&compaction.strategy)?)
    .bind(to_json_string(&compaction.status)?)
    .bind(&compaction.source_from_message_id)
    .bind(&compaction.source_to_message_id)
    .bind(&compaction.summary_message_id)
    .bind(&compaction.created_run_id)
    .bind(&compaction.provider_id)
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
    .bind(to_json_string(&CompactionStatus::Completed)?)
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
               source_to_message_id, summary_message_id, created_run_id, provider_id,
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
               source_to_message_id, summary_message_id, created_run_id, provider_id,
               model_id, input_message_count, created_at, completed_at, error
        FROM assistant_compactions
        WHERE session_id = ?
          AND status = '"completed"'
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
        status: parse_json::<ToolCallStatus>(&row.get::<String, _>("status"), "tool call status")?,
        result: parse_optional_json(row.get("result_json"), "tool call result")?,
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
    .bind(to_json_string(&tc.status)?)
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
    .bind(to_json_string(&status)?)
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

pub async fn list_tool_calls(
    pool: &DbPool,
    session_id: &str,
    run_id: Option<&str>,
) -> Result<Vec<ToolInvocation>, String> {
    let rows = if let Some(run_id) = run_id {
        sqlx::query(
            r#"
            SELECT id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at
            FROM assistant_tool_calls
            WHERE session_id = ? AND run_id = ?
            ORDER BY started_at ASC
            "#,
        )
        .bind(session_id)
        .bind(run_id)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list assistant tool calls: {}", e))?
    } else {
        sqlx::query(
            r#"
            SELECT id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at
            FROM assistant_tool_calls
            WHERE session_id = ?
            ORDER BY started_at ASC
            "#,
        )
        .bind(session_id)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list assistant tool calls: {}", e))?
    };

    rows.iter().map(map_tool_call_row).collect()
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
        WHERE run_id = ? AND status IN ('"pending"', '"running"')
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

    let run_result = sqlx::query(
        r#"
        UPDATE assistant_runs
        SET status = '"failed"', error = 'App restarted while running', completed_at = ?
        WHERE status IN ('"queued"', '"running"', '"waiting_for_tool"')
        "#,
    )
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to recover stale runs: {}", e))?;

    let tc_result = sqlx::query(
        r#"
        UPDATE assistant_tool_calls
        SET status = '"failed"', error = 'App restarted while running'
        WHERE status IN ('"pending"', '"running"')
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

    /// Per-test workspace pool in a tempdir, with the production
    /// workspace migrations applied.
    async fn create_test_pool() -> (tempfile::TempDir, DbPool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::db::init_workspace_db(tmp.path()).await.unwrap();
        (tmp, pool)
    }

    async fn insert_session(pool: &DbPool, id: &str) {
        sqlx::query(
            r#"
            INSERT INTO assistant_sessions
                (id, kind, title, context_json, created_at, updated_at)
            VALUES (?, '"chat"', 'Test', '{}', 1, 1)
            "#,
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn queue_rows_for(pool: &DbPool, message_id: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM assistant_message_queue WHERE message_id = ?",
        )
        .bind(message_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn message_text(pool: &DbPool, message_id: &str) -> Option<String> {
        let message = get_message(pool, message_id).await.unwrap()?;
        message.content.into_iter().find_map(|part| match part {
            ContentPart::Text { text } => Some(text),
            _ => None,
        })
    }

    #[tokio::test]
    async fn delete_message_removes_row_and_queue_entry() {
        let (_tmp, pool) = create_test_pool().await;
        insert_session(&pool, "s1").await;

        // Queued variant: the queue side-table row must go with the message.
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

    #[tokio::test]
    async fn delete_pending_queued_message_only_removes_pending() {
        let (_tmp, pool) = create_test_pool().await;
        insert_session(&pool, "s1").await;

        // Pending → deleted, returns true.
        let pending = create_user_message(
            &pool,
            "s1".to_string(),
            "queued".to_string(),
            Some("conn-1"),
        )
        .await
        .unwrap();
        assert!(delete_pending_queued_message(&pool, "s1", &pending.id)
            .await
            .unwrap());
        assert!(get_message(&pool, &pending.id).await.unwrap().is_none());
        assert_eq!(queue_rows_for(&pool, &pending.id).await, 0);

        // Already delivered → untouched, returns false.
        let delivered =
            create_user_message(&pool, "s1".to_string(), "taken".to_string(), Some("conn-1"))
                .await
                .unwrap();
        sqlx::query("UPDATE assistant_message_queue SET status = 'delivered' WHERE message_id = ?")
            .bind(&delivered.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!delete_pending_queued_message(&pool, "s1", &delivered.id)
            .await
            .unwrap());
        assert!(get_message(&pool, &delivered.id).await.unwrap().is_some());

        // Wrong session → untouched, returns false.
        let other =
            create_user_message(&pool, "s1".to_string(), "mine".to_string(), Some("conn-1"))
                .await
                .unwrap();
        assert!(
            !delete_pending_queued_message(&pool, "other-session", &other.id)
                .await
                .unwrap()
        );
        assert!(get_message(&pool, &other.id).await.unwrap().is_some());

        // Never queued → untouched, returns false.
        let direct = create_user_message(&pool, "s1".to_string(), "direct".to_string(), None)
            .await
            .unwrap();
        assert!(!delete_pending_queued_message(&pool, "s1", &direct.id)
            .await
            .unwrap());
        assert!(get_message(&pool, &direct.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn update_pending_queued_message_only_edits_pending() {
        let (_tmp, pool) = create_test_pool().await;
        insert_session(&pool, "s1").await;

        // Pending → edited; returns the updated message with original
        // created_at, and the queue row stays pending (edit ≠ delivery).
        let pending =
            create_user_message(&pool, "s1".to_string(), "old".to_string(), Some("conn-1"))
                .await
                .unwrap();
        let updated =
            update_pending_queued_message(&pool, "s1", &pending.id, "new text".to_string())
                .await
                .unwrap()
                .expect("pending message should be editable");
        assert_eq!(updated.created_at, pending.created_at);
        assert_eq!(
            message_text(&pool, &pending.id).await.as_deref(),
            Some("new text")
        );
        assert_eq!(queue_rows_for(&pool, &pending.id).await, 1);

        // Already delivered → None, text untouched.
        let delivered =
            create_user_message(&pool, "s1".to_string(), "taken".to_string(), Some("conn-1"))
                .await
                .unwrap();
        sqlx::query("UPDATE assistant_message_queue SET status = 'delivered' WHERE message_id = ?")
            .bind(&delivered.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            update_pending_queued_message(&pool, "s1", &delivered.id, "nope".to_string())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            message_text(&pool, &delivered.id).await.as_deref(),
            Some("taken")
        );

        // Wrong session → None, untouched.
        let other =
            create_user_message(&pool, "s1".to_string(), "mine".to_string(), Some("conn-1"))
                .await
                .unwrap();
        assert!(
            update_pending_queued_message(&pool, "other-session", &other.id, "x".to_string())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            message_text(&pool, &other.id).await.as_deref(),
            Some("mine")
        );

        // Never queued → None, untouched.
        let direct = create_user_message(&pool, "s1".to_string(), "direct".to_string(), None)
            .await
            .unwrap();
        assert!(
            update_pending_queued_message(&pool, "s1", &direct.id, "x".to_string())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            message_text(&pool, &direct.id).await.as_deref(),
            Some("direct")
        );
    }
}

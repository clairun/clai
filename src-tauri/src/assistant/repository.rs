use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::assistant::types::{
    AssistantMessage, AssistantRun, AssistantSession, ContentPart, MessageRole, RunNotice,
    RunStatus, RunTrigger, RunUsage, SessionContext, SessionKind, ToolCallStatus, ToolInvocation,
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

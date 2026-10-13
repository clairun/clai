//! `session_signals`: the durable record of input a session still owes a run.
//! A content signal points at the message row written in the same
//! transaction; the message itself stays in `assistant_messages`. Only
//! `user_message` signals exist so far.
//!
//! Lifecycle: `pending` → `claimed` by the run [`crate::assistant::dispatcher`]
//! starts for it, or `pending` → `consumed` when a live run takes it mid-run.

use sqlx::{Row, SqliteConnection};
use uuid::Uuid;

use crate::assistant::repository::{map_message_row, now_ms};
use crate::assistant::types::{AssistantMessage, ContentPart, MessageRole};
use crate::db::DbPool;

/// A content signal and its message.
#[derive(Debug, Clone)]
pub struct SignalContent {
    pub message: AssistantMessage,
    /// The provider connection the sender chose; `None` joins any run.
    pub connection_id: Option<String>,
}

const CONTENT_COLUMNS: &str = "m.id, m.session_id, m.role, m.content_json, \
     m.provider_metadata_json, m.created_at, s.connection_id";

fn map_content_row(row: &sqlx::sqlite::SqliteRow) -> Result<SignalContent, String> {
    Ok(SignalContent {
        message: map_message_row(row)?,
        connection_id: row.get("connection_id"),
    })
}

/// Records `message` as pending input for its session. Call inside the
/// transaction that inserts the message.
pub(crate) async fn insert_user_message_signal(
    conn: &mut SqliteConnection,
    message: &AssistantMessage,
    connection_id: &str,
) -> Result<(), String> {
    sqlx::query(
        r#"
        INSERT INTO session_signals
            (id, target_session_id, kind, lane, message_id, connection_id, status, created_at)
        VALUES (?, ?, 'user_message', 1, ?, ?, 'pending', ?)
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&message.session_id)
    .bind(&message.id)
    .bind(connection_id)
    .bind(message.created_at)
    .execute(conn)
    .await
    .map_err(|e| format!("Failed to insert session signal: {}", e))?;
    Ok(())
}

/// Pending content signals of a session, oldest first. Claimed signals are
/// not listed: they already are the claiming run's input.
pub async fn list_open_content(
    pool: &DbPool,
    session_id: &str,
) -> Result<Vec<SignalContent>, String> {
    let rows = sqlx::query(&format!(
        r#"
        SELECT {CONTENT_COLUMNS}
        FROM session_signals s
        JOIN assistant_messages m ON m.id = s.message_id
        WHERE s.target_session_id = ? AND s.lane = 1 AND s.status = 'pending'
        ORDER BY s.created_at ASC, s.id ASC
        "#
    ))
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list pending session signals: {}", e))?;

    rows.iter().map(map_content_row).collect()
}

/// Message ids of [`list_open_content`].
pub async fn list_open_message_ids(pool: &DbPool, session_id: &str) -> Result<Vec<String>, String> {
    sqlx::query_scalar(
        r#"
        SELECT message_id
        FROM session_signals
        WHERE target_session_id = ? AND lane = 1 AND status = 'pending'
          AND message_id IS NOT NULL
        ORDER BY created_at ASC, id ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list pending session signal ids: {}", e))
}

/// Content claimed by or consumed by `run_id`, oldest first.
pub async fn list_content_for_run(
    pool: &DbPool,
    session_id: &str,
    run_id: &str,
) -> Result<Vec<SignalContent>, String> {
    let rows = sqlx::query(&format!(
        r#"
        SELECT {CONTENT_COLUMNS}
        FROM session_signals s
        JOIN assistant_messages m ON m.id = s.message_id
        WHERE s.target_session_id = ? AND s.lane = 1
          AND (s.claimed_run_id = ? OR s.consumed_run_id = ?)
        ORDER BY s.created_at ASC, s.id ASC
        "#
    ))
    .bind(session_id)
    .bind(run_id)
    .bind(run_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list session signals for run: {}", e))?;

    rows.iter().map(map_content_row).collect()
}

/// Claims every pending content signal of the session that targets
/// `connection_id` (or no connection) for `run_id`. Returns the claimed
/// message ids.
pub(crate) async fn claim_batch(
    conn: &mut SqliteConnection,
    session_id: &str,
    run_id: &str,
    connection_id: &str,
) -> Result<Vec<String>, String> {
    sqlx::query_scalar(
        r#"
        UPDATE session_signals
        SET status = 'claimed', claimed_run_id = ?, claimed_at = ?
        WHERE target_session_id = ? AND lane = 1 AND status = 'pending'
          AND message_id IS NOT NULL
          AND (connection_id = ? OR connection_id IS NULL)
        RETURNING message_id
        "#,
    )
    .bind(run_id)
    .bind(now_ms())
    .bind(session_id)
    .bind(connection_id)
    .fetch_all(conn)
    .await
    .map_err(|e| format!("Failed to claim session signals: {}", e))
}

/// Marks the pending signals of `message_ids` consumed by a live run that
/// delivered them mid-run.
pub async fn consume(
    pool: &DbPool,
    session_id: &str,
    run_id: &str,
    message_ids: &[String],
) -> Result<(), String> {
    if message_ids.is_empty() {
        return Ok(());
    }

    let consumed_at = now_ms();
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start signal consume transaction: {}", e))?;
    for message_id in message_ids {
        sqlx::query(
            r#"
            UPDATE session_signals
            SET status = 'consumed', consumed_run_id = ?, consumed_at = ?
            WHERE target_session_id = ? AND message_id = ? AND status = 'pending'
            "#,
        )
        .bind(run_id)
        .bind(consumed_at)
        .bind(session_id)
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to consume session signal: {}", e))?;
    }
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit signal consume transaction: {}", e))
}

/// Deletes a user message whose signal is still pending, with the signal.
/// Returns `false`, touching nothing, once a run has claimed or consumed it:
/// the conditional delete is the atomic check against delivery.
pub async fn delete_pending_user_message(
    pool: &DbPool,
    session_id: &str,
    message_id: &str,
) -> Result<bool, String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start pending message delete transaction: {}", e))?;

    let removed = sqlx::query(
        r#"
        DELETE FROM session_signals
        WHERE message_id = ? AND target_session_id = ?
          AND kind = 'user_message' AND status = 'pending'
        "#,
    )
    .bind(message_id)
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to delete pending session signal: {}", e))?;
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
        .map_err(|e| format!("Failed to commit pending message delete transaction: {}", e))?;
    Ok(true)
}

/// Replaces the text of a user message whose signal is still pending.
/// Returns `None`, touching nothing, once a run has claimed or consumed it.
pub async fn edit_pending_user_message(
    pool: &DbPool,
    session_id: &str,
    message_id: &str,
    text: String,
) -> Result<Option<AssistantMessage>, String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start pending message edit transaction: {}", e))?;

    let created_at: Option<i64> = sqlx::query_scalar(
        r#"
        SELECT m.created_at
        FROM session_signals s
        JOIN assistant_messages m ON m.id = s.message_id
        WHERE s.message_id = ? AND s.target_session_id = ?
          AND s.kind = 'user_message' AND s.status = 'pending'
        "#,
    )
    .bind(message_id)
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| format!("Failed to read pending session signal: {}", e))?;
    let Some(created_at) = created_at else {
        return Ok(None);
    };

    let message = AssistantMessage {
        id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: MessageRole::User,
        content: vec![ContentPart::Text { text }],
        created_at,
        provider_metadata: None,
    };
    let content_json = serde_json::to_string(&message.content)
        .map_err(|e| format!("Failed to serialize JSON: {}", e))?;
    sqlx::query("UPDATE assistant_messages SET content_json = ? WHERE id = ? AND session_id = ?")
        .bind(content_json)
        .bind(message_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to update pending assistant message: {}", e))?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit pending message edit transaction: {}", e))?;
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::repository::{create_user_message, get_message};
    use crate::db::test_support::workspace_pool;

    async fn insert_session(pool: &DbPool, id: &str) {
        sqlx::query(
            "INSERT INTO assistant_sessions (id, context_json, created_at, updated_at)
             VALUES (?, '{}', 1, 1)",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn signal_rows_for(pool: &DbPool, message_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM session_signals WHERE message_id = ?")
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
    async fn delete_pending_user_message_removes_signal() {
        let (_tmp, pool) = workspace_pool().await;
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
        assert!(delete_pending_user_message(&pool, "s1", &pending.id)
            .await
            .unwrap());
        assert!(get_message(&pool, &pending.id).await.unwrap().is_none());
        assert_eq!(signal_rows_for(&pool, &pending.id).await, 0);

        // Already delivered → untouched, returns false.
        let delivered =
            create_user_message(&pool, "s1".to_string(), "taken".to_string(), Some("conn-1"))
                .await
                .unwrap();
        sqlx::query("UPDATE session_signals SET status = 'consumed' WHERE message_id = ?")
            .bind(&delivered.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!delete_pending_user_message(&pool, "s1", &delivered.id)
            .await
            .unwrap());
        assert!(get_message(&pool, &delivered.id).await.unwrap().is_some());

        // Wrong session → untouched, returns false.
        let other =
            create_user_message(&pool, "s1".to_string(), "mine".to_string(), Some("conn-1"))
                .await
                .unwrap();
        assert!(
            !delete_pending_user_message(&pool, "other-session", &other.id)
                .await
                .unwrap()
        );
        assert!(get_message(&pool, &other.id).await.unwrap().is_some());

        // Never queued → untouched, returns false.
        let direct = create_user_message(&pool, "s1".to_string(), "direct".to_string(), None)
            .await
            .unwrap();
        assert!(!delete_pending_user_message(&pool, "s1", &direct.id)
            .await
            .unwrap());
        assert!(get_message(&pool, &direct.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn edit_pending_user_message_only_edits_pending() {
        let (_tmp, pool) = workspace_pool().await;
        insert_session(&pool, "s1").await;

        // Pending → edited; returns the updated message with original
        // created_at, and the signal stays pending (edit ≠ delivery).
        let pending =
            create_user_message(&pool, "s1".to_string(), "old".to_string(), Some("conn-1"))
                .await
                .unwrap();
        let updated = edit_pending_user_message(&pool, "s1", &pending.id, "new text".to_string())
            .await
            .unwrap()
            .expect("pending message should be editable");
        assert_eq!(updated.created_at, pending.created_at);
        assert_eq!(
            message_text(&pool, &pending.id).await.as_deref(),
            Some("new text")
        );
        assert_eq!(signal_rows_for(&pool, &pending.id).await, 1);

        // Already delivered → None, text untouched.
        let delivered =
            create_user_message(&pool, "s1".to_string(), "taken".to_string(), Some("conn-1"))
                .await
                .unwrap();
        sqlx::query("UPDATE session_signals SET status = 'consumed' WHERE message_id = ?")
            .bind(&delivered.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            edit_pending_user_message(&pool, "s1", &delivered.id, "nope".to_string())
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
            edit_pending_user_message(&pool, "other-session", &other.id, "x".to_string())
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
            edit_pending_user_message(&pool, "s1", &direct.id, "x".to_string())
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

//! Tests for `src/assistant/repository.rs`
//!
//! These tests target the CRUD operations against a real workspace database:
//! a tempdir `data.sqlite` with the embedded `migrations/workspace/` files
//! applied, so the schema — including the foreign keys and cascades — is the
//! one production runs. They do not depend on Tauri state or the full app
//! runtime.

use super::repository::*;
use super::types::*;
use crate::db::test_support::{insert_task, workspace_pool};

fn sample_context() -> SessionContext {
    SessionContext::default()
}

// ---------------------------------------------------------------------------
// Session CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_and_get_session() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    assert_eq!(session.context, sample_context());
    assert!(session.created_at > 0);

    let fetched = get_session(&pool, &session.id).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_eq!(fetched.id, session.id);
    assert_eq!(fetched.context, session.context);
}

#[tokio::test]
async fn test_get_session_missing_returns_none() {
    let (_tmp, pool) = workspace_pool().await;
    let result = get_session(&pool, "no-such-id").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_find_manager_session_ignores_newer_self_task() {
    let (_tmp, pool) = workspace_pool().await;
    let context = |manager: &str| SessionContext {
        automation_id: Some(manager.to_string()),
        ..sample_context()
    };
    let conversation = create_session(&pool, context("manager")).await.unwrap();
    let self_task = create_session(&pool, context("manager")).await.unwrap();
    insert_task(&pool, "self-task", "running", Some(&self_task.id), None).await;
    insert_task(&pool, "pending-task", "queued", None, None).await;
    let other = create_session(&pool, context("other")).await.unwrap();
    for (id, updated_at) in [
        (&conversation.id, 10_i64),
        (&self_task.id, 30),
        (&other.id, 40),
    ] {
        sqlx::query("UPDATE assistant_sessions SET updated_at = ? WHERE id = ?")
            .bind(updated_at)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        find_manager_session(&pool, "manager")
            .await
            .unwrap()
            .unwrap()
            .id,
        conversation.id
    );
    let scheduled = create_session(&pool, context("manager")).await.unwrap();
    sqlx::query("UPDATE assistant_sessions SET updated_at = ? WHERE id = ?")
        .bind(20_i64)
        .bind(&scheduled.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        find_manager_session(&pool, "manager")
            .await
            .unwrap()
            .unwrap()
            .id,
        scheduled.id
    );
    assert!(find_manager_session(&pool, "missing")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_find_manager_session_breaks_updated_at_tie_by_id_desc() {
    let (_tmp, pool) = workspace_pool().await;
    let params = || SessionContext {
        automation_id: Some("manager".to_string()),
        ..sample_context()
    };
    // SQLite's order for equal sort keys is unspecified; `id DESC` is the
    // explicit tie-break.
    for id in ["session-a", "session-b"] {
        let session = create_session(&pool, params()).await.unwrap();
        sqlx::query("UPDATE assistant_sessions SET id = ?, updated_at = 10 WHERE id = ?")
            .bind(id)
            .bind(&session.id)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        find_manager_session(&pool, "manager")
            .await
            .unwrap()
            .unwrap()
            .id,
        "session-b"
    );
}

#[tokio::test]
async fn test_delete_session() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let deleted = delete_session(&pool, &session.id).await.unwrap();
    assert!(deleted);

    let missing = delete_session(&pool, &session.id).await.unwrap();
    assert!(!missing);

    let fetched = get_session(&pool, &session.id).await.unwrap();
    assert!(fetched.is_none());
}

#[tokio::test]
async fn test_count_session_messages() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    assert_eq!(count_session_messages(&pool, &session.id).await.unwrap(), 0);

    for n in 0..3 {
        create_message(
            &pool,
            CreateMessageParams {
                run_id: None,
                session_id: session.id.clone(),
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: format!("msg {}", n),
                }],
                provider_metadata: None,
            },
        )
        .await
        .unwrap();
    }

    assert_eq!(count_session_messages(&pool, &session.id).await.unwrap(), 3);
    // Unknown session: zero, not an error.
    assert_eq!(count_session_messages(&pool, "missing").await.unwrap(), 0);
}

#[tokio::test]
async fn test_update_session_context() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let mut updated = session.clone();
    updated.context.cli_session_id = Some("cli-1".to_string());

    update_session(&pool, &updated).await.unwrap();

    let fetched = get_session(&pool, &session.id).await.unwrap().unwrap();
    assert_eq!(fetched.context.cli_session_id.as_deref(), Some("cli-1"));
}

// ---------------------------------------------------------------------------
// Message CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_and_list_messages() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let _msg1 = create_message(
        &pool,
        CreateMessageParams {
            run_id: None,
            session_id: session.id.clone(),
            role: MessageRole::User,
            content: vec![ContentPart::Text {
                text: "Hello".to_string(),
            }],
            provider_metadata: None,
        },
    )
    .await
    .unwrap();

    let _msg2 = create_message(
        &pool,
        CreateMessageParams {
            run_id: None,
            session_id: session.id.clone(),
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "Hi there".to_string(),
            }],
            provider_metadata: Some(serde_json::json!({"model": "gpt-4"})),
        },
    )
    .await
    .unwrap();

    let messages = list_messages(&pool, &session.id).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(
        messages[1].provider_metadata,
        Some(serde_json::json!({"model": "gpt-4"}))
    );
}

#[tokio::test]
async fn test_update_message_content() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let msg = create_message(
        &pool,
        CreateMessageParams {
            run_id: None,
            session_id: session.id.clone(),
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "Old".to_string(),
            }],
            provider_metadata: None,
        },
    )
    .await
    .unwrap();

    let updated = update_message_content(
        &pool,
        &msg.id,
        &[
            ContentPart::Text {
                text: "New".to_string(),
            },
            ContentPart::ToolUse {
                tool_call_id: "tc-1".to_string(),
                tool_name: "fs.read".to_string(),
                arguments: serde_json::json!({"path": "/tmp"}),
            },
        ],
    )
    .await
    .unwrap();

    assert_eq!(updated.content.len(), 2);
    match &updated.content[0] {
        ContentPart::Text { text } => assert_eq!(text, "New"),
        _ => panic!("expected text"),
    }
    match &updated.content[1] {
        ContentPart::ToolUse {
            tool_call_id,
            tool_name,
            ..
        } => {
            assert_eq!(tool_call_id, "tc-1");
            assert_eq!(tool_name, "fs.read");
        }
        _ => panic!("expected tool use"),
    }
}

#[tokio::test]
async fn test_user_message_queue_lifecycle() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let unqueued = create_user_message(&pool, session.id.clone(), "first".into(), None)
        .await
        .unwrap();
    let queued = create_user_message(
        &pool,
        session.id.clone(),
        "while you work".into(),
        Some("conn-1"),
    )
    .await
    .unwrap();

    let pending = crate::assistant::signals::list_open_content(&pool, &session.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message.id, queued.id);
    assert_eq!(pending[0].connection_id.as_deref(), Some("conn-1"));
    assert_ne!(pending[0].message.id, unqueued.id);

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    crate::assistant::signals::consume(
        &pool,
        &session.id,
        &run.id,
        std::slice::from_ref(&queued.id),
    )
    .await
    .unwrap();

    assert!(
        crate::assistant::signals::list_open_content(&pool, &session.id)
            .await
            .unwrap()
            .is_empty()
    );

    let delivered = crate::assistant::signals::list_content_for_run(&pool, &session.id, &run.id)
        .await
        .unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message.id, queued.id);
}

#[tokio::test]
async fn test_get_active_run_ignores_terminal_runs() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Completed,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();
    assert!(get_active_run(&pool, &session.id).await.unwrap().is_none());

    let running = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        get_active_run(&pool, &session.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        running.id
    );
}

#[tokio::test]
async fn test_workspace_has_active_run_tracks_any_session() {
    let (_tmp, pool) = workspace_pool().await;

    let first = create_session(&pool, sample_context()).await.unwrap();
    let second = create_session(&pool, sample_context()).await.unwrap();

    assert!(!workspace_has_active_run(&pool).await.unwrap());

    create_run(
        &pool,
        CreateRunParams {
            session_id: first.id.clone(),
            status: RunStatus::Completed,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();
    create_run(
        &pool,
        CreateRunParams {
            session_id: second.id.clone(),
            status: RunStatus::WaitingForTool,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    assert!(workspace_has_active_run(&pool).await.unwrap());
}

// ---------------------------------------------------------------------------
// Run CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_and_get_run() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.status, RunStatus::Queued);
    assert_eq!(run.trigger, RunTrigger::UserMessage);
    assert_eq!(run.connection_id, "conn-1");
    assert!(run.completed_at.is_none());

    // Assert on the *stored* row, not the struct `create_run` returned: the
    // INSERT binds `protocol_id` and `model_id` as adjacent strings, so only a
    // round-trip catches a swapped pair.
    let fetched = get_run(&pool, &run.id)
        .await
        .unwrap()
        .expect("run persisted");
    assert_eq!(fetched.id, run.id);
    assert_eq!(fetched.protocol_id, "openai");
    assert_eq!(fetched.model_id, "gpt-4");
    assert_eq!(fetched.connection_id, "conn-1");
}

async fn insert_run_at(
    pool: &crate::db::DbPool,
    session_id: &str,
    status: RunStatus,
    started_at: i64,
) -> String {
    let run = create_run(
        pool,
        CreateRunParams {
            session_id: session_id.to_string(),
            status,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE assistant_runs SET started_at = ? WHERE id = ?")
        .bind(started_at)
        .bind(&run.id)
        .execute(pool)
        .await
        .unwrap();
    run.id
}

async fn current_run_ids(pool: &crate::db::DbPool, session_id: &str) -> Vec<String> {
    current_runs(pool, session_id)
        .await
        .unwrap()
        .into_iter()
        .map(|run| run.id)
        .collect()
}

#[tokio::test]
async fn test_current_runs_returns_latest_and_older_active_run() {
    let (_tmp, pool) = workspace_pool().await;
    let session = create_session(&pool, sample_context()).await.unwrap();

    insert_run_at(&pool, &session.id, RunStatus::Completed, 1).await;
    let running = insert_run_at(&pool, &session.id, RunStatus::Running, 2).await;
    let latest = insert_run_at(&pool, &session.id, RunStatus::Completed, 3).await;

    assert_eq!(current_run_ids(&pool, &session.id).await, [latest, running]);
}

#[tokio::test]
async fn test_current_runs_returns_only_latest_when_idle() {
    let (_tmp, pool) = workspace_pool().await;
    let session = create_session(&pool, sample_context()).await.unwrap();
    assert!(current_run_ids(&pool, &session.id).await.is_empty());

    for started_at in 1..=2 {
        insert_run_at(&pool, &session.id, RunStatus::Completed, started_at).await;
    }
    let latest = insert_run_at(&pool, &session.id, RunStatus::Failed, 3).await;

    assert_eq!(current_run_ids(&pool, &session.id).await, [latest]);
}

#[tokio::test]
async fn test_current_runs_lists_active_latest_run_once() {
    let (_tmp, pool) = workspace_pool().await;
    let session = create_session(&pool, sample_context()).await.unwrap();

    insert_run_at(&pool, &session.id, RunStatus::Completed, 1).await;
    let active = insert_run_at(&pool, &session.id, RunStatus::Queued, 2).await;

    assert_eq!(current_run_ids(&pool, &session.id).await, [active]);
}

#[tokio::test]
async fn test_update_run_status_to_terminal_sets_completed_at() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    assert!(run.completed_at.is_none());

    let updated = update_run_status(&pool, &run.id, RunStatus::Failed, Some("Oops"))
        .await
        .unwrap();
    assert_eq!(updated.status, RunStatus::Failed);
    assert_eq!(updated.error, Some("Oops".to_string()));
    assert!(updated.completed_at.is_some());
}

#[tokio::test]
async fn test_update_run_status_non_terminal_does_not_set_completed_at() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    let updated = update_run_status(&pool, &run.id, RunStatus::Running, None)
        .await
        .unwrap();
    assert_eq!(updated.status, RunStatus::Running);
    assert!(updated.completed_at.is_none());
}

#[tokio::test]
async fn test_complete_run_with_warnings() {
    let (_tmp, pool) = workspace_pool().await;

    let session = create_session(&pool, sample_context()).await.unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    let completed = complete_run(&pool, &run.id, RunStatus::CompletedWithWarnings, None)
        .await
        .unwrap();

    assert_eq!(completed.status, RunStatus::CompletedWithWarnings);
    assert!(completed.completed_at.is_some());
}

#[tokio::test]
async fn update_run_status_to_completed_with_warnings_sets_completed_at() {
    let (_tmp, pool) = workspace_pool().await;
    let session = create_session(&pool, sample_context()).await.unwrap();
    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    let updated = update_run_status(&pool, &run.id, RunStatus::CompletedWithWarnings, None)
        .await
        .unwrap();

    assert_eq!(updated.status, RunStatus::CompletedWithWarnings);
    assert!(updated.completed_at.is_some());
}

// ---------------------------------------------------------------------------
// Integration: session → messages → runs end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_session_lifecycle() {
    let (_tmp, pool) = workspace_pool().await;

    // 1. Create session
    let session = create_session(&pool, sample_context()).await.unwrap();

    // 2. Add messages
    create_message(
        &pool,
        CreateMessageParams {
            run_id: None,
            session_id: session.id.clone(),
            role: MessageRole::User,
            content: vec![ContentPart::Text {
                text: "Hello".to_string(),
            }],
            provider_metadata: None,
        },
    )
    .await
    .unwrap();

    // 3. Start a run
    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    // 4. Complete the run
    complete_run(&pool, &run.id, RunStatus::Completed, None)
        .await
        .unwrap();

    // 5. Verify everything is linked
    let runs = current_runs(&pool, &session.id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Completed);

    let messages = list_messages(&pool, &session.id).await.unwrap();
    assert_eq!(messages.len(), 1);

    // 6. Delete session — and with it, by ON DELETE CASCADE, its messages.
    // `list_messages` filters on session_id alone and never joins
    // assistant_sessions, so an orphaned row would still come back here.
    delete_session(&pool, &session.id).await.unwrap();
    assert!(get_session(&pool, &session.id).await.unwrap().is_none());
    assert!(
        list_messages(&pool, &session.id).await.unwrap().is_empty(),
        "messages cascade-deleted with their session"
    );
}

#[tokio::test]
async fn test_create_session_and_link_task_links_atomically() {
    let (_tmp, pool) = workspace_pool().await;
    // Insert a workspace_tasks row first (assignTask's order) without a session_id.
    insert_task(&pool, "task-1", "queued", None, None).await;

    let session = create_session_and_link_task(
        &mut pool.acquire().await.unwrap(),
        SessionContext {
            automation_id: Some("manager".to_string()),
            ..sample_context()
        },
        "task-1",
    )
    .await
    .unwrap();

    // The task row must now point at the session, so the anti-join excludes it.
    let linked: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM workspace_tasks WHERE id = ?")
            .bind("task-1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(linked.as_deref(), Some(session.id.as_str()));

    // The session must exist as a normal row (no caller-facing change).
    let fetched = get_session(&pool, &session.id).await.unwrap();
    assert!(fetched.is_some(), "session row exists after commit");

    // And it must never be taken as the manager conversation.
    assert!(
        find_manager_session(&pool, "manager")
            .await
            .unwrap()
            .is_none(),
        "task-linked session excluded by anti-join"
    );
}

// ---------------------------------------------------------------------------
// Message ordering
// ---------------------------------------------------------------------------

/// Same-millisecond rows (a run boundary and the assistant placeholder, or a
/// tool group written in one tick) load in insertion order, not in the order
/// of their random ids.
#[tokio::test]
async fn test_list_messages_orders_same_timestamp_by_insertion() {
    let (_tmp, pool) = workspace_pool().await;
    let session = create_session(&pool, sample_context()).await.unwrap();
    let inserted = ["z-first", "m-second", "a-third"];
    for id in inserted {
        sqlx::query(
            "INSERT INTO assistant_messages (id, session_id, role, content_json, provider_metadata_json, created_at) \
             VALUES (?, ?, 'user', '[]', NULL, 1000)",
        )
        .bind(id)
        .bind(&session.id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let loaded: Vec<String> = list_messages(&pool, &session.id)
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();

    assert_eq!(loaded, inserted);
}

#[tokio::test]
async fn tool_result_lookup_is_session_scoped_and_keeps_full_payload() {
    let (_tmp, pool) = workspace_pool().await;
    let first = create_session(&pool, sample_context()).await.unwrap();
    let second = create_session(&pool, sample_context()).await.unwrap();
    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: first.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn".into(),
            protocol_id: "openai".into(),
            model_id: "model".into(),
            error: None,
        },
    )
    .await
    .unwrap();
    create_tool_call(
        &pool,
        CreateToolCallParams {
            id: "call-1".into(),
            run_id: run.id,
            session_id: first.id.clone(),
            tool_name: "bash_exec".into(),
            params: serde_json::json!({}),
            status: ToolCallStatus::Running,
        },
    )
    .await
    .unwrap();
    let full = serde_json::json!({"stdout": "full output", "exitCode": 0});
    update_tool_call(
        &pool,
        "call-1",
        ToolCallStatus::Completed,
        Some(&full),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        get_tool_call_for_session(&pool, &first.id, "call-1")
            .await
            .unwrap()
            .unwrap()
            .result,
        Some(full)
    );
    assert!(get_tool_call_for_session(&pool, &second.id, "call-1")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn tool_input_lookup_is_session_scoped_and_preserves_stored_copies() {
    use super::display;

    let (_tmp, pool) = workspace_pool().await;
    let mut sessions = Vec::new();
    for _ in 0..2 {
        sessions.push(create_session(&pool, sample_context()).await.unwrap());
    }
    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: sessions[0].id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn".into(),
            protocol_id: "openai".into(),
            model_id: "model".into(),
            error: None,
        },
    )
    .await
    .unwrap();
    let full = serde_json::json!({"path":"report.md", "content":"long content".repeat(10_000)});
    let original = create_tool_call(
        &pool,
        CreateToolCallParams {
            id: "large-input".into(),
            run_id: run.id,
            session_id: sessions[0].id.clone(),
            tool_name: "fs_write".into(),
            params: full.clone(),
            status: ToolCallStatus::Completed,
        },
    )
    .await
    .unwrap();
    create_message(
        &pool,
        CreateMessageParams {
            run_id: None,
            session_id: sessions[0].id.clone(),
            role: MessageRole::Assistant,
            content: vec![ContentPart::ToolUse {
                tool_call_id: original.id.clone(),
                tool_name: original.tool_name.clone(),
                arguments: full.clone(),
            }],
            provider_metadata: None,
        },
    )
    .await
    .unwrap();
    let page = display::page(AssistantMessagePage {
        messages: list_messages(&pool, &sessions[0].id).await.unwrap(),
        tool_calls: list_tool_calls_by_ids(&pool, std::slice::from_ref(&original.id))
            .await
            .unwrap(),
        next_cursor: None,
        has_more: false,
        total_count: 1,
    });
    let page_json = serde_json::to_string(&page).unwrap();
    assert!(!page_json.contains("long content"));
    assert!(page_json.contains("report.md"));
    assert_eq!(
        get_tool_call_for_session(&pool, &sessions[0].id, &original.id)
            .await
            .unwrap()
            .unwrap()
            .params,
        full
    );
    assert!(
        get_tool_call_for_session(&pool, &sessions[1].id, &original.id)
            .await
            .unwrap()
            .is_none()
    );
    let stored = list_messages(&pool, &sessions[0].id).await.unwrap();
    assert!(
        matches!(&stored[0].content[0], ContentPart::ToolUse { arguments, .. } if arguments == &full)
    );
}

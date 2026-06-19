//! Tests for `src/assistant/repository.rs`
//!
//! These tests target the CRUD operations using an in-memory SQLite database.
//! They do not depend on Tauri state or the full app runtime.
//!
//! To integrate: copy to `src/assistant/repository_tests.rs` and add
//! `#[cfg(test)] mod repository_tests;` to `src/assistant/mod.rs`.

use super::repository::*;
use super::types::*;
use crate::config::ExecutionCapabilityConfig;
use crate::db::DbPool;
use sqlx::sqlite::SqlitePoolOptions;

async fn setup_test_pool() -> DbPool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("Failed to create in-memory SQLite pool");

    sqlx::query(
        r#"
        CREATE TABLE assistant_sessions (
            id TEXT PRIMARY KEY,
            tab_id TEXT,
            kind TEXT NOT NULL,
            title TEXT,
            context_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        CREATE TABLE assistant_session_links (
            child_session_id TEXT PRIMARY KEY,
            parent_session_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('rotation')),
            created_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        CREATE TABLE assistant_messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content_json TEXT NOT NULL,
            provider_metadata_json TEXT,
            created_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        CREATE TABLE assistant_runs (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            status TEXT NOT NULL,
            trigger TEXT NOT NULL,
            connection_id TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            model_id TEXT NOT NULL,
            usage_json TEXT,
            error TEXT,
            notices_json TEXT,
            started_at INTEGER NOT NULL,
            completed_at INTEGER
        )
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        CREATE TABLE assistant_message_queue (
            message_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            connection_id TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('pending', 'delivered')),
            queued_at INTEGER NOT NULL,
            delivered_run_id TEXT,
            delivered_at INTEGER
        )
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        CREATE TABLE assistant_tool_calls (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            tool_name TEXT NOT NULL,
            params_json TEXT NOT NULL,
            status TEXT NOT NULL,
            result_json TEXT,
            error TEXT,
            started_at INTEGER NOT NULL,
            completed_at INTEGER
        )
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    pool
}

fn sample_context() -> SessionContext {
    SessionContext {
        space_id: None,
        room_id: None,
        workspace_id: Some("ws-1".to_string()),
        tool_scopes: vec![],
        mcp_server_ids: vec![],
        execution: ExecutionCapabilityConfig::default(),
        cli_session_id: None,
        cli_session_provider: None,
        automation_id: None,
        agent_workspace_id: None,
        automation_name: None,
        inter_agent_call: None,
        workspace_agents: vec![],
    }
}

// ---------------------------------------------------------------------------
// Session CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_and_get_session() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("Test Session".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    assert_eq!(session.kind, SessionKind::Interactive);
    assert_eq!(session.title, Some("Test Session".to_string()));
    assert_eq!(session.context.workspace_id, Some("ws-1".to_string()));
    assert!(session.created_at > 0);

    let fetched = get_session(&pool, &session.id).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_eq!(fetched.id, session.id);
    assert_eq!(fetched.title, session.title);
}

#[tokio::test]
async fn test_get_session_missing_returns_none() {
    let pool = setup_test_pool().await;
    let result = get_session(&pool, "no-such-id").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_list_sessions_ordered_by_updated_at_desc() {
    let pool = setup_test_pool().await;

    let s1 = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("First".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let s2 = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::BackgroundJob,
            title: Some("Second".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let all = list_sessions(&pool).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, s2.id); // newest first
    assert_eq!(all[1].id, s1.id);
}

#[tokio::test]
async fn test_list_non_task_sessions_excludes_task_sessions() {
    let pool = setup_test_pool().await;
    // Minimal stub of the workspace_tasks table the anti-join references.
    sqlx::query("CREATE TABLE workspace_tasks (id TEXT PRIMARY KEY, session_id TEXT)")
        .execute(&pool)
        .await
        .unwrap();

    // The interactive chat (canonical conversation).
    let convo = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("Conversation".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    // A task-delegation session: BackgroundJob + a workspace_tasks row.
    let task = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::BackgroundJob,
            title: Some("Task A".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspace_tasks (id, session_id) VALUES (?, ?)")
        .bind("task-1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();

    // A task with no session yet (NULL session_id) must not nuke the result
    // set via NOT IN / NULL semantics.
    sqlx::query("INSERT INTO workspace_tasks (id, session_id) VALUES (?, NULL)")
        .bind("task-pending")
        .execute(&pool)
        .await
        .unwrap();

    // A non-task BackgroundJob session (e.g. a scheduled-run conversation).
    let scheduled = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::BackgroundJob,
            title: Some("Scheduled".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let result = list_non_task_sessions(&pool).await.unwrap();
    let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
    assert!(
        ids.contains(&convo.id.as_str()),
        "interactive conversation kept"
    );
    assert!(
        ids.contains(&scheduled.id.as_str()),
        "non-task background session kept"
    );
    assert!(
        !ids.contains(&task.id.as_str()),
        "task-linked session excluded"
    );
    assert_eq!(result.len(), 2);
}

#[tokio::test]
async fn test_delete_session() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("ToDelete".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let deleted = delete_session(&pool, &session.id).await.unwrap();
    assert!(deleted);

    let missing = delete_session(&pool, &session.id).await.unwrap();
    assert!(!missing);

    let fetched = get_session(&pool, &session.id).await.unwrap();
    assert!(fetched.is_none());
}

#[tokio::test]
async fn test_create_session_rotation_link_loads_parent() {
    let pool = setup_test_pool().await;

    let parent = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("Parent".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();
    let child = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("Child".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    assert_eq!(parent_session_id(&pool, &child.id).await.unwrap(), None);

    create_session_rotation_link(&pool, &child.id, &parent.id)
        .await
        .unwrap();

    assert_eq!(
        parent_session_id(&pool, &child.id).await.unwrap(),
        Some(parent.id)
    );
}

#[tokio::test]
async fn test_count_session_chain_messages() {
    let pool = setup_test_pool().await;

    // grandparent ← parent ← child rotation chain, with messages at each level.
    let mut chain_ids = Vec::new();
    for title in ["Grandparent", "Parent", "Child"] {
        let session = create_session(
            &pool,
            CreateSessionParams {
                kind: SessionKind::Interactive,
                title: Some(title.to_string()),
                context: sample_context(),
            },
        )
        .await
        .unwrap();
        chain_ids.push(session.id);
    }
    create_session_rotation_link(&pool, &chain_ids[1], &chain_ids[0])
        .await
        .unwrap();
    create_session_rotation_link(&pool, &chain_ids[2], &chain_ids[1])
        .await
        .unwrap();

    // 3 messages in grandparent, 2 in parent, 1 in child.
    for (idx, session_id) in chain_ids.iter().enumerate() {
        for n in 0..(3 - idx) {
            create_message(
                &pool,
                CreateMessageParams {
                    session_id: session_id.clone(),
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
    }

    // Child alone vs. child + ancestors.
    assert_eq!(
        count_session_chain_messages(&pool, &chain_ids[2], false)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        count_session_chain_messages(&pool, &chain_ids[2], true)
            .await
            .unwrap(),
        6
    );
    // Mid-chain: parent + grandparent, not the child below it.
    assert_eq!(
        count_session_chain_messages(&pool, &chain_ids[1], true)
            .await
            .unwrap(),
        5
    );
    // Unknown session: zero, not an error.
    assert_eq!(
        count_session_chain_messages(&pool, "missing", true)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn test_update_session_title_and_context() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("Old".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let mut updated = session.clone();
    updated.title = Some("New".to_string());
    updated.context.workspace_id = Some("ws-2".to_string());

    let result = update_session(&pool, &updated).await.unwrap();
    assert_eq!(result.title, Some("New".to_string()));

    let fetched = get_session(&pool, &session.id).await.unwrap().unwrap();
    assert_eq!(fetched.title, Some("New".to_string()));
    assert_eq!(fetched.context.workspace_id, Some("ws-2".to_string()));
}

// ---------------------------------------------------------------------------
// Message CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_and_list_messages() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let _msg1 = create_message(
        &pool,
        CreateMessageParams {
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
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let msg = create_message(
        &pool,
        CreateMessageParams {
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
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

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

    let pending = list_pending_queued_messages(&pool, &session.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message.id, queued.id);
    assert_eq!(pending[0].connection_id, "conn-1");
    assert_ne!(pending[0].message.id, unqueued.id);

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
            error: None,
        },
    )
    .await
    .unwrap();

    mark_queued_messages_delivered(
        &pool,
        &session.id,
        &run.id,
        std::slice::from_ref(&queued.id),
    )
    .await
    .unwrap();

    assert!(list_pending_queued_messages(&pool, &session.id)
        .await
        .unwrap()
        .is_empty());

    let delivered = list_delivered_queued_messages_for_run(&pool, &session.id, &run.id)
        .await
        .unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message.id, queued.id);
}

#[tokio::test]
async fn test_get_active_run_ignores_terminal_runs() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Completed,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
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
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
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

// ---------------------------------------------------------------------------
// Run CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_and_get_run() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
            error: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.status, RunStatus::Queued);
    assert_eq!(run.trigger, RunTrigger::UserMessage);
    assert_eq!(run.connection_id, "conn-1");
    assert!(run.completed_at.is_none());
    assert!(run.notices.is_empty());

    let fetched = get_run(&pool, &run.id).await.unwrap();
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().id, run.id);
}

#[tokio::test]
async fn test_list_runs_ordered_by_started_at_desc() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let run1 = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Completed,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
            error: None,
        },
    )
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let run2 = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Failed,
            trigger: RunTrigger::Retry,
            connection_id: "c1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
            error: Some("Timeout".to_string()),
        },
    )
    .await
    .unwrap();

    let runs = list_runs(&pool, &session.id).await.unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].id, run2.id); // newest first
    assert_eq!(runs[1].id, run1.id);
}

#[tokio::test]
async fn test_update_run_status_to_terminal_sets_completed_at() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
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
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
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
async fn test_complete_run_with_usage_and_notices() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
            error: None,
        },
    )
    .await
    .unwrap();

    let usage = RunUsage {
        input_tokens: Some(100),
        output_tokens: Some(50),
        reasoning_tokens: None,
        total_tokens: Some(150),
    };
    let notices = vec![RunNotice {
        kind: RunNoticeKind::CommandDenied,
        message: "sudo denied".to_string(),
        timestamp: 1234567890,
    }];

    let completed = complete_run(
        &pool,
        &run.id,
        RunStatus::CompletedWithWarnings,
        Some(&usage),
        None,
        &notices,
    )
    .await
    .unwrap();

    assert_eq!(completed.status, RunStatus::CompletedWithWarnings);
    assert!(completed.usage.is_some());
    let u = completed.usage.unwrap();
    assert_eq!(u.total_tokens, Some(150));
    assert_eq!(completed.notices.len(), 1);
    assert!(completed.completed_at.is_some());
}

#[tokio::test]
async fn test_complete_run_preserves_existing_usage_when_none_passed() {
    let pool = setup_test_pool().await;

    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: "c1".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: Some(RunUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                reasoning_tokens: None,
                total_tokens: Some(15),
            }),
            error: None,
        },
    )
    .await
    .unwrap();

    let completed = complete_run(&pool, &run.id, RunStatus::Completed, None, None, &[])
        .await
        .unwrap();

    assert_eq!(completed.status, RunStatus::Completed);
    assert!(completed.usage.is_some());
    assert_eq!(completed.usage.unwrap().total_tokens, Some(15));
}

// ---------------------------------------------------------------------------
// Integration: session → messages → runs end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_session_lifecycle() {
    let pool = setup_test_pool().await;

    // 1. Create session
    let session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("Demo".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

    // 2. Add messages
    create_message(
        &pool,
        CreateMessageParams {
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
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            usage: None,
            error: None,
        },
    )
    .await
    .unwrap();

    // 4. Complete the run
    complete_run(&pool, &run.id, RunStatus::Completed, None, None, &[])
        .await
        .unwrap();

    // 5. Verify everything is linked
    let runs = list_runs(&pool, &session.id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Completed);

    let messages = list_messages(&pool, &session.id).await.unwrap();
    assert_eq!(messages.len(), 1);

    // 6. Delete session
    delete_session(&pool, &session.id).await.unwrap();
    assert!(get_session(&pool, &session.id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_create_session_and_link_task_links_atomically() {
    let pool = setup_test_pool().await;
    sqlx::query("CREATE TABLE workspace_tasks (id TEXT PRIMARY KEY, session_id TEXT)")
        .execute(&pool)
        .await
        .unwrap();

    // Insert a workspace_tasks row first (assignTask's order) without a session_id.
    sqlx::query("INSERT INTO workspace_tasks (id, session_id) VALUES (?, NULL)")
        .bind("task-1")
        .execute(&pool)
        .await
        .unwrap();

    // The atomic helper should INSERT the session AND stamp session_id in one
    // transaction, so list_non_task_sessions (run before/after) reflects the
    // final state on both sides.
    let session = create_session_and_link_task(
        &pool,
        CreateSessionParams {
            kind: SessionKind::BackgroundJob,
            title: Some("Self-task".to_string()),
            context: sample_context(),
        },
        "task-1",
    )
    .await
    .unwrap();

    // The task row must now point at the session — the anti-join will exclude
    // it, so a concurrent resolver cannot hijack the conversation view.
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

    // And it must be invisible to list_non_task_sessions — the resolver
    // property the whole fix hinges on.
    let non_task_ids: Vec<String> = list_non_task_sessions(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert!(
        !non_task_ids.contains(&session.id),
        "task-linked session excluded by anti-join"
    );
}

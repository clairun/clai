//! Tests for `src/assistant/repository.rs`
//!
//! These tests target the CRUD operations against a real workspace database:
//! a tempdir `data.sqlite` with the embedded `migrations/workspace/` files
//! applied, so the schema — including the foreign keys and cascades — is the
//! one production runs. They do not depend on Tauri state or the full app
//! runtime.

use super::repository::*;
use super::types::*;
use crate::config::ExecutionCapabilityConfig;
use crate::db::test_support::{insert_task, workspace_pool};

fn sample_context() -> SessionContext {
    SessionContext {
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
    let (_tmp, pool) = workspace_pool().await;

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
    let (_tmp, pool) = workspace_pool().await;
    let result = get_session(&pool, "no-such-id").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_list_sessions_ordered_by_updated_at_desc() {
    let (_tmp, pool) = workspace_pool().await;

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
    let (_tmp, pool) = workspace_pool().await;
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
    insert_task(&pool, "task-1", "running", Some(&task.id), None).await;

    // A task with no session yet (NULL session_id) must not nuke the result
    // set via NOT IN / NULL semantics.
    insert_task(&pool, "task-pending", "queued", None, None).await;

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
    let (_tmp, pool) = workspace_pool().await;

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
    let (_tmp, pool) = workspace_pool().await;

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
    let (_tmp, pool) = workspace_pool().await;

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
    let (_tmp, pool) = workspace_pool().await;

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
    let (_tmp, pool) = workspace_pool().await;

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
    let (_tmp, pool) = workspace_pool().await;

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
    let (_tmp, pool) = workspace_pool().await;

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
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
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
    let (_tmp, pool) = workspace_pool().await;

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

    let first = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("first".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();
    let second = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some("second".to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap();

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
    assert!(run.notices.is_empty());

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

#[tokio::test]
async fn test_list_runs_ordered_by_started_at_desc() {
    let (_tmp, pool) = workspace_pool().await;

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
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
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
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
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
    let (_tmp, pool) = workspace_pool().await;

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
async fn test_complete_run_with_notices() {
    let (_tmp, pool) = workspace_pool().await;

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
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    let notices = vec![RunNotice {
        kind: RunNoticeKind::CommandDenied,
        message: "sudo denied".to_string(),
        timestamp: 1234567890,
    }];

    let completed = complete_run(
        &pool,
        &run.id,
        RunStatus::CompletedWithWarnings,
        None,
        &notices,
    )
    .await
    .unwrap();

    assert_eq!(completed.status, RunStatus::CompletedWithWarnings);
    assert_eq!(completed.notices.len(), 1);
    assert!(completed.completed_at.is_some());
}

// ---------------------------------------------------------------------------
// Integration: session → messages → runs end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_session_lifecycle() {
    let (_tmp, pool) = workspace_pool().await;

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
            protocol_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    // 4. Complete the run
    complete_run(&pool, &run.id, RunStatus::Completed, None, &[])
        .await
        .unwrap();

    // 5. Verify everything is linked
    let runs = list_runs(&pool, &session.id).await.unwrap();
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

// ---------------------------------------------------------------------------
// Message ordering and bounded queries
// ---------------------------------------------------------------------------

/// `list_messages` orders by `(created_at, id)`. `list_messages_before` and
/// `list_messages_after` already keyed on that pair; the full loader ordering
/// by `created_at` alone left same-millisecond messages in arbitrary DB order,
/// so a page boundary could split differently from the full load. These tests
/// pin the total order all three loaders share.
async fn message_test_session() -> (tempfile::TempDir, crate::db::DbPool, AssistantSession) {
    let (tmp, pool) = workspace_pool().await;
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
    (tmp, pool, session)
}

fn text_message(session_id: &str, role: MessageRole, text: &str) -> CreateMessageParams {
    CreateMessageParams {
        session_id: session_id.to_string(),
        role,
        content: vec![ContentPart::Text {
            text: text.to_string(),
        }],
        provider_metadata: None,
    }
}

/// Force every message in `messages` to the same `created_at`, so the tests
/// exercise the id tiebreak rather than racing the clock.
async fn force_created_at(pool: &crate::db::DbPool, created_at: i64, ids: &[&str]) {
    for id in ids {
        sqlx::query("UPDATE assistant_messages SET created_at = ? WHERE id = ?")
            .bind(created_at)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn test_list_messages_orders_same_timestamp_by_id() {
    let (_tmp, pool, session) = message_test_session().await;

    let first = create_message(&pool, text_message(&session.id, MessageRole::User, "one"))
        .await
        .unwrap();
    let second = create_message(&pool, text_message(&session.id, MessageRole::User, "two"))
        .await
        .unwrap();
    let third = create_message(&pool, text_message(&session.id, MessageRole::User, "three"))
        .await
        .unwrap();
    force_created_at(&pool, 1_000, &[&first.id, &second.id, &third.id]).await;

    let loaded = list_messages(&pool, &session.id).await.unwrap();
    let mut expected = [first, second, third];
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    let expected_ids: Vec<&str> = expected.iter().map(|m| m.id.as_str()).collect();
    let loaded_ids: Vec<&str> = loaded.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(loaded_ids, expected_ids);

    // The bounded loader keyed on (created_at, id) agrees with the full load,
    // so a window never reorders messages the full load emits in sequence.
    let after = list_messages_after(&pool, &session.id, (1_000, &expected[1].id))
        .await
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, expected[2].id);
}

#[tokio::test]
async fn test_list_messages_after_returns_strictly_later_messages_in_order() {
    let (_tmp, pool, session) = message_test_session().await;

    let early = create_message(&pool, text_message(&session.id, MessageRole::User, "early"))
        .await
        .unwrap();
    let boundary = create_message(
        &pool,
        text_message(&session.id, MessageRole::Assistant, "b"),
    )
    .await
    .unwrap();
    let late_a = create_message(
        &pool,
        text_message(&session.id, MessageRole::User, "late-a"),
    )
    .await
    .unwrap();
    let late_b = create_message(
        &pool,
        text_message(&session.id, MessageRole::User, "late-b"),
    )
    .await
    .unwrap();
    let other_session = create_session(
        &pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .unwrap();
    create_message(
        &pool,
        text_message(&other_session.id, MessageRole::User, "elsewhere"),
    )
    .await
    .unwrap();

    force_created_at(&pool, 1_000, &[&early.id]).await;
    force_created_at(&pool, 2_000, &[&boundary.id]).await;
    force_created_at(&pool, 3_000, &[&late_a.id, &late_b.id]).await;

    let tail = list_messages_after(&pool, &session.id, (2_000, &boundary.id))
        .await
        .unwrap();
    let tail_ids: Vec<&str> = tail.iter().map(|m| m.id.as_str()).collect();
    let mut expected = [late_a, late_b];
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    let expected_ids: Vec<&str> = expected.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(tail_ids, expected_ids);
    assert!(!tail_ids.contains(&early.id.as_str()));
    assert!(!tail_ids.contains(&boundary.id.as_str()));

    // A same-timestamp message only counts as after the boundary when its id
    // is greater: the boundary row must not come back for its own key, and
    // whichever late messages share its timestamp do only when their ids sort
    // after it.
    force_created_at(&pool, 3_000, &[&boundary.id]).await;
    let tail = list_messages_after(&pool, &session.id, (3_000, &boundary.id))
        .await
        .unwrap();
    let tail_ids: Vec<&str> = tail.iter().map(|m| m.id.as_str()).collect();
    assert!(!tail_ids.contains(&boundary.id.as_str()));
    for id in &tail_ids {
        assert!(expected_ids.contains(id));
        assert!(id > &boundary.id.as_str());
    }
    assert!(tail_ids.len() <= expected_ids.len());
}

#[tokio::test]
async fn test_latest_message_by_role_returns_newest_of_role() {
    let (_tmp, pool, session) = message_test_session().await;

    let user_old = create_message(&pool, text_message(&session.id, MessageRole::User, "old"))
        .await
        .unwrap();
    let assistant = create_message(
        &pool,
        text_message(&session.id, MessageRole::Assistant, "a"),
    )
    .await
    .unwrap();
    let user_new = create_message(&pool, text_message(&session.id, MessageRole::User, "new"))
        .await
        .unwrap();
    force_created_at(&pool, 1_000, &[&user_old.id, &assistant.id]).await;
    force_created_at(&pool, 2_000, &[&user_new.id]).await;

    let latest_user = latest_message_by_role(&pool, &session.id, MessageRole::User)
        .await
        .unwrap()
        .expect("a user message exists");
    assert_eq!(latest_user.id, user_new.id);

    // An absent role yields None instead of falling across roles.
    let latest_tool = latest_message_by_role(&pool, &session.id, MessageRole::Tool)
        .await
        .unwrap();
    assert!(latest_tool.is_none());
}

async fn pin_session_max_into_future(pool: &crate::db::DbPool, id: &str) -> i64 {
    // Pin the row's created_at 10s past wall clock, so the next insert can
    // only land past the session max via the bump, never via clock progress.
    let pinned = chrono::Utc::now().timestamp_millis() + 10_000;
    sqlx::query("UPDATE assistant_messages SET created_at = ? WHERE id = ?")
        .bind(pinned)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    pinned
}

#[tokio::test]
async fn test_create_message_bumps_same_millisecond_inserts_into_insertion_order() {
    let (_tmp, pool, session) = message_test_session().await;

    let first = create_message(&pool, text_message(&session.id, MessageRole::User, "one"))
        .await
        .unwrap();
    let pinned_max = pin_session_max_into_future(&pool, &first.id).await;

    let second = create_message(&pool, text_message(&session.id, MessageRole::Tool, "two"))
        .await
        .unwrap();

    // The pinned max sits 10s ahead of wall clock, so only the bump can
    // place this insert strictly past it.
    assert!(second.created_at > pinned_max);
    let loaded = list_messages(&pool, &session.id).await.unwrap();
    let loaded_ids: Vec<&str> = loaded.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(loaded_ids, vec![first.id.as_str(), second.id.as_str()]);
    assert!(loaded[0].created_at < loaded[1].created_at);
}

#[tokio::test]
async fn test_create_user_message_bumps_same_millisecond_inserts_into_insertion_order() {
    let (_tmp, pool, session) = message_test_session().await;

    let first = create_message(
        &pool,
        text_message(&session.id, MessageRole::Assistant, "one"),
    )
    .await
    .unwrap();
    let pinned_max = pin_session_max_into_future(&pool, &first.id).await;

    let second = create_user_message(&pool, session.id.clone(), "two".to_string(), None)
        .await
        .unwrap();

    assert!(second.created_at > pinned_max);
    let loaded = list_messages(&pool, &session.id).await.unwrap();
    let loaded_ids: Vec<&str> = loaded.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(loaded_ids, vec![first.id.as_str(), second.id.as_str()]);
    assert!(loaded[0].created_at < loaded[1].created_at);
}

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
use crate::db::DbPool;

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
// Compaction CRUD
// ---------------------------------------------------------------------------
//
// `assistant_compactions` is the table that decides what the provider is
// allowed to see: `latest_completed_compaction` is the single read behind
// `compaction::provider_history_messages`, and everything it returns causes
// the messages before the compaction window to be withheld from the next
// request. Its `WHERE` clause is therefore not a filter, it is a safety
// interlock — a half-written compaction must stay invisible.

async fn compaction_session(pool: &DbPool, title: &str) -> AssistantSession {
    create_session(
        pool,
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: Some(title.to_string()),
            context: sample_context(),
        },
    )
    .await
    .unwrap()
}

async fn text_message(pool: &DbPool, session_id: &str, text: &str) -> AssistantMessage {
    create_message(
        pool,
        CreateMessageParams {
            session_id: session_id.to_string(),
            role: MessageRole::User,
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            provider_metadata: None,
        },
    )
    .await
    .unwrap()
}

fn compaction_params(
    session_id: &str,
    source_from: Option<&str>,
    source_to: Option<&str>,
) -> CreateCompactionParams {
    CreateCompactionParams {
        session_id: session_id.to_string(),
        trigger: CompactionTrigger::Automatic,
        strategy: CompactionStrategy::LocalSummary,
        source_from_message_id: source_from.map(str::to_string),
        source_to_message_id: source_to.map(str::to_string),
        created_run_id: None,
        protocol_id: "anthropic".to_string(),
        model_id: "claude-sonnet-4".to_string(),
        input_message_count: 7,
    }
}

/// Overwrites the two orderable timestamps directly. `now_ms()` has
/// millisecond resolution and these rows are written microseconds apart, so
/// left alone they tie — and a tie lets SQLite return either row, which would
/// make an ordering assertion pass for the wrong reason. A test that wants a
/// defined ordering has to set both timestamps itself.
async fn force_timestamps(pool: &DbPool, compaction_id: &str, created_at: i64, completed_at: i64) {
    sqlx::query("UPDATE assistant_compactions SET created_at = ?, completed_at = ? WHERE id = ?")
        .bind(created_at)
        .bind(completed_at)
        .bind(compaction_id)
        .execute(pool)
        .await
        .expect("failed to force compaction timestamps");
}

#[tokio::test]
async fn test_create_compaction_round_trips_every_field() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "Compacted").await;
    let first = text_message(&pool, &session.id, "first").await;
    let last = text_message(&pool, &session.id, "last").await;

    // `created_run_id` is an FK to `assistant_runs`, so covering it non-NULL
    // needs a real run. It is worth the four lines: in the INSERT it sits
    // directly after `summary_message_id`, and swapping that pair would write
    // the run id into the summary column, which is exactly the value
    // `latest_completed_compaction` tests for being non-NULL.
    let run = create_run(
        &pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Queued,
            trigger: RunTrigger::UserMessage,
            connection_id: "conn-1".to_string(),
            protocol_id: "anthropic".to_string(),
            model_id: "claude-sonnet-4".to_string(),
            error: None,
        },
    )
    .await
    .unwrap();

    let mut params = compaction_params(&session.id, Some(&first.id), Some(&last.id));
    params.created_run_id = Some(run.id.clone());
    let created = create_compaction(&pool, params).await.unwrap();

    // A freshly created compaction is in flight: nothing terminal is set.
    assert_eq!(created.status, CompactionStatus::Running);
    assert_eq!(created.summary_message_id, None);
    assert_eq!(created.completed_at, None);
    assert_eq!(created.error, None);
    assert!(created.created_at > 0);

    let stored = get_compaction(&pool, &created.id)
        .await
        .unwrap()
        .expect("a created compaction must be readable back");

    // Fifteen bound columns and fifteen mapped columns, asserted one by one.
    // Every column that carries a value here is distinguishable from its
    // neighbours, so a transposed bind is a failure rather than a silent swap.
    // The three that remain NULL (`summary_message_id`, `completed_at`,
    // `error`) are still interchangeable with each other; `complete_compaction`
    // is where they get values, and the test below asserts them there.
    assert_eq!(stored.id, created.id);
    assert_eq!(stored.session_id, session.id);
    assert_eq!(stored.trigger, CompactionTrigger::Automatic);
    assert_eq!(stored.strategy, CompactionStrategy::LocalSummary);
    assert_eq!(stored.status, CompactionStatus::Running);
    assert_eq!(stored.source_from_message_id, Some(first.id));
    assert_eq!(stored.source_to_message_id, Some(last.id));
    assert_eq!(stored.summary_message_id, None);
    assert_eq!(stored.created_run_id, Some(run.id));
    assert_eq!(stored.protocol_id, "anthropic");
    assert_eq!(stored.model_id, "claude-sonnet-4");
    assert_eq!(stored.input_message_count, 7);
    assert_eq!(stored.created_at, created.created_at);
    assert_eq!(stored.completed_at, None);
    assert_eq!(stored.error, None);
}

#[tokio::test]
async fn test_create_compaction_touches_the_session() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "Touched").await;

    // `updated_at` orders the session list; compacting is activity on the
    // session and must move it, or a compacted conversation sinks.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    create_compaction(&pool, compaction_params(&session.id, None, None))
        .await
        .unwrap();

    let refreshed = get_session(&pool, &session.id).await.unwrap().unwrap();
    assert!(
        refreshed.updated_at > session.updated_at,
        "creating a compaction must touch its session: {} !> {}",
        refreshed.updated_at,
        session.updated_at
    );
}

#[tokio::test]
async fn test_create_compaction_requires_an_existing_session() {
    let (_tmp, pool) = workspace_pool().await;

    // `session_id` is a NOT NULL foreign key. This pins two things and not a
    // third: that the constraint exists, and that the pool still enforces
    // foreign keys at all (a connection-level setting — `db/mod.rs` sets
    // `.foreign_keys(true)` on connect). It says nothing about the delete
    // action; the cascade is pinned separately below.
    let error = create_compaction(&pool, compaction_params("no-such-session", None, None))
        .await
        .expect_err("a compaction for an unknown session must not be written");
    assert!(
        error.contains("FOREIGN KEY"),
        "expected a foreign-key violation, got: {error}"
    );
}

#[tokio::test]
async fn test_get_compaction_missing_returns_none() {
    let (_tmp, pool) = workspace_pool().await;
    assert!(get_compaction(&pool, "no-such-compaction")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_complete_compaction_sets_the_summary_and_clears_the_error() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "Completing").await;
    let last = text_message(&pool, &session.id, "last").await;
    let summary = text_message(&pool, &session.id, "the summary").await;

    let created = create_compaction(&pool, compaction_params(&session.id, None, Some(&last.id)))
        .await
        .unwrap();

    // Seed a failed row so the `error = NULL` in the UPDATE is exercised
    // rather than trivially already-null. Be clear about what this is:
    // `CompactionStatus::Failed` is constructed nowhere in the tree, so no
    // production path writes this state and the clause is defensive. The
    // absence is the real finding — a compaction whose summary fails stays
    // `running` forever, across restarts, and nothing sweeps it.
    sqlx::query(
        r#"UPDATE assistant_compactions SET status = '"failed"', error = 'boom' WHERE id = ?"#,
    )
    .bind(&created.id)
    .execute(&pool)
    .await
    .unwrap();

    let before = get_session(&pool, &session.id).await.unwrap().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let completed = complete_compaction(&pool, &created.id, &summary.id)
        .await
        .unwrap();

    assert_eq!(completed.status, CompactionStatus::Completed);
    assert_eq!(completed.summary_message_id, Some(summary.id.clone()));
    assert!(completed.completed_at.is_some());
    assert_eq!(
        completed.error, None,
        "completing must clear the error a previous failure left"
    );

    // The returned value is read back from the row, so what the caller sees
    // and what the next reader sees cannot diverge.
    let stored = get_compaction(&pool, &created.id).await.unwrap().unwrap();
    assert_eq!(stored.status, CompactionStatus::Completed);
    assert_eq!(stored.summary_message_id, Some(summary.id));
    assert_eq!(stored.completed_at, completed.completed_at);
    assert_eq!(stored.error, None);

    // Both ends of a compaction touch the session, not just the create side.
    let after = get_session(&pool, &session.id).await.unwrap().unwrap();
    assert!(
        after.updated_at > before.updated_at,
        "completing a compaction must touch its session: {} !> {}",
        after.updated_at,
        before.updated_at
    );
}

#[tokio::test]
async fn test_complete_compaction_reports_an_unknown_id() {
    let (_tmp, pool) = workspace_pool().await;

    // The UPDATE itself succeeds against zero rows; the error comes from the
    // read-back, which is why the read-back is not merely a convenience.
    let error = complete_compaction(&pool, "no-such-compaction", "msg-1")
        .await
        .expect_err("completing an unknown compaction must fail");
    assert!(
        error.contains("Assistant compaction not found"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn test_latest_completed_compaction_ignores_a_running_one() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "In flight").await;
    let last = text_message(&pool, &session.id, "last").await;

    let summary = text_message(&pool, &session.id, "the summary").await;
    let created = create_compaction(&pool, compaction_params(&session.id, None, Some(&last.id)))
        .await
        .unwrap();

    // Attach the summary while leaving the status `running`, so `status` is
    // the *only* term excluding this row. Without it the row would also be
    // caught by `summary_message_id IS NOT NULL`, and the test would still
    // pass with the status filter deleted — proving nothing. No public path
    // reaches this state, which is the point: it is what a write interrupted
    // between the two UPDATEs would leave behind.
    sqlx::query("UPDATE assistant_compactions SET summary_message_id = ? WHERE id = ?")
        .bind(&summary.id)
        .bind(&created.id)
        .execute(&pool)
        .await
        .unwrap();

    // A compaction whose summary is still being generated must not truncate
    // the history sent to the provider.
    assert!(latest_completed_compaction(&pool, &session.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_latest_completed_compaction_ignores_one_without_a_summary() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "No summary").await;
    let last = text_message(&pool, &session.id, "last").await;
    let summary = text_message(&pool, &session.id, "the summary").await;

    let created = create_compaction(&pool, compaction_params(&session.id, None, Some(&last.id)))
        .await
        .unwrap();
    complete_compaction(&pool, &created.id, &summary.id)
        .await
        .unwrap();

    // Reach the state the way production can: delete the summary message.
    // `summary_message_id` is ON DELETE SET NULL and `delete_message` is
    // reachable from both assistant drivers, so a `completed` row with no
    // summary is a state a user can produce — not a hypothetical the guard
    // defends against. This also pins the third of the table's FK actions.
    delete_message(&pool, &summary.id).await.unwrap();

    let orphaned = get_compaction(&pool, &created.id).await.unwrap().unwrap();
    assert_eq!(orphaned.summary_message_id, None);
    assert_eq!(orphaned.status, CompactionStatus::Completed);
    assert!(latest_completed_compaction(&pool, &session.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_latest_completed_compaction_ignores_one_without_a_window_end() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "No window end").await;
    let summary = text_message(&pool, &session.id, "the summary").await;

    let created = create_compaction(&pool, compaction_params(&session.id, None, None))
        .await
        .unwrap();
    complete_compaction(&pool, &created.id, &summary.id)
        .await
        .unwrap();

    // `source_to_message_id` is the cut point. Without it the reader cannot
    // tell which messages the summary replaced, so the compaction is unusable
    // even though it completed.
    assert!(latest_completed_compaction(&pool, &session.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_latest_completed_compaction_picks_the_newest_completion() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "Twice compacted").await;
    let last = text_message(&pool, &session.id, "last").await;
    let summary = text_message(&pool, &session.id, "the summary").await;

    let older = create_compaction(&pool, compaction_params(&session.id, None, Some(&last.id)))
        .await
        .unwrap();
    complete_compaction(&pool, &older.id, &summary.id)
        .await
        .unwrap();

    let newer = create_compaction(&pool, compaction_params(&session.id, None, Some(&last.id)))
        .await
        .unwrap();
    complete_compaction(&pool, &newer.id, &summary.id)
        .await
        .unwrap();

    // Phase 1: creation order and completion order agree. Kills a flipped
    // sort direction.
    force_timestamps(&pool, &older.id, 1_000, 1_000).await;
    force_timestamps(&pool, &newer.id, 2_000, 2_000).await;
    let latest = latest_completed_compaction(&pool, &session.id)
        .await
        .unwrap()
        .expect("a completed compaction must be found");
    assert_eq!(latest.id, newer.id);

    // Phase 2: point the two orders in opposite directions, with no ties on
    // either column. `created_at` still favours `newer`; `completed_at` now
    // favours `older`. Only `ORDER BY completed_at DESC` gives `older`, so
    // this kills a sort that reads `created_at` — which phase 1 could not,
    // because there the two columns agreed.
    force_timestamps(&pool, &older.id, 1_000, 3_000).await;
    force_timestamps(&pool, &newer.id, 2_000, 2_000).await;
    let latest = latest_completed_compaction(&pool, &session.id)
        .await
        .unwrap()
        .expect("a completed compaction must be found");
    assert_eq!(
        latest.id, older.id,
        "the ordering must be by completion time, not creation time"
    );
}

#[tokio::test]
async fn test_latest_completed_compaction_is_scoped_to_its_session() {
    let (_tmp, pool) = workspace_pool().await;
    let mine = compaction_session(&pool, "Mine").await;
    let theirs = compaction_session(&pool, "Theirs").await;

    let my_last = text_message(&pool, &mine.id, "last").await;
    let my_summary = text_message(&pool, &mine.id, "summary").await;
    let my_compaction =
        create_compaction(&pool, compaction_params(&mine.id, None, Some(&my_last.id)))
            .await
            .unwrap();
    complete_compaction(&pool, &my_compaction.id, &my_summary.id)
        .await
        .unwrap();

    // One session's compaction must never truncate another's history.
    let found = latest_completed_compaction(&pool, &mine.id)
        .await
        .unwrap()
        .expect("the session's own compaction must be found");
    assert_eq!(found.id, my_compaction.id);
    assert!(latest_completed_compaction(&pool, &theirs.id)
        .await
        .unwrap()
        .is_none());
}

#[test]
fn test_completed_status_matches_the_literal_the_query_hardcodes() {
    // The serialised form of this variant is written out by hand in two
    // places no compiler checks: the `status = '"completed"'` literal inside
    // `latest_completed_compaction`, and the CHECK constraint in
    // `20260605000000_assistant_compactions.sql`. Renaming the variant does
    // not fail silently — the next completion violates the CHECK and
    // `map_compaction_row` fails to parse existing rows, both loudly — but it
    // fails at runtime, in a migration-backed test, a long way from the
    // rename. This assertion moves that to the rename itself.
    assert_eq!(
        serde_json::to_string(&CompactionStatus::Completed).unwrap(),
        r#""completed""#
    );
}

#[tokio::test]
async fn test_deleting_a_session_deletes_its_compactions() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "Doomed").await;
    let last = text_message(&pool, &session.id, "last").await;
    let created = create_compaction(&pool, compaction_params(&session.id, None, Some(&last.id)))
        .await
        .unwrap();

    assert!(delete_session(&pool, &session.id).await.unwrap());

    // ON DELETE CASCADE on session_id: a deleted session leaves no compaction
    // rows pointing at a session that no longer exists. Note the limit of
    // this pin and the one above: the fixture builds its schema from the
    // migrations, so they catch an edit to the migration file. SQLite cannot
    // change an FK action without rebuilding the table, so they say nothing
    // about what an already-installed database enforces.
    assert!(get_compaction(&pool, &created.id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_deleting_the_window_end_message_hides_the_compaction() {
    let (_tmp, pool) = workspace_pool().await;
    let session = compaction_session(&pool, "Edited").await;
    let last = text_message(&pool, &session.id, "last").await;
    let summary = text_message(&pool, &session.id, "the summary").await;

    let created = create_compaction(&pool, compaction_params(&session.id, None, Some(&last.id)))
        .await
        .unwrap();
    complete_compaction(&pool, &created.id, &summary.id)
        .await
        .unwrap();
    assert!(latest_completed_compaction(&pool, &session.id)
        .await
        .unwrap()
        .is_some());

    delete_message(&pool, &last.id).await.unwrap();

    // `source_to_message_id` is ON DELETE SET NULL, and the reader skips rows
    // whose window end is NULL. So deleting the last compacted message
    // un-compacts the session: the compaction row survives, the summary
    // message survives, and the next request carries the full history again.
    // Recorded, not asserted as desirable — see the PR for the argument.
    let orphaned = get_compaction(&pool, &created.id).await.unwrap().unwrap();
    assert_eq!(orphaned.source_to_message_id, None);
    assert_eq!(orphaned.status, CompactionStatus::Completed);
    assert!(latest_completed_compaction(&pool, &session.id)
        .await
        .unwrap()
        .is_none());
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

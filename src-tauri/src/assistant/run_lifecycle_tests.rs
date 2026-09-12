//! Wiring tests for `src/assistant/run_lifecycle.rs`.
//!
//! The pure helpers in that module (`final_status`, `completion_event`,
//! `tool_call_update`, `tool_result_metadata`) are tested inline next to the
//! code. What could not be tested there is everything those helpers feed: the
//! row each lifecycle edge writes, the event that follows it, and the order the
//! two happen in. That wiring needed an `AppHandle`, which cannot be built
//! outside a running Tauri app — so it went untested on all four provider
//! paths, and PR #180's review recorded three surviving mutations because of
//! it.
//!
//! `RunLifecycleHost` closes that gap. Here it is implemented by `TestHost`:
//! a real SQLite database with the production schema (the embedded workspace
//! migrations, via `db::init_workspace_db` — not a hand-written copy that can
//! drift) plus an in-memory event log.

use super::run_lifecycle::*;
use super::types::*;
use crate::assistant::engine::{AssistantEngineError, RunTurnInput};
use crate::assistant::events::AssistantUiEvent;
use crate::assistant::repository::{self, CreateRunParams, CreateSessionParams};
use crate::config::ExecutionCapabilityConfig;
use crate::db::DbPool;
use std::sync::Mutex;

/// A `RunLifecycleHost` backed by a real database and a recording event sink.
struct TestHost {
    pool: DbPool,
    /// Every announcement, paired with the run id it was made against, so a
    /// test can pin *which* run the UI was told about and not only what it was
    /// told.
    events: Mutex<Vec<(String, AssistantUiEvent)>>,
    /// Kept alive for the lifetime of the host: dropping it deletes the
    /// workspace directory the pool is reading.
    _dir: tempfile::TempDir,
}

impl RunLifecycleHost for TestHost {
    fn pool(&self) -> &DbPool {
        &self.pool
    }

    fn announce(&self, _session: &AssistantSession, run_id: &str, event: AssistantUiEvent) {
        self.events
            .lock()
            .unwrap()
            .push((run_id.to_string(), event));
    }
}

impl TestHost {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::init_workspace_db(dir.path())
            .await
            .expect("workspace db");
        Self {
            pool,
            events: Mutex::new(Vec::new()),
            _dir: dir,
        }
    }

    /// The recorded events, reduced to their variant names so an assertion can
    /// state the sequence it expects without rebuilding every payload.
    fn event_names(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(_, event)| match event {
                AssistantUiEvent::RunFailed { .. } => "RunFailed",
                AssistantUiEvent::RunCancelled { .. } => "RunCancelled",
                AssistantUiEvent::RunCompleted { .. } => "RunCompleted",
                AssistantUiEvent::ToolCallStarted { .. } => "ToolCallStarted",
                AssistantUiEvent::ToolCallCompleted { .. } => "ToolCallCompleted",
                AssistantUiEvent::ToolCallFailed { .. } => "ToolCallFailed",
                AssistantUiEvent::MessageCreated { .. } => "MessageCreated",
                _ => "other",
            })
            .collect()
    }

    fn events(&self) -> Vec<AssistantUiEvent> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(_, event)| event.clone())
            .collect()
    }

    /// The run id each announcement carried, in order.
    fn event_run_ids(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(run_id, _)| run_id.clone())
            .collect()
    }
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

async fn session(host: &TestHost) -> AssistantSession {
    repository::create_session(
        host.pool(),
        CreateSessionParams {
            kind: SessionKind::Interactive,
            title: None,
            context: sample_context(),
        },
    )
    .await
    .expect("session")
}

async fn run(host: &TestHost, session: &AssistantSession, connection_id: &str) -> AssistantRun {
    repository::create_run(
        host.pool(),
        CreateRunParams {
            session_id: session.id.clone(),
            status: RunStatus::Running,
            trigger: RunTrigger::UserMessage,
            connection_id: connection_id.to_string(),
            protocol_id: "anthropic".to_string(),
            model_id: "claude-x".to_string(),
            error: None,
        },
    )
    .await
    .expect("run")
}

async fn running_tool_call(host: &TestHost, session: &AssistantSession, run_id: &str, id: &str) {
    record_tool_call_started(
        host,
        session,
        run_id,
        id,
        "bash_exec",
        serde_json::json!({ "command": "ls" }),
    )
    .await
    .expect("start tool call");
    host.events.lock().unwrap().clear();
}

async fn reload_run(host: &TestHost, run_id: &str) -> AssistantRun {
    repository::get_run(host.pool(), run_id)
        .await
        .expect("get run")
        .expect("run row")
}

async fn tool_call(host: &TestHost, session: &AssistantSession, id: &str) -> ToolInvocation {
    repository::list_tool_calls(host.pool(), &session.id, None)
        .await
        .expect("tool calls")
        .into_iter()
        .find(|call| call.id == id)
        .expect("tool call row")
}

fn notice() -> RunNotice {
    RunNotice {
        kind: RunNoticeKind::CommandDenied,
        message: "denied".to_string(),
        timestamp: 7,
    }
}

// ---------------------------------------------------------------------------
// Terminal edges
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failing_a_run_closes_its_open_tool_calls_before_the_run_itself() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;
    running_tool_call(&host, &session, &run.id, "call-1").await;

    fail_run(&host, &session, &run.id, "provider exploded")
        .await
        .expect("fail_run");

    // Order is the invariant: a spinner must never outlive the run it belongs
    // to, so the tool call is closed first and the run terminal event last.
    assert_eq!(host.event_names(), vec!["ToolCallFailed", "RunFailed"]);
    // Both announcements name the run they belong to; a UI filtering by run
    // must not be handed an event tagged with someone else's id.
    assert_eq!(host.event_run_ids(), vec![run.id.clone(), run.id.clone()]);

    let call = tool_call(&host, &session, "call-1").await;
    assert!(matches!(call.status, ToolCallStatus::Failed));
    assert_eq!(call.error.as_deref(), Some("provider exploded"));

    let row = reload_run(&host, &run.id).await;
    assert!(matches!(row.status, RunStatus::Failed));
    assert_eq!(row.error.as_deref(), Some("provider exploded"));
}

#[tokio::test]
async fn cancelling_a_run_closes_its_open_tool_calls_with_the_cancellation_reason() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;
    running_tool_call(&host, &session, &run.id, "call-1").await;

    cancel_run(&host, &session, &run.id).await.expect("cancel");

    assert_eq!(host.event_names(), vec!["ToolCallFailed", "RunCancelled"]);

    let call = tool_call(&host, &session, "call-1").await;
    // There is no `ToolCallStatus::Cancelled`: a call interrupted by a
    // cancellation is recorded `Failed`, and only the reason says why.
    assert!(matches!(call.status, ToolCallStatus::Failed));
    assert_eq!(call.error.as_deref(), Some("Run cancelled"));

    let row = reload_run(&host, &run.id).await;
    assert!(matches!(row.status, RunStatus::Cancelled));
    // A cancelled run carries no error string: the user asked for this.
    assert_eq!(row.error, None);
}

#[tokio::test]
async fn a_terminal_edge_leaves_already_finished_tool_calls_alone() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;
    running_tool_call(&host, &session, &run.id, "call-1").await;
    record_tool_call_result(
        &host,
        &session,
        &run.id,
        "call-1",
        ToolCallOutcome::Completed {
            payload: serde_json::json!({ "ok": true }),
        },
        None,
        MissingToolCall::Propagate,
    )
    .await
    .expect("complete tool call");
    host.events.lock().unwrap().clear();

    fail_run(&host, &session, &run.id, "boom")
        .await
        .expect("fail_run");

    // Only the run event: the completed call is not re-closed, and its result
    // is not overwritten with the run's error.
    assert_eq!(host.event_names(), vec!["RunFailed"]);
    let call = tool_call(&host, &session, "call-1").await;
    assert!(matches!(call.status, ToolCallStatus::Completed));
    assert_eq!(call.error, None);
}

#[tokio::test]
async fn notices_ride_along_with_the_completion_they_describe() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;

    complete_run_with_notices(&host, &session, &run.id, &[notice()])
        .await
        .expect("complete");

    assert_eq!(host.event_names(), vec!["RunCompleted"]);
    let row = reload_run(&host, &run.id).await;
    assert!(matches!(row.status, RunStatus::CompletedWithWarnings));
    assert_eq!(row.notices.len(), 1);
    assert_eq!(row.notices[0].message, "denied");

    // The event must carry the same run that was persisted, not a stale copy
    // assembled before the write.
    match &host.events()[0] {
        AssistantUiEvent::RunCompleted { run } => {
            assert!(matches!(run.status, RunStatus::CompletedWithWarnings));
            assert_eq!(run.notices.len(), 1);
        }
        other => panic!("expected RunCompleted, got {:?}", other),
    }
}

#[tokio::test]
async fn a_clean_run_completes_without_warnings() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;

    complete_run_with_notices(&host, &session, &run.id, &[])
        .await
        .expect("complete");

    assert_eq!(host.event_names(), vec!["RunCompleted"]);
    let row = reload_run(&host, &run.id).await;
    assert!(matches!(row.status, RunStatus::Completed));
    assert!(row.notices.is_empty());
}

// ---------------------------------------------------------------------------
// Tool call edges
// ---------------------------------------------------------------------------

#[tokio::test]
async fn starting_a_tool_call_records_it_running_and_announces_the_same_row() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;

    record_tool_call_started(
        &host,
        &session,
        &run.id,
        "call-1",
        "bash_exec",
        serde_json::json!({ "command": "ls" }),
    )
    .await
    .expect("start");

    assert_eq!(host.event_names(), vec!["ToolCallStarted"]);
    match &host.events()[0] {
        AssistantUiEvent::ToolCallStarted { tool_call } => {
            assert_eq!(tool_call.id, "call-1");
            assert_eq!(tool_call.tool_name, "bash_exec");
            assert_eq!(tool_call.run_id, run.id);
            assert!(matches!(tool_call.status, ToolCallStatus::Running));
        }
        other => panic!("expected ToolCallStarted, got {:?}", other),
    }
}

#[tokio::test]
async fn a_completed_call_announces_itself_then_hands_the_payload_back_to_the_provider() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;
    running_tool_call(&host, &session, &run.id, "call-1").await;

    record_tool_call_result(
        &host,
        &session,
        &run.id,
        "call-1",
        ToolCallOutcome::Completed {
            payload: serde_json::json!({ "stdout": "ok" }),
        },
        Some("opencode"),
        MissingToolCall::Propagate,
    )
    .await
    .expect("result");

    assert_eq!(
        host.event_names(),
        vec!["ToolCallCompleted", "MessageCreated"]
    );

    let call = tool_call(&host, &session, "call-1").await;
    assert!(matches!(call.status, ToolCallStatus::Completed));
    assert_eq!(call.result, Some(serde_json::json!({ "stdout": "ok" })));

    let messages = repository::list_messages(host.pool(), &session.id)
        .await
        .expect("messages");
    let tool_message = messages
        .iter()
        .find(|message| matches!(message.role, MessageRole::Tool))
        .expect("tool message");
    assert_eq!(
        tool_message.provider_metadata,
        Some(serde_json::json!({ "source": "opencode" }))
    );
    match &tool_message.content[0] {
        ContentPart::ToolResult {
            tool_call_id,
            payload,
            started_at,
            completed_at,
        } => {
            assert_eq!(tool_call_id, "call-1");
            assert_eq!(payload, &serde_json::json!({ "stdout": "ok" }));
            // These two are read off the row *before* it is moved into the
            // completion event, and nothing downstream reads them today —
            // which is exactly why they need pinning here.
            assert_eq!(*started_at, Some(call.started_at));
            assert_eq!(*completed_at, call.completed_at);
            assert!(call.completed_at.is_some());
        }
        other => panic!("expected a tool result part, got {:?}", other),
    }
}

#[tokio::test]
async fn a_failed_call_is_announced_as_a_failure_and_still_answers_the_provider() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;
    running_tool_call(&host, &session, &run.id, "call-1").await;

    record_tool_call_result(
        &host,
        &session,
        &run.id,
        "call-1",
        ToolCallOutcome::Failed {
            payload: serde_json::json!({ "error": "permission denied" }),
            error: Some("permission denied"),
        },
        None,
        MissingToolCall::Propagate,
    )
    .await
    .expect("result");

    // The row says failed and the event must agree — a row written `Failed`
    // but announced as completed renders as a success that never happened.
    assert_eq!(host.event_names(), vec!["ToolCallFailed", "MessageCreated"]);

    let call = tool_call(&host, &session, "call-1").await;
    assert!(matches!(call.status, ToolCallStatus::Failed));
    assert_eq!(call.error.as_deref(), Some("permission denied"));
    // The payload lives in the message, never in the row's result.
    assert_eq!(call.result, None);

    let messages = repository::list_messages(host.pool(), &session.id)
        .await
        .expect("messages");
    assert!(messages
        .iter()
        .any(|message| matches!(message.role, MessageRole::Tool)));
}

#[tokio::test]
async fn a_result_for_a_call_we_never_recorded_is_dropped_quietly_on_the_cli_paths() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;

    record_tool_call_result(
        &host,
        &session,
        &run.id,
        "never-announced",
        ToolCallOutcome::Completed {
            payload: serde_json::json!({ "ok": true }),
        },
        None,
        MissingToolCall::SkipQuietly,
    )
    .await
    .expect("skipped quietly");

    // Nothing announced and no orphan tool message: the turn survives.
    assert!(host.event_names().is_empty());
    let messages = repository::list_messages(host.pool(), &session.id)
        .await
        .expect("messages");
    assert!(messages.is_empty());
}

#[tokio::test]
async fn the_same_missing_call_fails_the_turn_on_the_api_path() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let run = run(&host, &session, "conn-1").await;

    let result = record_tool_call_result(
        &host,
        &session,
        &run.id,
        "never-announced",
        ToolCallOutcome::Completed {
            payload: serde_json::json!({ "ok": true }),
        },
        None,
        MissingToolCall::Propagate,
    )
    .await;

    assert!(
        result.is_err(),
        "the engine wrote the row itself moments earlier, so a miss is a real fault"
    );
    assert!(host.event_names().is_empty());
}

// ---------------------------------------------------------------------------
// Opening edge
// ---------------------------------------------------------------------------

fn connection(id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.to_string(),
        name: "Test".to_string(),
        protocol_id: "anthropic".to_string(),
        provider_id: "anthropic".to_string(),
        auth_mode: AuthMode::DeveloperApiKey,
        base_url: None,
        secret_ref: "secret".to_string(),
        model_id: "claude-x".to_string(),
        account_label: None,
        enabled: true,
        created_at: 0,
        updated_at: 0,
    }
}

fn turn_input(
    session: &AssistantSession,
    connection_id: &str,
    run_id: Option<&str>,
) -> RunTurnInput {
    RunTurnInput {
        session_id: session.id.clone(),
        run_id: run_id.map(str::to_string),
        trigger: RunTrigger::UserMessage,
        connection_id: connection_id.to_string(),
        cancel_token: tokio_util::sync::CancellationToken::new(),
        inter_agent_call_depth: None,
        trigger_message_id: None,
    }
}

#[tokio::test]
async fn a_turn_without_a_run_opens_one_queued_against_its_connection() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let connection = connection("conn-1");

    let run_id = resolve_run_id(
        &host,
        &session,
        &connection,
        &turn_input(&session, "conn-1", None),
    )
    .await
    .expect("resolve");

    let row = reload_run(&host, &run_id).await;
    // Queued, not Running: the run is opened before the provider is reached,
    // and the caller moves it on once the stream actually starts.
    assert!(matches!(row.status, RunStatus::Queued));
    assert_eq!(row.session_id, session.id);
    assert_eq!(row.connection_id, "conn-1");
    // protocol and model are read off the connection, and are not the same
    // field: swapping them would misattribute every run in the history.
    assert_eq!(row.protocol_id, "anthropic");
    assert_eq!(row.model_id, "claude-x");
    // Opening a run announces nothing here; the caller emits `RunStarted`.
    assert!(host.event_names().is_empty());
}

#[tokio::test]
async fn a_supplied_run_is_reused_rather_than_replaced() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let existing = run(&host, &session, "conn-1").await;

    let run_id = resolve_run_id(
        &host,
        &session,
        &connection("conn-1"),
        &turn_input(&session, "conn-1", Some(&existing.id)),
    )
    .await
    .expect("resolve");

    assert_eq!(run_id, existing.id);
    // Reused, not re-opened: its status is untouched.
    let row = reload_run(&host, &run_id).await;
    assert!(matches!(row.status, RunStatus::Running));
}

#[tokio::test]
async fn continuing_a_run_on_a_different_connection_is_rejected() {
    let host = TestHost::new().await;
    let session = session(&host).await;
    let existing = run(&host, &session, "conn-1").await;

    let error = resolve_run_id(
        &host,
        &session,
        &connection("conn-2"),
        &turn_input(&session, "conn-2", Some(&existing.id)),
    )
    .await
    .expect_err("a run must not be re-pointed at another connection");

    // Rejected by id, so the caller can say which run it was: silently
    // re-pointing it would attribute this turn's model to the wrong row.
    assert!(
        matches!(&error, AssistantEngineError::RunConnectionMismatch(id) if id == &existing.id),
        "expected RunConnectionMismatch, got {:?}",
        error
    );
    let row = reload_run(&host, &existing.id).await;
    assert_eq!(row.connection_id, "conn-1");
}

#[tokio::test]
async fn continuing_a_run_that_does_not_exist_is_a_persistence_error() {
    let host = TestHost::new().await;
    let session = session(&host).await;

    let error = resolve_run_id(
        &host,
        &session,
        &connection("conn-1"),
        &turn_input(&session, "conn-1", Some("no-such-run")),
    )
    .await
    .expect_err("a missing run is not silently re-opened");

    assert!(
        matches!(&error, AssistantEngineError::Persistence(message) if message.contains("no-such-run")),
        "expected a Persistence error naming the run, got {:?}",
        error
    );
}

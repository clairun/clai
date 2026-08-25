//! Run lifecycle helpers shared by the API engine and the CLI local agent.
//!
//! Both `engine::run_session_turn` and `local_agent::run_session_turn` implement
//! the same contract — `(AssistantDeps, RunTurnInput) -> Result<(),
//! AssistantEngineError>` while emitting one `AssistantUiEvent` stream — so the
//! bookkeeping at every edge of a run — open it, and close it as failed,
//! cancelled or complete — has to behave identically on both paths. All four
//! edges used to be written twice: `fail_run` and `cancel_run` existed
//! character-for-character in each file, `resolve_run_id` existed as a function
//! in `local_agent` and as an inline `match` in `engine`, and the completion
//! edge was an inline copy in each. This module is the single copy.
//!
//! These are the state transitions, not the decisions that trigger them: each
//! function persists one run-state change and emits the matching UI event, and
//! the callers decide *when* a run is failed, cancelled or complete. Callers
//! also own the assistant message — a caller that streamed content finalizes it
//! before closing the run, so nothing here has to guess what was written.

use crate::assistant::engine::{AssistantDeps, AssistantEngineError, RunTurnInput};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository::{
    self, CreateMessageParams, CreateRunParams, CreateToolCallParams,
};
use crate::assistant::types::{
    AssistantSession, ContentPart, MessageRole, ProviderConnection, RunNotice, RunStatus, RunUsage,
    ToolCallStatus, ToolInvocation,
};
use serde_json::Value;

/// Reuse the run the caller was handed, or open a new one.
///
/// A supplied `run_id` must belong to the same connection the turn is running
/// on: continuing a run against a different connection would attribute the new
/// turn's usage and model to the wrong row, so it is rejected as
/// `RunConnectionMismatch` rather than silently re-pointed.
pub(crate) async fn resolve_run_id(
    deps: &AssistantDeps,
    session: &AssistantSession,
    connection: &ProviderConnection,
    input: &RunTurnInput,
) -> Result<String, AssistantEngineError> {
    match &input.run_id {
        Some(id) => {
            let existing_run = repository::get_run(&deps.pool, id).await?.ok_or_else(|| {
                AssistantEngineError::Persistence(format!("run not found: {}", id))
            })?;
            if existing_run.connection_id != input.connection_id {
                return Err(AssistantEngineError::RunConnectionMismatch(id.clone()));
            }
            Ok(id.clone())
        }
        None => {
            let run = repository::create_run(
                &deps.pool,
                CreateRunParams {
                    session_id: session.id.clone(),
                    status: RunStatus::Queued,
                    trigger: input.trigger.clone(),
                    connection_id: connection.id.clone(),
                    protocol_id: connection.protocol_id.clone(),
                    model_id: connection.model_id.clone(),
                    usage: None,
                    error: None,
                },
            )
            .await?;
            Ok(run.id)
        }
    }
}

/// Mark a run failed, failing any tool call still marked running under it.
///
/// The tool calls are closed first so the UI never keeps a spinner alive under
/// a terminal run.
pub(crate) async fn fail_run(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
    error_msg: &str,
) -> Result<(), AssistantEngineError> {
    for tool_call in
        repository::fail_running_tool_calls_for_run(&deps.pool, run_id, error_msg).await?
    {
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::ToolCallFailed { tool_call },
        );
    }
    let run = repository::complete_run(
        &deps.pool,
        run_id,
        RunStatus::Failed,
        usage,
        Some(error_msg),
        &[],
    )
    .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::RunFailed { run },
    );
    Ok(())
}

/// Mark a run cancelled. The assistant message is *not* touched here: every
/// caller finalizes it first, so cancelling never has to guess what was
/// streamed. (This used to take a `_message_id` it ignored, which is exactly
/// the shape a silent data-loss bug takes.)
pub(crate) async fn cancel_run(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
) -> Result<(), AssistantEngineError> {
    for tool_call in
        repository::fail_running_tool_calls_for_run(&deps.pool, run_id, "Run cancelled").await?
    {
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::ToolCallFailed { tool_call },
        );
    }
    let run = repository::complete_run(&deps.pool, run_id, RunStatus::Cancelled, usage, None, &[])
        .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::RunCancelled { run },
    );
    Ok(())
}

/// A run that produced notices completes *with warnings*.
///
/// Notices are the record of something the user should know about but that did
/// not fail the turn — a denied command, an unavailable sandbox, a granted
/// path. The distinction is only visible through this function, so it is kept
/// pure and tested rather than re-derived at each completion site.
pub(crate) fn final_status(notices: &[RunNotice]) -> RunStatus {
    if notices.is_empty() {
        RunStatus::Completed
    } else {
        RunStatus::CompletedWithWarnings
    }
}

/// Close a run that finished on its own terms, carrying its notices.
pub(crate) async fn complete_run_with_notices(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
    notices: &[RunNotice],
) -> Result<(), AssistantEngineError> {
    let run = repository::complete_run(
        &deps.pool,
        run_id,
        final_status(notices),
        usage,
        None,
        notices,
    )
    .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::RunCompleted { run },
    );
    Ok(())
}

/// Persist a tool call as `Running` and announce it to the UI.
///
/// Every provider path reaches this point differently — the API engine has a
/// `ToolInvocationDraft` it is about to execute itself, while each CLI path
/// scrapes an id, a name and an argument blob out of a provider-specific
/// stream envelope — but from here on the record and the event are the same on
/// all four, so this is where they converge.
pub(crate) async fn record_tool_call_started(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    params: Value,
) -> Result<(), String> {
    let invocation = repository::create_tool_call(
        &deps.pool,
        CreateToolCallParams {
            id: tool_call_id.to_string(),
            run_id: run_id.to_string(),
            session_id: session.id.clone(),
            tool_name: tool_name.to_string(),
            params,
            status: ToolCallStatus::Running,
        },
    )
    .await?;

    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::ToolCallStarted {
            tool_call: invocation,
        },
    );

    Ok(())
}

/// How a tool call ended, in the only two shapes the record can take.
///
/// The two destinations have different audiences, which is why they take
/// different values. `payload` goes into the tool-role *message*, which the
/// chat never renders (`ChatMessageList.tsx` hides every tool-role message) —
/// it is what the **provider** replays on the next request, so a failure still
/// carries a body explaining itself (an `{"error": …}` object on most paths,
/// the provider's own error block on the Claude path). The `tool_call` **row**
/// is what the *user* sees, inline under the call, and `error` is the
/// human-readable string it shows; it is optional because a provider can report
/// a failed status without saying why.
pub(crate) enum ToolCallOutcome<'a> {
    Completed {
        payload: Value,
    },
    Failed {
        payload: Value,
        error: Option<&'a str>,
    },
}

/// Split an outcome into the three arguments `update_tool_call` wants.
///
/// The rule that matters and is easy to get wrong on a fifth copy: the stored
/// *result* is only ever set on success. A failed call keeps its payload in the
/// tool message for the user to read, but the row itself carries the error, not
/// a result.
fn tool_call_update<'a>(
    outcome: &'a ToolCallOutcome<'_>,
) -> (ToolCallStatus, Option<&'a Value>, Option<&'a str>) {
    match outcome {
        ToolCallOutcome::Completed { payload } => (ToolCallStatus::Completed, Some(payload), None),
        ToolCallOutcome::Failed { error, .. } => (ToolCallStatus::Failed, None, *error),
    }
}

/// What to do when the `tool_call` row cannot be updated.
pub(crate) enum MissingToolCall {
    /// Fail the caller. The API engine wrote the row itself moments earlier, so
    /// a failure here is a genuine persistence fault, not a race.
    Propagate,
    /// Warn and record nothing further. The CLI paths update rows they may
    /// never have created: a provider can emit a result for a tool_use that its
    /// own stream never announced, and losing that one message is much better
    /// than failing the user's turn over it.
    SkipQuietly,
}

/// Which UI event closes a tool call out.
///
/// The row and the event have to agree: a row written as `Failed` that is
/// announced as `ToolCallCompleted` renders as a success that never happened,
/// and nothing downstream would catch it. Deriving both from the same status
/// here is what keeps them in step.
fn completion_event(status: ToolCallStatus, tool_call: ToolInvocation) -> AssistantUiEvent {
    match status {
        ToolCallStatus::Failed => AssistantUiEvent::ToolCallFailed { tool_call },
        _ => AssistantUiEvent::ToolCallCompleted { tool_call },
    }
}

/// Tag the tool message with the CLI runtime that produced it.
///
/// The API path has no runtime to name and passes `None`. The key is `source`,
/// the same one `compaction.rs` tags its own generated messages with.
fn tool_result_metadata(metadata_source: Option<&str>) -> Option<Value> {
    metadata_source.map(|source| serde_json::json!({ "source": source }))
}

/// Close a tool call out: update the row, emit the matching completion event,
/// and persist the tool-role message that carries the payload back to the
/// provider on the next request.
///
/// Does nothing beyond a warning when the row cannot be updated under
/// `MissingToolCall::SkipQuietly`.
pub(crate) async fn record_tool_call_result(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    tool_call_id: &str,
    outcome: ToolCallOutcome<'_>,
    metadata_source: Option<&str>,
    on_missing: MissingToolCall,
) -> Result<(), String> {
    let (status, result, error) = tool_call_update(&outcome);
    let updated =
        match repository::update_tool_call(&deps.pool, tool_call_id, status.clone(), result, error)
            .await
        {
            Ok(tool_call) => tool_call,
            Err(err) => match on_missing {
                MissingToolCall::Propagate => return Err(err),
                MissingToolCall::SkipQuietly => {
                    tracing::warn!(
                        tool_call_id = %tool_call_id,
                        source = ?metadata_source,
                        error = %err,
                        "Tool call update failed even after the tool_use was registered"
                    );
                    return Ok(());
                }
            },
        };

    let started_at = updated.started_at;
    let completed_at = updated.completed_at;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        completion_event(status, updated),
    );

    let payload = match outcome {
        ToolCallOutcome::Completed { payload } | ToolCallOutcome::Failed { payload, .. } => payload,
    };
    let message = repository::create_message(
        &deps.pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                payload,
                started_at: Some(started_at),
                completed_at,
            }],
            provider_metadata: tool_result_metadata(metadata_source),
        },
    )
    .await?;

    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::MessageCreated { message },
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::RunNoticeKind;

    fn notice() -> RunNotice {
        RunNotice {
            kind: RunNoticeKind::CommandDenied,
            message: "denied".to_string(),
            timestamp: 0,
        }
    }

    #[test]
    fn a_run_without_notices_completes_cleanly() {
        assert!(matches!(final_status(&[]), RunStatus::Completed));
    }

    fn invocation(status: ToolCallStatus) -> ToolInvocation {
        ToolInvocation {
            id: "call_a".to_string(),
            run_id: "run_a".to_string(),
            session_id: "session_a".to_string(),
            tool_name: "bash_exec".to_string(),
            params: serde_json::json!({}),
            status,
            result: None,
            error: None,
            started_at: 0,
            completed_at: None,
        }
    }

    #[test]
    fn a_failed_row_is_announced_as_a_failure() {
        let event = completion_event(ToolCallStatus::Failed, invocation(ToolCallStatus::Failed));
        assert!(matches!(event, AssistantUiEvent::ToolCallFailed { .. }));
    }

    #[test]
    fn a_completed_row_is_announced_as_a_completion() {
        let event = completion_event(
            ToolCallStatus::Completed,
            invocation(ToolCallStatus::Completed),
        );
        assert!(matches!(event, AssistantUiEvent::ToolCallCompleted { .. }));
    }

    #[test]
    fn a_cli_runtime_tags_the_tool_message_with_its_source() {
        assert_eq!(
            tool_result_metadata(Some("opencode")),
            Some(serde_json::json!({ "source": "opencode" }))
        );
    }

    #[test]
    fn the_api_path_leaves_the_tool_message_untagged() {
        assert_eq!(tool_result_metadata(None), None);
    }

    #[test]
    fn a_completed_call_stores_its_payload_as_the_result() {
        let outcome = ToolCallOutcome::Completed {
            payload: serde_json::json!({"ok": true}),
        };
        let (status, result, error) = tool_call_update(&outcome);
        assert!(matches!(status, ToolCallStatus::Completed));
        assert_eq!(result, Some(&serde_json::json!({"ok": true})));
        assert_eq!(error, None);
    }

    #[test]
    fn a_failed_call_stores_no_result_even_though_it_has_a_payload() {
        let outcome = ToolCallOutcome::Failed {
            payload: serde_json::json!({"error": "boom"}),
            error: Some("boom"),
        };
        let (status, result, error) = tool_call_update(&outcome);
        assert!(matches!(status, ToolCallStatus::Failed));
        assert_eq!(result, None);
        assert_eq!(error, Some("boom"));
    }

    #[test]
    fn a_failure_without_a_reason_is_still_a_failure() {
        let outcome = ToolCallOutcome::Failed {
            payload: serde_json::json!({"error": "OpenCode tool execution failed"}),
            error: None,
        };
        let (status, result, error) = tool_call_update(&outcome);
        assert!(matches!(status, ToolCallStatus::Failed));
        assert_eq!(result, None);
        assert_eq!(error, None);
    }

    #[test]
    fn a_single_notice_is_enough_to_complete_with_warnings() {
        assert!(matches!(
            final_status(&[notice()]),
            RunStatus::CompletedWithWarnings
        ));
    }
}

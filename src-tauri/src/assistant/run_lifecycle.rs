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
use crate::assistant::repository::{self, CreateRunParams};
use crate::assistant::types::{
    AssistantSession, ProviderConnection, RunNotice, RunStatus, RunUsage,
};

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

    #[test]
    fn a_single_notice_is_enough_to_complete_with_warnings() {
        assert!(matches!(
            final_status(&[notice()]),
            RunStatus::CompletedWithWarnings
        ));
    }
}

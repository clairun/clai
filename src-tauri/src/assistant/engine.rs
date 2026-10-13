use std::sync::Arc;
use tauri::{AppHandle, Manager};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::providers;
use crate::assistant::providers::types::ProviderError;
use crate::assistant::repository;
use crate::assistant::run_context::resolve_run_context;
use crate::assistant::run_lifecycle;
use crate::assistant::turn_common::discard_unanswered_run_input_with_sink;
use crate::assistant::types::{AssistantSession, RunId, RunTrigger, SessionId};
use crate::assistant::{api_turn, local_agent};
use crate::db::DbPool;
use crate::AppState;

#[derive(Clone)]
pub struct AssistantDeps {
    /// The workspace that owns `pool`.
    pub workspace_id: String,
    pub pool: DbPool,
    pub app: AppHandle,
}

impl AssistantDeps {
    /// [`emit_event`] for this workspace.
    pub fn emit(
        &self,
        session: &AssistantSession,
        run_id: Option<&str>,
        event: AssistantUiEvent,
    ) -> Result<(), String> {
        emit_event(&self.app, &self.workspace_id, session, run_id, event)
    }
}

#[derive(Debug, Clone)]
pub struct RunTurnInput {
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub trigger: RunTrigger,
    pub connection_id: String,
    pub cancel_token: CancellationToken,
}

#[derive(Debug, Error)]
pub enum AssistantEngineError {
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("provider not configured: {0}")]
    ProviderNotConfigured(String),
    #[error("run connection mismatch for run {0}")]
    RunConnectionMismatch(String),
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
    #[error("persistence error: {0}")]
    Persistence(String),
    /// The run's configuration could not be resolved; the message is shown as is.
    #[error("{0}")]
    RunConfig(String),
}

impl From<String> for AssistantEngineError {
    fn from(s: String) -> Self {
        AssistantEngineError::Persistence(s)
    }
}

pub async fn run_session_turn(
    deps: &AssistantDeps,
    input: RunTurnInput,
) -> Result<(), AssistantEngineError> {
    let session = repository::get_session(&deps.pool, &input.session_id)
        .await?
        .ok_or_else(|| AssistantEngineError::SessionNotFound(input.session_id.clone()))?;
    let app_state = deps.app.try_state::<AppState>();
    let connection = app_state.as_ref().and_then(|state| {
        state
            .config_manager
            .lock()
            .ok()?
            .get_provider_connection(&input.connection_id)
    });
    let Some(connection) = connection else {
        let error = AssistantEngineError::ProviderNotConfigured(input.connection_id.clone());
        return fail_before_start(deps, &session, &input, error).await;
    };
    let run_context = match app_state.as_ref() {
        Some(state) => resolve_run_context(
            state.inner(),
            &deps.workspace_id,
            session.context.automation_id.as_deref(),
        ),
        None => Err("application state is not available".to_string()),
    };
    let run_context = match run_context {
        Ok(run_context) => Arc::new(run_context),
        Err(error) => {
            let error = AssistantEngineError::RunConfig(error);
            return fail_before_start(deps, &session, &input, error).await;
        }
    };

    if providers::is_cli_provider(&connection.protocol_id) {
        local_agent::run_session_turn(deps, input, session, run_context, connection).await
    } else {
        api_turn::run_session_turn(deps, input, session, &run_context, connection).await
    }
}

/// Close a run that could not start, so its row does not stay `queued`, and
/// drop the user message that triggered it, as every other failure before the
/// provider answered does (`discard_unanswered_run_input`).
async fn fail_before_start(
    deps: &AssistantDeps,
    session: &AssistantSession,
    input: &RunTurnInput,
    error: AssistantEngineError,
) -> Result<(), AssistantEngineError> {
    close_unstarted_run(&deps.pool, &session.id, input, error, |run_id, event| {
        let _ = deps.emit(session, Some(run_id), event);
    })
    .await
}

/// Failing to record the start failure is only logged: the start failure is
/// what the caller must see.
async fn close_unstarted_run(
    pool: &DbPool,
    session_id: &str,
    input: &RunTurnInput,
    error: AssistantEngineError,
    mut emit: impl FnMut(&str, AssistantUiEvent),
) -> Result<(), AssistantEngineError> {
    let Some(run_id) = input.run_id.as_deref() else {
        return Err(error);
    };
    let message = error.to_string();
    if let Err(close_error) =
        run_lifecycle::fail_run_with_sink(pool, run_id, &message, |event| emit(run_id, event)).await
    {
        tracing::warn!(run_id, error = %close_error, "could not mark unstarted run failed");
    }
    discard_unanswered_run_input_with_sink(pool, session_id, run_id, None, |message_id| {
        emit(run_id, AssistantUiEvent::MessageDeleted { message_id });
    })
    .await;
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::repository::CreateRunParams;
    use crate::assistant::types::{ContentPart, RunStatus, SessionContext};
    use crate::db::test_support::workspace_pool;

    fn input(run_id: &str) -> RunTurnInput {
        RunTurnInput {
            session_id: String::new(),
            run_id: Some(run_id.to_string()),
            trigger: RunTrigger::UserMessage,
            connection_id: "connection".into(),
            cancel_token: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn start_failure_fails_the_queued_run_and_returns_the_original_error() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();
        let run = repository::create_run(
            &pool,
            CreateRunParams {
                session_id: session.id.clone(),
                status: RunStatus::Queued,
                trigger: RunTrigger::UserMessage,
                connection_id: "connection".into(),
                protocol_id: "openai".into(),
                model_id: "model".into(),
                error: None,
            },
        )
        .await
        .unwrap();
        let trigger = repository::create_user_message_with_content(
            &pool,
            session.id.clone(),
            vec![ContentPart::Text {
                text: "hello".into(),
            }],
            Some("connection"),
        )
        .await
        .unwrap();
        crate::assistant::signals::consume(
            &pool,
            &session.id,
            &run.id,
            std::slice::from_ref(&trigger.id),
        )
        .await
        .unwrap();
        let start_error = || AssistantEngineError::RunConfig("agent removed".into());

        let mut events = Vec::new();
        let result = close_unstarted_run(
            &pool,
            &session.id,
            &input(&run.id),
            start_error(),
            |_, event| events.push(event),
        )
        .await;

        assert_eq!(result.unwrap_err().to_string(), start_error().to_string());
        let stored = repository::get_run(&pool, &run.id).await.unwrap().unwrap();
        assert!(matches!(stored.status, RunStatus::Failed));
        assert_eq!(stored.error.as_deref(), Some("agent removed"));
        assert!(repository::get_message(&pool, &trigger.id)
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            events.as_slice(),
            [
                AssistantUiEvent::RunFailed { .. },
                AssistantUiEvent::MessageDeleted { .. }
            ]
        ));

        let unknown = close_unstarted_run(
            &pool,
            &session.id,
            &input("missing-run"),
            start_error(),
            |_, _| {},
        )
        .await;
        assert_eq!(unknown.unwrap_err().to_string(), start_error().to_string());
    }
}

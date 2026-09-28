use std::path::PathBuf;

use async_trait::async_trait;
use tauri::{AppHandle, Manager};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::assistant::api_turn::ApiTurnRunner;
use crate::assistant::local_agent::CliTurnRunner;
use crate::assistant::providers;
use crate::assistant::providers::types::ProviderError;
use crate::assistant::repository;
use crate::assistant::types::{AssistantSession, ProviderConnection, RunId, RunTrigger, SessionId};
use crate::db::DbPool;
use crate::AppState;

#[derive(Clone)]
pub struct AssistantDeps {
    pub pool: DbPool,
    pub app: AppHandle,
}

#[derive(Debug, Clone)]
pub struct RunTurnInput {
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub trigger: RunTrigger,
    pub connection_id: String,
    pub cancel_token: CancellationToken,
    pub inter_agent_call_depth: Option<u32>,
    /// Id of the user message that triggered this run (Some only for the
    /// direct send path). If the run fails before the provider produces
    /// anything, this message is discarded — see
    /// `discard_unanswered_run_input`.
    pub trigger_message_id: Option<String>,
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
}

impl From<String> for AssistantEngineError {
    fn from(s: String) -> Self {
        AssistantEngineError::Persistence(s)
    }
}

pub struct TurnTarget {
    pub(crate) session: AssistantSession,
    pub(crate) connection: ProviderConnection,
    pub(crate) workspace_root: Option<PathBuf>,
}

async fn load_turn_target(
    deps: &AssistantDeps,
    input: &RunTurnInput,
) -> Result<TurnTarget, AssistantEngineError> {
    let session = repository::get_session(&deps.pool, &input.session_id)
        .await?
        .ok_or_else(|| AssistantEngineError::SessionNotFound(input.session_id.clone()))?;

    let app_state = deps.app.try_state::<AppState>();
    let connection = app_state
        .as_ref()
        .and_then(|state| {
            state
                .config_manager
                .lock()
                .ok()?
                .get_provider_connection(&input.connection_id)
        })
        .ok_or_else(|| AssistantEngineError::ProviderNotConfigured(input.connection_id.clone()))?;
    let workspace_root = match session.context.agent_workspace_id.as_deref() {
        Some(workspace_id) => {
            let root = app_state
                .as_ref()
                .and_then(|state| state.workspace_root(workspace_id));
            if root.is_none() {
                return Err(AssistantEngineError::Persistence(format!(
                    "workspace {} no longer exists or failed to load",
                    workspace_id
                )));
            }
            root
        }
        None => None,
    };
    Ok(TurnTarget {
        session,
        connection,
        workspace_root,
    })
}

#[async_trait]
pub trait TurnRunner: Send + Sync {
    async fn run_session_turn(
        &self,
        deps: &AssistantDeps,
        input: RunTurnInput,
        target: TurnTarget,
    ) -> Result<(), AssistantEngineError>;
}

fn turn_runner_for(connection: &ProviderConnection) -> &'static dyn TurnRunner {
    static API: ApiTurnRunner = ApiTurnRunner;
    static CLI: CliTurnRunner = CliTurnRunner;
    if providers::is_cli_provider(&connection.protocol_id) {
        &CLI
    } else {
        &API
    }
}

pub async fn run_session_turn(
    deps: &AssistantDeps,
    input: RunTurnInput,
) -> Result<(), AssistantEngineError> {
    let target = load_turn_target(deps, &input).await?;
    turn_runner_for(&target.connection)
        .run_session_turn(deps, input, target)
        .await
}

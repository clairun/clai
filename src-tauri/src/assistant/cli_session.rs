use crate::assistant::repository;
use crate::assistant::types::AssistantSession;
use crate::db::DbPool;

pub async fn reset_cli_session_for_rotation(
    pool: &DbPool,
    session: &mut AssistantSession,
) -> Result<(), String> {
    if session.context.cli_session_id.is_none() && session.context.cli_session_provider.is_none() {
        return Ok(());
    }
    session.context.cli_session_id = None;
    session.context.cli_session_provider = None;
    session.context.cli_tool_exposure = None;
    session.updated_at = chrono::Utc::now().timestamp_millis();
    *session = repository::update_session(pool, session).await?;
    Ok(())
}

/// The thread a Codex turn runs in: the one to resume, if any, and the tool
/// exposure (see `codex_app_server::CODEX_TOOL_EXPOSURE`) recorded with
/// whichever thread the turn ends up in.
pub struct CodexThread {
    resume: Option<String>,
    tool_exposure: Option<&'static str>,
}

impl CodexThread {
    pub fn to_resume(&self) -> Option<&str> {
        self.resume.as_deref()
    }

    pub fn tool_exposure(&self) -> Option<&'static str> {
        self.tool_exposure
    }
}

/// The thread for a Codex turn whose CLAI tools are exposed as
/// `tool_exposure`. A thread opened with another exposure is dropped like a
/// rotated one, so the new thread is seeded from CLAI's conversation.
pub async fn codex_thread(
    pool: &DbPool,
    session: &mut AssistantSession,
    tool_exposure: Option<&'static str>,
) -> Result<CodexThread, String> {
    if session.context.cli_session_id.is_some()
        && session.context.cli_tool_exposure.as_deref() != tool_exposure
    {
        tracing::info!(
            target: "clai::cli_session",
            session_id = %session.id,
            from = ?session.context.cli_tool_exposure,
            to = ?tool_exposure,
            "CLAI tool exposure changed; starting a new Codex thread"
        );
        reset_cli_session_for_rotation(pool, session).await?;
    }
    Ok(CodexThread {
        resume: session.context.cli_session_id.clone(),
        tool_exposure,
    })
}

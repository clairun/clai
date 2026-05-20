use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::assistant::types::{
    AssistantMessage, AssistantRun, AssistantSession, MessageId, RunId, ToolInvocation,
};

pub const ASSISTANT_EVENT_NAME: &str = "assistant://event";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AssistantUiEvent {
    SessionCreated { session: Box<AssistantSession> },
    MessageCreated { message: AssistantMessage },
    RunQueued { run: AssistantRun },
    RunStarted { run: AssistantRun },
    AssistantDelta { message_id: MessageId, text: String },
    AssistantMessageCompleted { message: AssistantMessage },
    ToolCallStarted { tool_call: ToolInvocation },
    ToolCallCompleted { tool_call: ToolInvocation },
    ToolCallFailed { tool_call: ToolInvocation },
    RunCompleted { run: AssistantRun },
    RunFailed { run: AssistantRun },
    RunCancelled { run: AssistantRun },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantEventEnvelope {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    /// The workspace this session belongs to. Lets the frontend map
    /// run-lifecycle events (RunStarted/RunCompleted) back to a
    /// workspace card without having to track session→workspace itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub timestamp: i64,
    pub event: AssistantUiEvent,
}

pub fn emit_event(
    app: &AppHandle,
    session: &AssistantSession,
    run_id: Option<&str>,
    event: AssistantUiEvent,
) -> Result<(), String> {
    let envelope = AssistantEventEnvelope {
        session_id: session.id.clone(),
        run_id: run_id.map(str::to_string),
        tab_id: session.tab_id.clone(),
        workspace_id: session.context.workspace_id.clone(),
        timestamp: Utc::now().timestamp_millis(),
        event,
    };

    app.emit(ASSISTANT_EVENT_NAME, envelope)
        .map_err(|e| format!("Failed to emit assistant event: {}", e))
}

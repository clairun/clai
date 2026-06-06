use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

use crate::assistant::tools::ask_user::AskUserOption;
use crate::assistant::types::{
    AssistantCompaction, AssistantMessage, AssistantRun, AssistantSession, MessageId, RunId,
    ToolInvocation,
};

pub const ASSISTANT_EVENT_NAME: &str = "assistant://event";

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum AssistantUiEvent {
    SessionCreated {
        session: Box<AssistantSession>,
    },
    MessageCreated {
        message: AssistantMessage,
    },
    /// A message was removed from the conversation: a user message whose
    /// run failed before the provider produced anything (connection error,
    /// usage limit, CLI spawn failure), the empty assistant placeholder
    /// created for that run, or a queued message the user deleted before
    /// pickup. The FE drops it from session state — there's no point
    /// showing a message that never got an answer; the typed text stays
    /// recoverable via the input's history (↑).
    MessageDeleted {
        message_id: MessageId,
    },
    /// Messages that were sitting in the queue ("Queued" chip in the UI)
    /// have been handed to a run. The FE clears their queued markers —
    /// from here on they're ordinary conversation messages.
    QueuedMessagesDelivered {
        message_ids: Vec<MessageId>,
    },
    SessionCompacted {
        compaction: AssistantCompaction,
        summary_message: AssistantMessage,
    },
    RunQueued {
        run: AssistantRun,
    },
    RunStarted {
        run: AssistantRun,
    },
    AssistantDelta {
        message_id: MessageId,
        text: String,
    },
    AssistantThinkingDelta {
        message_id: MessageId,
        text: String,
    },
    AssistantMessageCompleted {
        message: AssistantMessage,
    },
    /// Emitted mid-turn when an assistant message's persisted content
    /// has grown (e.g. a Claude Code stream just added a `ToolUse`
    /// part). The frontend should swap the message's `content` for the
    /// new version so live tool-call rendering doesn't have to wait for
    /// the turn-final `AssistantMessageCompleted`. Distinct from
    /// `*Completed` so the streaming-state UI (spinner, "writing…")
    /// stays on until the run actually ends.
    AssistantMessageUpdated {
        message: AssistantMessage,
    },
    ToolCallStarted {
        tool_call: ToolInvocation,
    },
    ToolCallCompleted {
        tool_call: ToolInvocation,
    },
    ToolCallFailed {
        tool_call: ToolInvocation,
    },
    RunCompleted {
        run: AssistantRun,
    },
    RunFailed {
        run: AssistantRun,
    },
    RunCancelled {
        run: AssistantRun,
    },
    /// The `ask_user` tool is awaiting a human answer. The FE renders an
    /// inline answer block (radios — or checkboxes when `multi_select` —
    /// for options, textarea for free text) keyed by `pending_id` and
    /// submits via `assistant_submit_user_input`.
    AskUserRequested {
        pending_id: String,
        question: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        options: Option<Vec<AskUserOption>>,
        /// When true the user may pick any number of options.
        #[serde(default)]
        multi_select: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra_context: Option<String>,
    },
    /// The `ask_user` tool has received the user's answer; the inline
    /// block can be cleared.
    AskUserResolved {
        pending_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AssistantEventEnvelope {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
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
        workspace_id: session.context.workspace_id.clone(),
        timestamp: Utc::now().timestamp_millis(),
        event,
    };

    app.emit(ASSISTANT_EVENT_NAME, envelope)
        .map_err(|e| format!("Failed to emit assistant event: {}", e))
}

use crate::assistant::engine::AssistantDeps;
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository;
use crate::assistant::types::{ContentPart, MessageRole, ProviderInputMessage, RunTrigger};

pub(crate) fn build_trigger_message(
    session: &crate::assistant::types::AssistantSession,
    trigger: &RunTrigger,
) -> Option<ProviderInputMessage> {
    let automation_name = session
        .context
        .automation_name
        .as_deref()
        .unwrap_or("automation");
    let now = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string();

    let text = match trigger {
        RunTrigger::Scheduled => Some(format!(
            "--- New scheduled run at {} ---\n\
             Tool outputs above this marker are from previous runs.\n\
             Evaluate whether they are still fresh enough for the current pass and re-run tools when needed.\n\n\
             Run the next scheduled pass for {} now. Inspect the current state, \
             update the workspace as needed, and end with a concise status update.",
            now, automation_name
        )),
        RunTrigger::ManualAutomation => Some(format!(
            "--- Manual run at {} ---\n\
             Tool outputs above this marker are from previous runs.\n\
             Evaluate whether they are still fresh enough and re-run tools when needed.\n\n\
             Run the automation {} now and report the current findings.",
            now, automation_name
        )),
        RunTrigger::InterAgentCall
        | RunTrigger::WorkspaceTask
        | RunTrigger::UserMessage
        | RunTrigger::Retry => None,
    }?;

    Some(ProviderInputMessage {
        role: MessageRole::User,
        content: vec![ContentPart::Text { text }],
    })
}

/// True when any content part is an image. Image-bearing messages are
/// preserved on run failure (see `discard_unanswered_run_input`).
pub(crate) fn message_contains_image(parts: &[ContentPart]) -> bool {
    parts
        .iter()
        .any(|part| matches!(part, ContentPart::Image { .. }))
}

pub(crate) fn run_produced_no_content(parts: &[ContentPart]) -> bool {
    parts
        .iter()
        .all(|part| matches!(part, ContentPart::Text { text } if text.is_empty()))
}

/// Best-effort cleanup after a run that failed before the provider produced
/// anything (connection error, usage limit, CLI spawn failure): delete the
/// user message(s) that triggered it — the direct trigger plus any queued
/// messages delivered to this run — and the empty assistant placeholder, then
/// emit `MessageDeleted` for each so the UI drops them too. A message that
/// never got an answer has no business lingering in the conversation; the
/// failed run row keeps its error, so the failure banner still explains what
/// happened, and the typed text stays recoverable via the input history.
///
/// Exception: messages carrying a [`ContentPart::Image`] are preserved. Unlike
/// typed text, an attached image is not recoverable from the composer's input
/// history, so deleting the message would orphan the stored file. A preserved
/// image stays in conversation history and is auto-retried on the next turn —
/// the per-turn `connection_supports_images` gate decides whether it is sent —
/// so switching to a vision-capable model after the failure just works.
/// Errors are logged, not propagated — cleanup must never mask the original
/// failure.
#[allow(
    clippy::cognitive_complexity,
    reason = "lint debt: cognitive complexity 33 against a budget of 25"
)]
pub(crate) async fn discard_unanswered_run_input(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
    run_id: &str,
    trigger_message_id: Option<&str>,
    assistant_placeholder_id: Option<&str>,
) {
    let mut message_ids: Vec<String> = Vec::new();
    match repository::list_delivered_queued_messages_for_run(&deps.pool, &session.id, run_id).await
    {
        Ok(queued) => {
            for q in queued {
                // Preserve image-bearing messages (see fn doc): they are not
                // recoverable from the composer input history.
                if message_contains_image(&q.message.content) {
                    continue;
                }
                message_ids.push(q.message.id);
            }
        }
        Err(error) => tracing::warn!(
            run_id,
            error,
            "Failed to list queued messages while discarding unanswered run input"
        ),
    }
    if let Some(id) = trigger_message_id {
        // Skip deletion when the trigger carries an image. On lookup failure,
        // preserve rather than risk orphaning an image-bearing message.
        let preserve = match repository::get_message(&deps.pool, id).await {
            Ok(Some(message)) => message_contains_image(&message.content),
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(
                    run_id,
                    message_id = id,
                    error,
                    "Failed to load trigger message while discarding unanswered run input"
                );
                true
            }
        };
        if !preserve && !message_ids.iter().any(|existing| existing == id) {
            message_ids.push(id.to_string());
        }
    }
    // The empty assistant placeholder is always noise — drop it regardless.
    message_ids.extend(assistant_placeholder_id.map(str::to_string));

    for message_id in message_ids {
        match repository::delete_message(&deps.pool, &message_id).await {
            Ok(()) => {
                let _ = emit_event(
                    &deps.app,
                    session,
                    Some(run_id),
                    AssistantUiEvent::MessageDeleted { message_id },
                );
            }
            Err(error) => tracing::warn!(
                run_id,
                message_id,
                error,
                "Failed to delete unanswered run input message"
            ),
        }
    }
}

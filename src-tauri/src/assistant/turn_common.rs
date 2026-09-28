use crate::assistant::engine::AssistantDeps;
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository;
use crate::assistant::types::{ContentPart, MessageRole, ProviderInputMessage, RunTrigger};
use crate::db::DbPool;

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
pub(crate) async fn discard_unanswered_run_input(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
    run_id: &str,
    trigger_message_id: Option<&str>,
    assistant_placeholder_id: Option<&str>,
) {
    discard_unanswered_run_input_with_sink(
        &deps.pool,
        &session.id,
        run_id,
        trigger_message_id,
        assistant_placeholder_id,
        |message_id| {
            let _ = emit_event(
                &deps.app,
                session,
                Some(run_id),
                AssistantUiEvent::MessageDeleted { message_id },
            );
        },
    )
    .await;
}

#[allow(
    clippy::cognitive_complexity,
    reason = "lint debt: cognitive complexity 33 against a budget of 25"
)]
async fn discard_unanswered_run_input_with_sink(
    pool: &DbPool,
    session_id: &str,
    run_id: &str,
    trigger_message_id: Option<&str>,
    assistant_placeholder_id: Option<&str>,
    mut on_deleted: impl FnMut(String),
) {
    let mut message_ids: Vec<String> = Vec::new();
    match repository::list_delivered_queued_messages_for_run(pool, session_id, run_id).await {
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
        let preserve = match repository::get_message(pool, id).await {
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
        match repository::delete_message(pool, &message_id).await {
            Ok(()) => {
                on_deleted(message_id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::repository::{CreateRunParams, CreateSessionParams};
    use crate::assistant::types::{RunStatus, SessionContext, SessionKind};
    use crate::db::test_support::workspace_pool;

    fn image() -> ContentPart {
        ContentPart::Image {
            id: "image-1".into(),
            path: "image-1.png".into(),
            media_type: "image/png".into(),
            filename: None,
            width: None,
            height: None,
        }
    }

    async fn user_message(
        pool: &DbPool,
        session_id: &str,
        content: Vec<ContentPart>,
        queued: bool,
    ) -> crate::assistant::types::AssistantMessage {
        repository::create_user_message_with_content(
            pool,
            session_id.to_string(),
            content,
            queued.then_some("connection"),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn unanswered_input_discards_text_but_keeps_images_and_pending_queue() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(
            &pool,
            CreateSessionParams {
                kind: SessionKind::Interactive,
                title: None,
                context: SessionContext::default(),
            },
        )
        .await
        .unwrap();
        let run = repository::create_run(
            &pool,
            CreateRunParams {
                session_id: session.id.clone(),
                status: RunStatus::Running,
                trigger: RunTrigger::UserMessage,
                connection_id: "connection".into(),
                protocol_id: "openai".into(),
                model_id: "model".into(),
                error: None,
            },
        )
        .await
        .unwrap();
        let text = |value: &str| ContentPart::Text { text: value.into() };
        let trigger_text = user_message(&pool, &session.id, vec![text("trigger")], false).await;
        let trigger_image = user_message(
            &pool,
            &session.id,
            vec![text("image trigger"), image()],
            false,
        )
        .await;
        let delivered_text = user_message(&pool, &session.id, vec![text("delivered")], true).await;
        let delivered_image = user_message(&pool, &session.id, vec![image()], true).await;
        let pending = user_message(&pool, &session.id, vec![text("pending")], true).await;
        repository::mark_queued_messages_delivered(
            &pool,
            &session.id,
            &run.id,
            &[delivered_text.id.clone(), delivered_image.id.clone()],
        )
        .await
        .unwrap();

        let mut deleted = Vec::new();
        discard_unanswered_run_input_with_sink(
            &pool,
            &session.id,
            &run.id,
            Some(&trigger_text.id),
            None,
            |id| deleted.push(id),
        )
        .await;
        discard_unanswered_run_input_with_sink(
            &pool,
            &session.id,
            &run.id,
            Some(&trigger_image.id),
            None,
            |id| deleted.push(id),
        )
        .await;

        assert_eq!(deleted.len(), 2);
        assert!(deleted.contains(&trigger_text.id));
        assert!(deleted.contains(&delivered_text.id));
        for id in [&trigger_text.id, &delivered_text.id] {
            assert!(repository::get_message(&pool, id).await.unwrap().is_none());
        }
        for id in [&trigger_image.id, &delivered_image.id, &pending.id] {
            assert!(repository::get_message(&pool, id).await.unwrap().is_some());
        }
        let pending_ids = repository::list_pending_queued_message_ids(&pool, &session.id)
            .await
            .unwrap();
        assert_eq!(pending_ids, vec![pending.id]);
    }
}

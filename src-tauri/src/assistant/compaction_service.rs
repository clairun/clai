use crate::assistant::{conversation::RunConversation, conversation::COMPACTION_METADATA_SOURCE};
use std::path::Path;

use crate::assistant::compaction::{self, CompactionOutcome, PreparedCompaction};
use crate::assistant::repository::{self, CreateCompactionParams, CreateMessageParams};
use crate::assistant::types::{
    AssistantSession, CompactionTrigger, ContentPart, MessageRole, ProviderConnection,
};
use crate::db::DbPool;

/// Load a conversation with queued rows marked pending before any compaction cut.
pub async fn load_with_pending(pool: &DbPool, session_id: &str) -> Result<RunConversation, String> {
    let mut conversation = RunConversation::load(pool, session_id).await?;
    let pending = repository::list_pending_queued_messages(pool, session_id).await?;
    conversation.refresh_pending(pending.into_iter().map(|queued| queued.message).collect());
    Ok(conversation)
}

/// Commit a prepared summary, then update the run's provider view.
pub async fn commit_compaction(
    pool: &DbPool,
    conversation: &mut RunConversation,
    prepared: PreparedCompaction,
) -> Result<CompactionOutcome, String> {
    let PreparedCompaction {
        session_id,
        trigger,
        strategy,
        source_from_message_id,
        source_to_message_id,
        created_run_id,
        protocol_id,
        model_id,
        input_message_count,
        summary_text,
        consumed,
    } = prepared;
    let compaction = repository::create_compaction(
        pool,
        CreateCompactionParams {
            session_id: session_id.clone(),
            trigger: trigger.clone(),
            strategy: strategy.clone(),
            source_from_message_id: Some(source_from_message_id.clone()),
            source_to_message_id: Some(source_to_message_id.clone()),
            created_run_id,
            protocol_id,
            model_id,
            input_message_count,
        },
    )
    .await?;
    let summary_message = repository::create_message(
        pool,
        CreateMessageParams {
            session_id,
            role: MessageRole::System,
            content: vec![ContentPart::Text { text: summary_text }],
            provider_metadata: Some(serde_json::json!({
                "source": COMPACTION_METADATA_SOURCE,
                "compactionId": compaction.id,
                "trigger": trigger,
                "strategy": strategy,
                "sourceFromMessageId": source_from_message_id,
                "sourceToMessageId": source_to_message_id,
                "createdAt": chrono::Utc::now().timestamp_millis(),
            })),
        },
    )
    .await?;
    let compaction =
        repository::complete_compaction(pool, &compaction.id, &summary_message.id).await?;
    conversation.apply_compaction(consumed, summary_message.clone());
    Ok(CompactionOutcome {
        compaction,
        summary_message,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn compact_conversation(
    pool: &DbPool,
    session: &AssistantSession,
    connection: &ProviderConnection,
    summary_working_dir: Option<&Path>,
    trigger: CompactionTrigger,
    run_id: Option<&str>,
    verbatim_tail_tokens: usize,
    conversation: &mut RunConversation,
) -> Result<Option<CompactionOutcome>, String> {
    let prepared = compaction::prepare_compaction(
        session,
        connection,
        summary_working_dir,
        trigger,
        run_id,
        verbatim_tail_tokens,
        conversation,
    )
    .await?;
    match prepared {
        Some(prepared) => commit_compaction(pool, conversation, prepared)
            .await
            .map(Some),
        None => Ok(None),
    }
}

pub async fn compact_for_context_limit_recovery(
    pool: &DbPool,
    session: &AssistantSession,
    connection: &ProviderConnection,
    summary_working_dir: Option<&Path>,
    run_id: &str,
    conversation: &mut RunConversation,
) -> Result<Option<CompactionOutcome>, String> {
    let prepared = compaction::prepare_context_limit_recovery(
        session,
        connection,
        summary_working_dir,
        run_id,
        conversation,
    )
    .await?;
    match prepared {
        Some(prepared) => commit_compaction(pool, conversation, prepared)
            .await
            .map(Some),
        None => Ok(None),
    }
}

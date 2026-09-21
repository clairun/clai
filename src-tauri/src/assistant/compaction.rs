use futures::StreamExt;
use std::path::Path;

use crate::assistant::providers;
use crate::assistant::providers::types::ProviderError;
use crate::assistant::repository::{self, CreateCompactionParams, CreateMessageParams};
use crate::assistant::types::{
    AssistantCompaction, AssistantMessage, AssistantSession, CompactionStrategy, CompactionTrigger,
    CompletionRequest, ContentPart, MessageRole, ProviderConnection, ProviderEvent,
    ProviderInputMessage, ToolDefinition,
};
use crate::db::DbPool;

pub const COMPACTION_METADATA_SOURCE: &str = "clai-compaction";

const RECENT_TAIL_MESSAGES: usize = 16;
const MIN_AUTOMATIC_COMPACT_MESSAGES: usize = 24;
const MIN_MANUAL_COMPACT_MESSAGES: usize = 2;
/// Trigger threshold for automatic compaction, measured against the estimated
/// size of the outgoing request (system prompt + provider view + tool
/// definitions). The predecessor gate measured content at the summariser's
/// caps, which under-counted tool-heavy requests by an unbounded factor and
/// let runs crash on the provider's context limit instead of compacting.
const AUTO_COMPACTION_REQUEST_CHARS: usize = 360_000;
/// Wire overhead each message adds on top of its rendered content (role
/// label, ids, JSON keys in the request envelope).
const REQUEST_OVERHEAD_CHARS_PER_MESSAGE: usize = 32;
/// Hysteresis for the automatic trigger: messages persisted after the
/// standing summary before the trigger can fire again. Without it a just-
/// compacted session re-triggers on its own retained tail (the tail alone
/// satisfies the message-count gate) and summarizes its fresh context away.
const MIN_MESSAGES_SINCE_COMPACTION: usize = MIN_AUTOMATIC_COMPACT_MESSAGES;
const SUMMARY_TRANSCRIPT_MAX_CHARS: usize = 96_000;
const SUMMARY_MAX_OUTPUT_TOKENS: u32 = 4096;
const SUMMARY_TOOL_CALL_MAX_CHARS: usize = 4_000;
const SUMMARY_TOOL_RESULT_MAX_CHARS: usize = 8_000;
/// Hard budget for the summary body we store back into the conversation.
/// `SUMMARY_MAX_OUTPUT_TOKENS` is only a *request*: on the CLI path it is
/// advisory prose in the prompt (`providers/cli.rs`), so nothing enforces it.
/// An oversized summary is not self-limiting -- it is replayed on every
/// following turn and re-injected into every fresh CLI session until the next
/// compaction folds it away -- so it is clamped here, at the single point where
/// a summary becomes durable.
///
/// The number is pinned by two neighbours, not by taste:
/// * it must stay above a *conforming* summary (~4 chars per token against a
///   4096-token ask, ~16_400 chars) so a well-behaved model is never clamped;
/// * with the preamble it must stay under `SUMMARY_TRANSCRIPT_MAX_CHARS / 3`,
///   the head slice of `transcript_for_summary`, or the *next* compaction pass
///   drops this summary's own tail into its omitted middle.
///
/// It is also well under `CLI_FRESH_CONTEXT_SUMMARY_MAX_BYTES` (64_000), so the
/// CLI fresh-session clamp never has to re-cut a summary we produced.
const SUMMARY_MESSAGE_MAX_CHARS: usize = 24_000;

#[derive(Debug, Clone)]
pub struct CompactionOutcome {
    pub compaction: AssistantCompaction,
    pub summary_message: AssistantMessage,
}

struct CompactionWindow {
    messages: Vec<AssistantMessage>,
    source_from_message_id: Option<String>,
    source_to_message_id: Option<String>,
}

pub fn is_compaction_summary_message(message: &AssistantMessage) -> bool {
    message
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("source"))
        .and_then(|value| value.as_str())
        == Some(COMPACTION_METADATA_SOURCE)
}

/// The provider-bound history for a session, loaded with bounded queries.
///
/// `view` is byte-identical to what a full-session load produced before:
/// `[standing summary] + messages after the boundary`, or every non-summary
/// message when the session has no usable compaction. `summary_message` is
/// the standing summary itself (the first message of `view` when present).
/// `messages_since_compaction` counts messages persisted after that summary
/// and drives the automatic trigger's hysteresis; `None` when the session has
/// never completed a compaction. `boundary` is the `(created_at, id)` key of
/// the boundary message (`source_to`): the last message the standing summary
/// covers. Messages at or before it are no longer part of `view`, so callers
/// that must distinguish "summarized away" from "never loaded" compare
/// against it.
pub struct ProviderHistory {
    pub view: Vec<AssistantMessage>,
    pub summary_message: Option<AssistantMessage>,
    pub messages_since_compaction: Option<usize>,
    pub boundary: Option<(i64, String)>,
}

pub async fn load_provider_history(
    pool: &DbPool,
    session_id: &str,
) -> Result<ProviderHistory, String> {
    let latest = repository::latest_completed_compaction(pool, session_id).await?;
    let Some(compaction) = latest else {
        // Never compacted: the provider view is the whole session minus
        // standing summaries from any earlier pass.
        let messages = repository::list_messages(pool, session_id).await?;
        return Ok(ProviderHistory {
            view: filter_non_summary(&messages),
            summary_message: None,
            messages_since_compaction: None,
            boundary: None,
        });
    };

    if let (Some(summary_id), Some(source_to_id)) = (
        compaction.summary_message_id.as_deref(),
        compaction.source_to_message_id.as_deref(),
    ) {
        if let (Some(summary), Some(source_to)) = (
            repository::get_message(pool, summary_id).await?,
            repository::get_message(pool, source_to_id).await?,
        ) {
            if summary.session_id == session_id && source_to.session_id == session_id {
                // Healthy path: two point lookups plus the post-boundary
                // tail, without loading the compacted-away majority.
                let tail = repository::list_messages_after(
                    pool,
                    session_id,
                    (source_to.created_at, source_to.id.as_str()),
                )
                .await?;
                let messages_since_compaction = tail
                    .iter()
                    .filter(|message| is_after(message, (summary.created_at, summary.id.as_str())))
                    .count();
                let mut view = Vec::with_capacity(tail.len() + 1);
                view.push(summary.clone());
                view.extend(tail.into_iter().filter(|message| {
                    message.id != summary_id && !is_compaction_summary_message(message)
                }));
                return Ok(ProviderHistory {
                    view,
                    summary_message: Some(summary),
                    messages_since_compaction: Some(messages_since_compaction),
                    boundary: Some((source_to.created_at, source_to.id.clone())),
                });
            }
        }
    }

    // Degenerate compaction row (referenced messages deleted, or pointing at
    // another session): fall back to the full-session load, which is exactly
    // what the pre-boundary implementation did for such rows.
    let messages = repository::list_messages(pool, session_id).await?;
    let view = provider_history_messages_with_compaction(&messages, Some(&compaction));
    let summary_message = compaction
        .summary_message_id
        .as_deref()
        .and_then(|id| messages.iter().find(|message| message.id == id).cloned());
    let messages_since_compaction = summary_message.as_ref().map(|summary| {
        messages
            .iter()
            .filter(|message| is_after(message, (summary.created_at, summary.id.as_str())))
            .count()
    });
    Ok(ProviderHistory {
        view,
        summary_message,
        messages_since_compaction,
        boundary: None,
    })
}

fn filter_non_summary(messages: &[AssistantMessage]) -> Vec<AssistantMessage> {
    messages
        .iter()
        .filter(|message| !is_compaction_summary_message(message))
        .cloned()
        .collect()
}

fn is_after(message: &AssistantMessage, key: (i64, &str)) -> bool {
    message.created_at > key.0 || (message.created_at == key.0 && message.id.as_str() > key.1)
}

/// Decide whether the automatic compaction trigger fires before the next
/// provider call.
///
/// Three gates, checked in order of cheapness:
///
/// * message count: at least `MIN_AUTOMATIC_COMPACT_MESSAGES +
///   RECENT_TAIL_MESSAGES` non-summary messages in `view`. A summary never
///   counts toward it, so the gate cannot be satisfied by the summary alone.
/// * hysteresis: at least `MIN_MESSAGES_SINCE_COMPACTION` messages persisted
///   after the standing summary. The retained tail of a just-finished pass
///   satisfies the count gate on its own; without hysteresis a session could
///   summarize its own fresh tail every turn and thrash.
/// * request size: the estimated size of the outgoing request — system
///   prompt, `view`, and tool definitions — crosses
///   `AUTO_COMPACTION_REQUEST_CHARS`.
///
/// `messages_since_compaction` is `None` when the session has never completed
/// a compaction; hysteresis only applies to sessions that have. The forced
/// (`/compact`) path never consults this function.
pub fn should_auto_compact(
    view: &[AssistantMessage],
    system_prompt: Option<&ProviderInputMessage>,
    tools: &[ToolDefinition],
    messages_since_compaction: Option<usize>,
) -> bool {
    let non_compaction_messages = view
        .iter()
        .filter(|message| !is_compaction_summary_message(message))
        .count();
    if non_compaction_messages < MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES {
        return false;
    }
    if let Some(since) = messages_since_compaction {
        if since < MIN_MESSAGES_SINCE_COMPACTION {
            return false;
        }
    }

    request_chars(view, system_prompt, tools) >= AUTO_COMPACTION_REQUEST_CHARS
}

pub fn is_context_limit_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "context length",
        "context window",
        "maximum context",
        "max context",
        "too many tokens",
        "token limit",
        "prompt is too long",
        "prompt too long",
        "input is too long",
        "input exceeds the maximum length",
        "input_too_large",
        "input tokens",
        "exceeds the model",
        "exceeds context",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// What automatic compaction achieved during a run.
///
/// Compaction failures used to be `tracing::warn!`-and-drop: the run then died
/// on the provider's raw "prompt is too long", so the user had no way to tell
/// that compaction is what failed, let alone whether the summariser broke
/// (retryable) or the history simply cannot shrink further (not retryable).
/// Runs thread their attempt to the failure site instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CompactionAttempt {
    /// Compaction never ran during this run.
    #[default]
    NotAttempted,
    /// Compaction ran but had nothing it could summarize; history is unchanged.
    NothingToCompact,
    /// Compaction ran and failed; history is unchanged.
    Failed(String),
}

impl CompactionAttempt {
    /// Record a compaction error, keeping the *first* failure of the run: a
    /// later attempt is usually the same fault, and the earliest one is what
    /// let the context grow past the limit.
    pub fn record_failure(&mut self, error: impl std::fmt::Display) {
        if matches!(self, Self::Failed(_)) {
            return;
        }
        *self = Self::Failed(error.to_string());
    }

    /// Compaction succeeded, so an earlier failure no longer describes this
    /// run: the history *did* shrink, and telling the user that compaction is
    /// broken would send them away from the one remedy that works.
    pub fn record_success(&mut self) {
        *self = Self::NotAttempted;
    }

    /// Record that compaction ran but produced nothing. Never downgrades a
    /// recorded failure.
    pub fn record_nothing_to_compact(&mut self) {
        if matches!(self, Self::NotAttempted) {
            *self = Self::NothingToCompact;
        }
    }
}

/// User-facing text for a run that hit the provider's context limit.
///
/// Detection stays with the caller (`is_context_limit_error`); `subject` names
/// what failed, e.g. "Claude Code" or "The request". When nothing is known
/// about compaction and the provider already told the user to run `/compact`,
/// the provider message is passed through unchanged — there is nothing to add.
pub fn context_limit_failure_message(
    subject: &str,
    provider_message: &str,
    attempt: &CompactionAttempt,
) -> String {
    if matches!(attempt, CompactionAttempt::NotAttempted)
        && provider_message.contains("run `/compact`")
    {
        return provider_message.to_string();
    }

    let (diagnosis, remedy) = match attempt {
        CompactionAttempt::NotAttempted => (
            "CLAI tried automatic compaction when possible.".to_string(),
            "Run `/compact` or start a new thread, then retry.",
        ),
        CompactionAttempt::NothingToCompact => (
            "Automatic compaction ran but found nothing it could summarize, so the history is unchanged."
                .to_string(),
            "Compacting again will not help; start a new thread to continue.",
        ),
        CompactionAttempt::Failed(error) => (
            format!("Automatic compaction failed, so the history is unchanged: {error}"),
            "Compacting manually will most likely fail the same way; start a new thread to continue.",
        ),
    };

    format!(
        "{subject} could not complete because the conversation context is too large for the \
         provider's current turn limit. {diagnosis} {remedy}\n\nProvider error: {provider_message}"
    )
}

pub async fn reset_cli_session_for_rotation(
    pool: &DbPool,
    session: &mut AssistantSession,
) -> Result<(), String> {
    if session.context.cli_session_id.is_none() && session.context.cli_session_provider.is_none() {
        return Ok(());
    }
    session.context.cli_session_id = None;
    session.context.cli_session_provider = None;
    session.updated_at = chrono::Utc::now().timestamp_millis();
    *session = repository::update_session(pool, session).await?;
    Ok(())
}

pub async fn compact_session_history(
    pool: &DbPool,
    session: &AssistantSession,
    connection: &ProviderConnection,
    summary_working_dir: Option<&Path>,
    trigger: CompactionTrigger,
    run_id: Option<&str>,
    force: bool,
) -> Result<Option<CompactionOutcome>, String> {
    let provider_view = load_provider_history(pool, &session.id).await?.view;
    let Some(window) = select_compaction_window(&provider_view, force) else {
        return Ok(None);
    };

    let strategy = if providers::is_cli_provider(&connection.protocol_id) {
        CompactionStrategy::SessionRotationSummary
    } else {
        CompactionStrategy::LocalSummary
    };

    let summary = summarize_window(
        session,
        connection,
        summary_working_dir,
        run_id,
        &window.messages,
    )
    .await?;

    let compaction = repository::create_compaction(
        pool,
        CreateCompactionParams {
            session_id: session.id.clone(),
            trigger: trigger.clone(),
            strategy: strategy.clone(),
            source_from_message_id: window.source_from_message_id.clone(),
            source_to_message_id: window.source_to_message_id.clone(),
            created_run_id: run_id.map(str::to_string),
            protocol_id: connection.protocol_id.clone(),
            model_id: connection.model_id.clone(),
            input_message_count: window.messages.len() as i64,
        },
    )
    .await?;

    let summary_message = repository::create_message(
        pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::System,
            content: vec![ContentPart::Text {
                text: summary_message_text(&summary),
            }],
            provider_metadata: Some(serde_json::json!({
                "source": COMPACTION_METADATA_SOURCE,
                "compactionId": compaction.id,
                "trigger": trigger,
                "strategy": strategy,
                "sourceFromMessageId": window.source_from_message_id,
                "sourceToMessageId": window.source_to_message_id,
                "createdAt": chrono::Utc::now().timestamp_millis(),
            })),
        },
    )
    .await?;

    let compaction =
        repository::complete_compaction(pool, &compaction.id, &summary_message.id).await?;

    Ok(Some(CompactionOutcome {
        compaction,
        summary_message,
    }))
}

pub async fn compact_for_context_limit_recovery(
    pool: &DbPool,
    session: &AssistantSession,
    connection: &ProviderConnection,
    summary_working_dir: Option<&Path>,
    run_id: &str,
) -> Result<Option<CompactionOutcome>, String> {
    compact_session_history(
        pool,
        session,
        connection,
        summary_working_dir,
        CompactionTrigger::ErrorRecovery,
        Some(run_id),
        true,
    )
    .await
}

fn provider_history_messages_with_compaction(
    messages: &[AssistantMessage],
    latest: Option<&AssistantCompaction>,
) -> Vec<AssistantMessage> {
    let Some(compaction) = latest else {
        return messages
            .iter()
            .filter(|message| !is_compaction_summary_message(message))
            .cloned()
            .collect();
    };
    let Some(summary_message_id) = compaction.summary_message_id.as_deref() else {
        return messages
            .iter()
            .filter(|message| !is_compaction_summary_message(message))
            .cloned()
            .collect();
    };
    let Some(source_to_message_id) = compaction.source_to_message_id.as_deref() else {
        return messages
            .iter()
            .filter(|message| !is_compaction_summary_message(message))
            .cloned()
            .collect();
    };

    let summary = messages
        .iter()
        .find(|message| message.id == summary_message_id)
        .cloned();
    let source_to_idx = messages
        .iter()
        .position(|message| message.id == source_to_message_id);

    match (summary, source_to_idx) {
        (Some(summary), Some(source_to_idx)) => {
            let mut out = vec![summary];
            out.extend(
                messages
                    .iter()
                    .skip(source_to_idx + 1)
                    .filter(|message| {
                        message.id != summary_message_id && !is_compaction_summary_message(message)
                    })
                    .cloned(),
            );
            out
        }
        _ => messages
            .iter()
            .filter(|message| !is_compaction_summary_message(message))
            .cloned()
            .collect(),
    }
}

fn select_compaction_window(
    provider_view: &[AssistantMessage],
    force: bool,
) -> Option<CompactionWindow> {
    let compactable: Vec<AssistantMessage> = provider_view
        .iter()
        .filter(|message| {
            !matches!(message.role, MessageRole::System) || is_compaction_summary_message(message)
        })
        .cloned()
        .collect();
    let min_messages = if force {
        MIN_MANUAL_COMPACT_MESSAGES
    } else {
        MIN_AUTOMATIC_COMPACT_MESSAGES
    };
    if compactable.len() < min_messages {
        return None;
    }

    let tail_count = if force {
        RECENT_TAIL_MESSAGES.min(compactable.len().saturating_sub(min_messages))
    } else {
        RECENT_TAIL_MESSAGES
    };
    let compact_count = compactable.len().saturating_sub(tail_count);
    if compact_count < min_messages {
        return None;
    }

    // The positional boundary above can land in the middle of an
    // assistant->tool group. Nudge it off before committing to the window.
    let compact_count = group_aware_boundary(&compactable, compact_count, min_messages)?;

    let messages = compactable[..compact_count].to_vec();
    let source_from_message_id = messages.first().map(|message| message.id.clone());
    let source_to_message_id = messages.last().map(|message| message.id.clone());

    Some(CompactionWindow {
        messages,
        source_from_message_id,
        source_to_message_id,
    })
}

/// Move a compaction boundary off the middle of an assistant->tool group.
///
/// `boundary` is the index of the first message that stays in the retained
/// tail, so `messages[..boundary]` is what gets summarised away. A
/// `MessageRole::Tool` message sitting exactly at `boundary` is the failure
/// case: its results are retained while the assistant message that issued the
/// corresponding `ToolUse` calls is summarised away. `normalize_history_for_provider`
/// then finds no assistant claiming those `tool_call_id`s and drops the results
/// as orphans -- on *every* subsequent turn, because the compaction record is
/// persistent. The model silently never sees that tool output again.
///
/// Preference order:
/// 1. Walk the boundary **back** to the owning assistant, so the whole group is
///    retained. This is the cheap, information-preserving option.
/// 2. If that would shrink the window below `min_messages`, walk **forward**
///    instead and compact the group whole. The results are then summarised
///    rather than lost.
/// 3. If swallowing the group forward would leave no tail at all, decline to
///    compact this round (`None`). Waiting is always safe; the next turn will
///    have more messages to work with.
fn group_aware_boundary(
    messages: &[AssistantMessage],
    boundary: usize,
    min_messages: usize,
) -> Option<usize> {
    if !matches!(
        messages.get(boundary).map(|message| &message.role),
        Some(MessageRole::Tool)
    ) {
        return Some(boundary);
    }

    // Walk back over the group's sibling results to the assistant that owns them.
    let mut retained = boundary;
    while retained > 0 && matches!(messages[retained].role, MessageRole::Tool) {
        retained -= 1;
    }

    if matches!(messages[retained].role, MessageRole::Tool) {
        // Ran off the front of the view without finding an owner, so the
        // owning assistant was already outside it. Nothing here to protect.
        return Some(boundary);
    }

    if retained >= min_messages {
        return Some(retained);
    }

    // Retaining the whole group leaves too little behind to be worth
    // summarising. Compact the group instead of splitting it.
    let mut swallowed = boundary;
    while swallowed < messages.len() && matches!(messages[swallowed].role, MessageRole::Tool) {
        swallowed += 1;
    }

    if swallowed >= messages.len() {
        // The entire remainder is one tool group; compacting it would leave an
        // empty tail. Skip this round rather than produce a degenerate window.
        return None;
    }

    Some(swallowed)
}

async fn summarize_window(
    session: &AssistantSession,
    connection: &ProviderConnection,
    summary_working_dir: Option<&Path>,
    source_run_id: Option<&str>,
    messages: &[AssistantMessage],
) -> Result<String, String> {
    let adapter = providers::resolve_adapter(&connection.protocol_id).map_err(|e| e.to_string())?;
    let transcript = transcript_for_summary(messages);
    let request = CompletionRequest {
        run_id: compaction_summary_run_id(session, source_run_id),
        session_id: session.id.clone(),
        model_id: connection.model_id.clone(),
        messages: vec![
            ProviderInputMessage {
                role: MessageRole::System,
                content: vec![ContentPart::Text {
                    text: SUMMARY_SYSTEM_PROMPT.to_string(),
                }],
            },
            ProviderInputMessage {
                role: MessageRole::User,
                content: vec![ContentPart::Text { text: transcript }],
            },
        ],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: Some(SUMMARY_MAX_OUTPUT_TOKENS),
        images: Default::default(),
    };

    let mut stream = adapter
        .stream_sessionless_completion(connection, request, summary_working_dir)
        .await
        .map_err(provider_error_message)?;
    let mut summary = String::new();
    while let Some(event) = stream.next().await {
        match event.map_err(provider_error_message)? {
            ProviderEvent::TextDelta { text } => summary.push_str(&text),
            ProviderEvent::ProviderError { message } => return Err(message),
            ProviderEvent::MessageStart
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ThinkingSignature { .. }
            | ProviderEvent::ToolCallDelta { .. }
            | ProviderEvent::ToolCallReady { .. }
            | ProviderEvent::MessageComplete => {}
        }
    }

    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err("Compaction summary was empty".to_string());
    }
    Ok(summary)
}

fn compaction_summary_run_id(session: &AssistantSession, source_run_id: Option<&str>) -> String {
    match source_run_id {
        Some(run_id) => format!("compaction-{run_id}"),
        None => format!(
            "compaction-{}-{}",
            session.id,
            chrono::Utc::now().timestamp_millis()
        ),
    }
}

const SUMMARY_SYSTEM_PROMPT: &str = r#"Summarize the previous conversation so another assistant can continue it with minimal context.

Write only prose conclusions and durable state.

Preserve:
- user goals and constraints
- concrete decisions, assumptions, unresolved tasks, and current status
- files touched, code changes, test outcomes, errors, and blockers at a plain-language level
- stable evidence references when exact details may matter, using opaque ids from the transcript such as `source message <id>` or `assistant_tool_calls row <id>`
- any instructions that remain binding

Do not copy command arguments, invocation syntax, raw JSON, transcript markers, XML-like wrappers, or tool-result payloads. If prior tool activity matters, summarize the outcome in ordinary words and cite a source id for later lookup with `history_query`.

Do not include filler, greetings, or obsolete intermediate details. Do not invent facts. Write a compact but complete continuation summary."#;

/// Preamble prepended to every stored compaction summary message.
const SUMMARY_MESSAGE_PREAMBLE: &str =
    "Conversation summary generated by CLAI compaction. Treat this as the \
     authoritative summary of the compacted earlier messages. If you are \
     missing context needed to continue, recover it before acting rather \
     than asking the user to repeat anything: your durable state is in \
     `.clai/memory/` and the full verbatim history (every message and tool \
     result) is in `.clai/data.sqlite` — query it with the read-only \
     `history_query` tool (no approval needed) to recover specifics.";

/// Inserted where an over-budget summary was cut, so the next summarizer pass
/// (and any human reading the thread) can tell the gap from a model omission.
const SUMMARY_BODY_OMISSION_MARKER: &str =
    "\n\n[... middle of this summary omitted: it exceeded the stored-summary budget ...]\n\n";

fn summary_message_text(summary: &str) -> String {
    format!(
        "{}\n\n{}",
        SUMMARY_MESSAGE_PREAMBLE,
        clamp_summary_body(summary)
    )
}

/// Keep the head and the tail of an oversized summary rather than only the
/// head. `SUMMARY_SYSTEM_PROMPT` constrains what a summary contains, not the
/// order it says it in, so we cannot know which end carries the "what to do
/// next" part: keeping both ends is the hedge, keeping one is a bet.
fn clamp_summary_body(summary: &str) -> String {
    let trimmed = summary.trim();
    if trimmed.len() <= SUMMARY_MESSAGE_MAX_CHARS {
        return trimmed.to_string();
    }

    // The marker is part of what we store, so it comes out of the budget: the
    // result never exceeds SUMMARY_MESSAGE_MAX_CHARS.
    let content_budget = SUMMARY_MESSAGE_MAX_CHARS - SUMMARY_BODY_OMISSION_MARKER.len();
    let head_len = content_budget / 2;
    let tail_len = content_budget - head_len;
    format!(
        "{}{}{}",
        safe_prefix(trimmed, head_len),
        SUMMARY_BODY_OMISSION_MARKER,
        safe_suffix(trimmed, tail_len)
    )
}

fn transcript_for_summary(messages: &[AssistantMessage]) -> String {
    let rendered = render_transcript(messages);
    if rendered.len() <= SUMMARY_TRANSCRIPT_MAX_CHARS {
        return format!("Transcript to summarize:\n\n{}", rendered);
    }

    let head_len = SUMMARY_TRANSCRIPT_MAX_CHARS / 3;
    let tail_len = SUMMARY_TRANSCRIPT_MAX_CHARS - head_len;
    let head = safe_prefix(&rendered, head_len);
    let tail = safe_suffix(&rendered, tail_len);
    format!(
        "Transcript to summarize. The middle was omitted because it exceeded the summarizer budget; preserve all concrete information visible here.\n\n{}\n\n[... middle omitted during compaction ...]\n\n{}",
        head, tail
    )
}

fn render_transcript(messages: &[AssistantMessage]) -> String {
    render_messages(
        messages,
        SUMMARY_TOOL_CALL_MAX_CHARS,
        SUMMARY_TOOL_RESULT_MAX_CHARS,
    )
    .join("\n\n")
}

/// Render each message to a standalone string (with a `[role message id]`
/// header), capping tool payloads at the given sizes. Returns one entry per
/// message so callers can select whole messages without cutting mid-message.
fn render_messages(
    messages: &[AssistantMessage],
    tool_call_max: usize,
    tool_result_max: usize,
) -> Vec<String> {
    messages
        .iter()
        .map(|message| {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };
            let body = render_content_parts(&message.content, tool_call_max, tool_result_max);
            format!("[{} message {}]\n{}", role, message.id, body)
        })
        .collect()
}

fn render_content_parts(
    content: &[ContentPart],
    tool_call_max: usize,
    tool_result_max: usize,
) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.clone()),
            ContentPart::Thinking { .. } => None,
            ContentPart::ToolUse {
                tool_name,
                arguments,
                ..
            } => Some(format!(
                "[tool call: {} {}]",
                tool_name,
                truncate_json(arguments, tool_call_max)
            )),
            ContentPart::ToolResult { payload, .. } => Some(format!(
                "[tool result: {}]",
                truncate_json(payload, tool_result_max)
            )),
            // The summariser doesn't need pixels — a placeholder keeps the
            // turn structure without shipping image bytes (and the summary
            // model may lack vision).
            ContentPart::Image { .. } => Some("[image]".to_string()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Estimate the size of the request about to be sent, in characters.
///
/// A close estimator of the outgoing payload, not exact accounting: every
/// message of `view` — including the compaction summary, which is the first
/// message of the view — is measured at its rendered text size (full tool
/// arguments and results, not the summariser's caps) plus a fixed
/// per-message overhead; the system prompt is counted because it is
/// prepended to every call; tool definitions are counted by name,
/// description, and rendered JSON schema.
///
/// Counting the summary cannot make compaction chase its own summary: the
/// message-count gate only counts non-summary messages. (A pass normally
/// leaves `RECENT_TAIL_MESSAGES` behind; `group_aware_boundary` can retain a
/// straddled tool group and leave more, in which case hysteresis defers the
/// follow-up pass until enough new messages accumulate.)
///
/// It under-counts by construction: assistant thinking content is not
/// rendered at all, images count as placeholders rather than base64 bytes,
/// and JSON escaping inflates the wire payload past the rendered text. The
/// forced context-limit recovery path remains the backstop for what slips
/// past the estimate.
fn request_chars(
    view: &[AssistantMessage],
    system_prompt: Option<&ProviderInputMessage>,
    tools: &[ToolDefinition],
) -> usize {
    let message_chars: usize = view
        .iter()
        .map(|message| {
            render_content_parts(&message.content, usize::MAX, usize::MAX).len()
                + REQUEST_OVERHEAD_CHARS_PER_MESSAGE
        })
        .sum();
    let prompt_chars = system_prompt
        .map(|message| render_content_parts(&message.content, usize::MAX, usize::MAX).len())
        .unwrap_or(0);
    let tool_chars: usize = tools
        .iter()
        .map(|tool| {
            tool.name.len()
                + tool.description.len()
                + serde_json::to_string(&tool.input_schema)
                    .map(|value| value.len())
                    .unwrap_or_default()
        })
        .sum();
    message_chars + prompt_chars + tool_chars
}

fn truncate_json(value: &serde_json::Value, max_chars: usize) -> String {
    let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    if rendered.len() <= max_chars {
        rendered
    } else {
        format!("{}...[truncated]", safe_prefix(&rendered, max_chars))
    }
}

fn safe_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn safe_suffix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len().saturating_sub(max_bytes);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

fn provider_error_message(error: ProviderError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::{ContentPart, MessageRole};

    fn msg(id: &str, role: MessageRole, parts: Vec<ContentPart>) -> AssistantMessage {
        AssistantMessage {
            id: id.to_string(),
            session_id: "s".to_string(),
            role,
            content: parts,
            created_at: 0,
            provider_metadata: None,
        }
    }

    fn text(t: &str) -> ContentPart {
        ContentPart::Text {
            text: t.to_string(),
        }
    }

    fn tool_use_msg(id: &str, call_ids: &[&str]) -> AssistantMessage {
        msg(
            id,
            MessageRole::Assistant,
            call_ids
                .iter()
                .map(|call_id| ContentPart::ToolUse {
                    tool_call_id: (*call_id).to_string(),
                    tool_name: "probe".to_string(),
                    arguments: serde_json::Value::Null,
                })
                .collect(),
        )
    }

    fn tool_result_msg(id: &str, call_id: &str) -> AssistantMessage {
        msg(
            id,
            MessageRole::Tool,
            vec![ContentPart::ToolResult {
                tool_call_id: call_id.to_string(),
                payload: serde_json::Value::Null,
                started_at: None,
                completed_at: None,
            }],
        )
    }

    /// `n` alternating user/assistant text messages, none of them tool-related.
    fn filler(prefix: &str, n: usize) -> Vec<AssistantMessage> {
        (0..n)
            .map(|i| {
                let role = if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                };
                msg(&format!("{prefix}{i}"), role, vec![text("filler")])
            })
            .collect()
    }

    /// An assistant issuing `n` parallel calls, followed by all `n` results.
    fn tool_group(prefix: &str, n: usize) -> Vec<AssistantMessage> {
        let call_ids: Vec<String> = (0..n).map(|i| format!("{prefix}call{i}")).collect();
        let refs: Vec<&str> = call_ids.iter().map(|s| s.as_str()).collect();
        let mut out = vec![tool_use_msg(&format!("{prefix}asst"), &refs)];
        out.extend(
            call_ids
                .iter()
                .enumerate()
                .map(|(i, call_id)| tool_result_msg(&format!("{prefix}res{i}"), call_id)),
        );
        out
    }

    #[test]
    fn automatic_compaction_requires_the_minimum_message_count() {
        let mut view = filler(
            "f",
            MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES - 1,
        );
        view[0].content = vec![text(&"x".repeat(AUTO_COMPACTION_REQUEST_CHARS))];

        assert!(!should_auto_compact(&view, None, &[], None));
    }

    #[test]
    fn automatic_compaction_requires_the_estimated_size_threshold() {
        let mut view = filler("f", MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES);

        assert!(!should_auto_compact(&view, None, &[], None));

        view[0].content = vec![text(&"x".repeat(AUTO_COMPACTION_REQUEST_CHARS))];

        assert!(should_auto_compact(&view, None, &[], None));
    }

    /// The compaction summary is the first message of the provider view, so it
    /// is part of the prompt `should_auto_compact` is sizing. The scenario is a
    /// session that has compacted once and whose *new* history is still under
    /// budget: with the summary uncounted it read as under budget, while the
    /// prompt actually being sent was over it.
    ///
    /// The summary here is exactly `SUMMARY_MESSAGE_MAX_CHARS`, the cap a stored
    /// summary is clamped to, so this is a state production can really reach.
    #[test]
    fn estimated_size_counts_the_compaction_summary() {
        let message_count = MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES;
        // Sized so the history alone sits below the trigger while history plus a
        // full-size summary sits above it, with margin on both sides.
        let chars_per_message =
            (AUTO_COMPACTION_REQUEST_CHARS - SUMMARY_MESSAGE_MAX_CHARS / 2) / message_count;
        let mut view: Vec<AssistantMessage> = (0..message_count)
            .map(|i| {
                msg(
                    &format!("f{i}"),
                    MessageRole::User,
                    vec![text(&"y".repeat(chars_per_message))],
                )
            })
            .collect();
        assert!(
            !should_auto_compact(&view, None, &[], None),
            "the history on its own is under budget"
        );

        let mut summary = msg(
            "summary",
            MessageRole::System,
            vec![text(&summary_message_text(
                &"x".repeat(SUMMARY_MESSAGE_MAX_CHARS),
            ))],
        );
        summary.provider_metadata = Some(serde_json::json!({
            "source": COMPACTION_METADATA_SOURCE,
        }));
        view.insert(0, summary);

        assert!(
            should_auto_compact(&view, None, &[], None),
            "the summary is sent to the provider too, so it has to be counted"
        );
    }

    /// ...but it must not count toward the *message* gate, or a single large
    /// summary would let a nearly empty history compact itself again.
    #[test]
    fn the_compaction_summary_does_not_count_toward_the_message_gate() {
        let mut view = filler(
            "f",
            MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES - 1,
        );
        let mut summary = msg(
            "summary",
            MessageRole::System,
            vec![text(&"x".repeat(AUTO_COMPACTION_REQUEST_CHARS))],
        );
        summary.provider_metadata = Some(serde_json::json!({
            "source": COMPACTION_METADATA_SOURCE,
        }));
        view.insert(0, summary);

        assert!(!should_auto_compact(&view, None, &[], None));
    }

    /// The trigger sizes the *outgoing request*, so the system prompt —
    /// prepended to every call by the engine — is part of the budget. A view
    /// under the threshold on its own must cross it once the prompt is
    /// included, or the gate protects a request that no longer exists.
    #[test]
    fn the_trigger_counts_the_system_prompt() {
        let mut view = filler("f", MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES);
        let chars_per_message = (AUTO_COMPACTION_REQUEST_CHARS - 40_000)
            / (MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES);
        for message in &mut view {
            message.content = vec![text(&"y".repeat(chars_per_message))];
        }
        let system_prompt = ProviderInputMessage {
            role: MessageRole::System,
            content: vec![text(&"z".repeat(80_000))],
        };

        assert!(!should_auto_compact(&view, None, &[], None));
        assert!(should_auto_compact(&view, Some(&system_prompt), &[], None));
    }

    /// The trigger measures tool payloads at the size they are *sent*, not at
    /// the summariser's caps. The predecessor estimate capped tool results at
    /// `SUMMARY_TOOL_RESULT_MAX_CHARS`, so a run dominated by oversized tool
    /// output could sit far past the provider's context limit while the gate
    /// read "under budget" — the exact crash auto-compaction exists to prevent.
    #[test]
    fn the_trigger_sizes_tool_payloads_at_their_real_size() {
        let message_count = MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES;
        let mut view = filler("f", message_count - 1);
        view.push(msg(
            "fat",
            MessageRole::Tool,
            vec![ContentPart::ToolResult {
                tool_call_id: "call".to_string(),
                payload: serde_json::Value::String(
                    "z".repeat(AUTO_COMPACTION_REQUEST_CHARS).to_string(),
                ),
                started_at: None,
                completed_at: None,
            }],
        ));

        assert!(should_auto_compact(&view, None, &[], None));

        // The same shape at a tenth of the size stays under the trigger.
        let capped = view
            .iter()
            .map(|message| {
                let mut message = message.clone();
                if message.id == "fat" {
                    message.content = vec![ContentPart::ToolResult {
                        tool_call_id: "call".to_string(),
                        payload: serde_json::Value::String(
                            "z".repeat(AUTO_COMPACTION_REQUEST_CHARS / 10).to_string(),
                        ),
                        started_at: None,
                        completed_at: None,
                    }];
                }
                message
            })
            .collect::<Vec<_>>();
        assert!(!should_auto_compact(&capped, None, &[], None));
    }

    /// Hysteresis: a just-compacted session's retained tail satisfies the
    /// message-count gate on its own, so the trigger must additionally require
    /// fresh messages after the standing summary. Without this, a 40-message
    /// view that compacts leaves a 24-message tail that re-triggers next turn
    /// and summarizes the session's own fresh context away, forever.
    #[test]
    fn the_trigger_rearms_only_after_fresh_messages_follow_a_compaction() {
        let message_count = MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES;
        let mut view = filler("f", message_count);
        for message in &mut view {
            message.content = vec![text(
                &"y".repeat(AUTO_COMPACTION_REQUEST_CHARS / message_count + 1),
            )];
        }
        let mut summary = msg(
            "summary",
            MessageRole::System,
            vec![text(&summary_message_text("done"))],
        );
        summary.provider_metadata = Some(serde_json::json!({
            "source": COMPACTION_METADATA_SOURCE,
        }));
        view.insert(0, summary);

        assert!(
            should_auto_compact(&view, None, &[], None),
            "a never-compacted session is not gated by hysteresis"
        );
        assert!(
            !should_auto_compact(&view, None, &[], Some(0)),
            "the freshly completed pass leaves nothing new to compact"
        );
        assert!(!should_auto_compact(
            &view,
            None,
            &[],
            Some(MIN_MESSAGES_SINCE_COMPACTION - 1)
        ));
        assert!(should_auto_compact(
            &view,
            None,
            &[],
            Some(MIN_MESSAGES_SINCE_COMPACTION)
        ));
    }

    /// The forced path (`/compact`) must not be touched by the automatic
    /// gates: `select_compaction_window(force = true)` selects a window from a
    /// view that every gate above rejects.
    #[test]
    fn manual_compaction_ignores_the_automatic_gates() {
        let view = filler("f", MIN_MANUAL_COMPACT_MESSAGES + 2);

        assert!(!should_auto_compact(
            &view,
            None,
            &[],
            Some(MIN_MESSAGES_SINCE_COMPACTION)
        ));

        let window = select_compaction_window(&view, true).expect("manual window");
        assert!(!window.messages.is_empty());
    }

    /// The summariser input is budgeted: an oversized transcript is cut to a
    /// head and a tail rather than sent whole. Two deleted tests used to guard
    /// this budget; it had no coverage since. The only slack over the budget is
    /// the fixed framing the function adds around the transcript.
    #[test]
    fn the_summary_transcript_stays_within_its_budget() {
        // The cut branch adds two fixed string literals around the transcript
        // (the instruction line and the omission marker) and nothing else, so
        // the overhead is exact, not approximate.
        let framing_chars = 186;
        let view: Vec<AssistantMessage> = (0..40)
            .map(|i| {
                msg(
                    &format!("m{i}"),
                    MessageRole::User,
                    vec![text(
                        &format!("{i}").repeat(SUMMARY_TRANSCRIPT_MAX_CHARS / 8),
                    )],
                )
            })
            .collect();
        let rendered = render_transcript(&view);
        assert!(rendered.len() > SUMMARY_TRANSCRIPT_MAX_CHARS);

        let transcript = transcript_for_summary(&view);

        assert_eq!(
            transcript.len(),
            SUMMARY_TRANSCRIPT_MAX_CHARS + framing_chars,
            "the cut transcript is the budget plus fixed framing, nothing else"
        );
        // Both ends survive: the cut takes the middle, not the tail.
        assert!(transcript.contains("[user message m0]"));
        assert!(transcript.contains("[user message m39]"));
    }

    /// A transcript that fits is passed through untouched, so the budget test
    /// above cannot pass by clamping everything.
    #[test]
    fn a_small_summary_transcript_is_not_cut() {
        let view = filler("f", 4);

        let transcript = transcript_for_summary(&view);

        assert!(transcript.starts_with("Transcript to summarize:\n\n"));
        assert!(!transcript.contains("middle omitted"));
        assert!(transcript.contains("[user message f0]"));
        assert!(transcript.contains("[assistant message f3]"));
    }

    #[test]
    fn forced_compaction_accepts_manual_minimum_history() {
        let view = filler("m", MIN_MANUAL_COMPACT_MESSAGES);

        assert!(select_compaction_window(&view, false).is_none());

        let window = select_compaction_window(&view, true).expect("manual window");

        assert_eq!(window.messages.len(), MIN_MANUAL_COMPACT_MESSAGES);
        assert_eq!(window.source_from_message_id.as_deref(), Some("m0"));
        assert_eq!(window.source_to_message_id.as_deref(), Some("m1"));
    }

    /// The invariant the compaction boundary must preserve: every tool result
    /// left in the retained tail still has the assistant that issued it in the
    /// tail. A violation is invisible at compaction time and only shows up as a
    /// dropped orphan inside `normalize_history_for_provider`, on every later turn.
    ///
    /// Assumes `view` contains no `System` messages, so window indices line up
    /// with `view` indices.
    fn assert_no_split_tool_group(view: &[AssistantMessage], window: &CompactionWindow) {
        let retained = &view[window.messages.len()..];
        for message in retained.iter().filter(|m| m.role == MessageRole::Tool) {
            for part in &message.content {
                let ContentPart::ToolResult { tool_call_id, .. } = part else {
                    continue;
                };
                let owned = retained.iter().any(|candidate| {
                    candidate.content.iter().any(|p| {
                        matches!(p, ContentPart::ToolUse { tool_call_id: id, .. } if id == tool_call_id)
                    })
                });
                assert!(
                    owned,
                    "tool result {tool_call_id} was retained but its owning assistant was compacted away"
                );
            }
        }
    }

    #[test]
    fn compaction_boundary_is_untouched_when_it_falls_between_turns() {
        let view = filler("f", 45);

        let window = select_compaction_window(&view, false).expect("window");

        // 45 - RECENT_TAIL_MESSAGES(16) = 29, taken as-is: nothing to protect.
        assert_eq!(window.messages.len(), 29);
        assert_eq!(window.source_from_message_id.as_deref(), Some("f0"));
        assert_eq!(window.source_to_message_id.as_deref(), Some("f28"));
        assert_no_split_tool_group(&view, &window);
    }

    #[test]
    fn compaction_boundary_retreats_rather_than_splitting_a_tool_group() {
        // Shaped like the case observed live: a 16-way parallel tool batch whose
        // size exactly equals RECENT_TAIL_MESSAGES, so the positional boundary
        // lands on the batch's first result and orphans all 16 of them.
        let mut view = filler("f", 28);
        view.extend(tool_group("g", 16));
        assert_eq!(view.len(), 45);
        assert_eq!(view[29].role, MessageRole::Tool, "boundary lands mid-group");

        let window = select_compaction_window(&view, false).expect("window");

        // Boundary pulled back from 29 to 28 so the whole group stays together.
        assert_eq!(window.messages.len(), 28);
        assert_eq!(window.source_to_message_id.as_deref(), Some("f27"));
        assert!(
            !window.messages.iter().any(|m| m.id == "gasst"),
            "the assistant owning the batch must not be compacted"
        );
        assert_no_split_tool_group(&view, &window);
    }

    #[test]
    fn compaction_boundary_swallows_a_group_too_early_to_retain() {
        // The group starts before MIN_AUTOMATIC_COMPACT_MESSAGES, so retreating
        // would leave too little to summarise. It must be compacted whole instead.
        let mut view = filler("a", 22);
        view.extend(tool_group("g", 5));
        view.extend(filler("b", 12));
        assert_eq!(view.len(), 40);
        assert_eq!(view[24].role, MessageRole::Tool, "boundary lands mid-group");

        let window = select_compaction_window(&view, false).expect("window");

        // Boundary advanced from 24 past the last result at 27.
        assert_eq!(window.messages.len(), 28);
        assert!(
            window.messages.iter().any(|m| m.id == "gasst"),
            "the whole group belongs in the window"
        );
        assert_eq!(
            window
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::Tool)
                .count(),
            5,
            "all five results compacted with their assistant"
        );
        assert_no_split_tool_group(&view, &window);
    }

    #[test]
    fn compaction_declines_when_the_tail_is_one_giant_tool_group() {
        // Retreating drops below the minimum and advancing would consume the
        // entire tail, so the only safe answer is to wait for more messages.
        let mut view = filler("f", 23);
        view.extend(tool_group("g", 16));
        assert_eq!(view.len(), 40);
        assert_eq!(view[24].role, MessageRole::Tool, "boundary lands mid-group");

        assert!(select_compaction_window(&view, false).is_none());
    }

    #[test]
    fn compaction_boundary_tolerates_results_whose_assistant_is_already_gone() {
        // A view that opens mid-group (the owner was compacted in an earlier
        // round). There is nothing left to protect, so the boundary stands and
        // the pre-existing orphan is not made worse.
        let mut view: Vec<AssistantMessage> = (0..30)
            .map(|i| tool_result_msg(&format!("orphan{i}"), &format!("lost{i}")))
            .collect();
        view.extend(filler("f", 15));
        assert_eq!(view.len(), 45);

        let window = select_compaction_window(&view, false).expect("window");

        assert_eq!(window.messages.len(), 29);
    }

    #[test]
    fn context_limit_classifier_matches_codex_turn_start_input_too_large() {
        assert!(is_context_limit_error(
            "Error: turn/start: Input exceeds the maximum length of 1048576 characters (input_too_large, actual_chars=1072355)"
        ));
    }

    // --- R-comp.7: the stored summary is bounded --------------------------------

    #[test]
    fn a_summary_exactly_at_the_budget_is_stored_verbatim() {
        let body = "a".repeat(SUMMARY_MESSAGE_MAX_CHARS);
        let out = summary_message_text(&body);

        assert!(out.ends_with(&body), "the body must not be touched");
        assert!(!out.contains(SUMMARY_BODY_OMISSION_MARKER));
    }

    #[test]
    fn an_oversized_summary_is_clamped_and_keeps_both_ends() {
        let body = format!(
            "{}{}{}",
            "HEAD-MARKER",
            "x".repeat(SUMMARY_MESSAGE_MAX_CHARS * 4),
            "TAIL-MARKER"
        );

        let clamped = clamp_summary_body(&body);

        assert!(
            clamped.len() <= SUMMARY_MESSAGE_MAX_CHARS,
            "clamped to {} bytes, budget is {}",
            clamped.len(),
            SUMMARY_MESSAGE_MAX_CHARS
        );
        assert!(clamped.starts_with("HEAD-MARKER"), "head was dropped");
        assert!(clamped.ends_with("TAIL-MARKER"), "tail was dropped");
        assert!(
            clamped.contains(SUMMARY_BODY_OMISSION_MARKER),
            "cut is silent"
        );
    }

    #[test]
    fn clamping_an_oversized_summary_keeps_the_recovery_preamble() {
        let out = summary_message_text(&"y".repeat(SUMMARY_MESSAGE_MAX_CHARS * 3));

        assert!(out.starts_with(SUMMARY_MESSAGE_PREAMBLE));
        assert!(out.contains("history_query"));
    }

    #[test]
    fn clamping_does_not_split_a_multi_byte_character() {
        // '€' is 3 bytes, so both cut points land mid-character.
        let body = "\u{20ac}".repeat(SUMMARY_MESSAGE_MAX_CHARS);

        let clamped = clamp_summary_body(&body);

        let without_marker = clamped.replace(SUMMARY_BODY_OMISSION_MARKER, "");
        assert!(
            without_marker.chars().all(|c| c == '\u{20ac}'),
            "a cut landed inside a character"
        );
    }

    #[test]
    fn summary_message_text_includes_recovery_guidance() {
        let out = summary_message_text("the summary body");
        assert!(out.contains("the summary body"));
        assert!(out.contains(".clai/memory/"));
        assert!(out.contains(".clai/data.sqlite"));
        assert!(out.contains("history_query"));
    }

    // --- R-comp.6: context-limit failure text ---------------------------------

    const CTX_ERROR: &str = "input length and `max_tokens` exceed context limit";

    #[test]
    fn context_limit_message_passes_through_when_the_provider_already_advises_compacting() {
        let provider = "Context low · run `/compact` to compact & continue";
        assert_eq!(
            context_limit_failure_message(
                "Claude Code",
                provider,
                &CompactionAttempt::NotAttempted
            ),
            provider,
            "nothing is known about compaction and the provider already said what to do"
        );
    }

    #[test]
    fn context_limit_message_without_an_attempt_keeps_the_provider_error() {
        let text =
            context_limit_failure_message("Codex", CTX_ERROR, &CompactionAttempt::NotAttempted);
        assert!(text.starts_with("Codex could not complete"), "{text}");
        assert!(text.contains(CTX_ERROR), "{text}");
        assert!(
            text.contains("Run `/compact` or start a new thread"),
            "{text}"
        );
    }

    #[test]
    fn context_limit_message_reports_a_history_that_cannot_shrink() {
        let text = context_limit_failure_message(
            "The request",
            CTX_ERROR,
            &CompactionAttempt::NothingToCompact,
        );
        assert!(text.contains("found nothing it could summarize"), "{text}");
        assert!(text.contains("start a new thread"), "{text}");
        assert!(
            !text.contains("Run `/compact` or"),
            "compacting again cannot help, so it must not be the advice: {text}"
        );
        assert!(text.contains(CTX_ERROR), "{text}");
    }

    #[test]
    fn context_limit_message_surfaces_the_compaction_error_verbatim() {
        // The point of R-comp.6: before this, the summariser error was
        // `warn!`-and-dropped and the user only ever saw CTX_ERROR.
        let text = context_limit_failure_message(
            "The request",
            CTX_ERROR,
            &CompactionAttempt::Failed("summary request failed: 401 Unauthorized".to_string()),
        );
        assert!(text.contains("Automatic compaction failed"), "{text}");
        assert!(
            text.contains("summary request failed: 401 Unauthorized"),
            "{text}"
        );
        assert!(
            text.contains(CTX_ERROR),
            "provider error must survive: {text}"
        );
    }

    #[test]
    fn a_failed_compaction_overrides_the_providers_compact_advice() {
        // `/compact` runs the same summariser that just failed, so passing the
        // provider's advice through unchanged would send the user in a circle.
        let text = context_limit_failure_message(
            "Claude Code",
            "Context low · run `/compact` to compact & continue",
            &CompactionAttempt::Failed("claude exited with status 1".to_string()),
        );
        assert!(text.contains("claude exited with status 1"), "{text}");
        assert!(text.contains("start a new thread"), "{text}");
    }

    #[test]
    fn a_successful_compaction_clears_an_earlier_failure() {
        // A transient summariser error followed by a compaction that worked:
        // the history did shrink, so blaming compaction would be misdirection.
        let mut attempt = CompactionAttempt::NotAttempted;
        attempt.record_failure("summary request failed: 429");
        attempt.record_success();
        let text = context_limit_failure_message("The request", CTX_ERROR, &attempt);
        assert!(!text.contains("429"), "compaction later succeeded: {text}");
        assert!(
            text.contains("Run `/compact` or start a new thread"),
            "{text}"
        );
    }

    #[test]
    fn attempt_keeps_the_first_failure_and_never_downgrades_it() {
        let mut attempt = CompactionAttempt::NotAttempted;
        attempt.record_nothing_to_compact();
        assert_eq!(attempt, CompactionAttempt::NothingToCompact);

        attempt.record_failure("first");
        attempt.record_failure("second");
        attempt.record_nothing_to_compact();
        assert_eq!(
            attempt,
            CompactionAttempt::Failed("first".to_string()),
            "the earliest failure is the one that let the context grow"
        );
    }

    /// DB-backed tests for `load_provider_history`, against a real migrated
    /// sqlite pool (same harness `repository_tests` uses).
    mod provider_history {
        use super::super::*;
        use crate::assistant::repository::{
            complete_compaction, create_compaction, create_message, create_session,
        };
        use crate::assistant::types::{SessionContext, SessionKind};
        use crate::config::ExecutionCapabilityConfig;
        use crate::db::test_support::workspace_pool;

        fn sample_context() -> SessionContext {
            SessionContext {
                workspace_id: Some("ws-1".to_string()),
                tool_scopes: vec![],
                mcp_server_ids: vec![],
                execution: ExecutionCapabilityConfig::default(),
                cli_session_id: None,
                cli_session_provider: None,
                automation_id: None,
                agent_workspace_id: None,
                automation_name: None,
                inter_agent_call: None,
                workspace_agents: vec![],
            }
        }

        async fn db_session() -> (tempfile::TempDir, crate::db::DbPool, AssistantSession) {
            let (tmp, pool) = workspace_pool().await;
            let session = create_session(
                &pool,
                crate::assistant::repository::CreateSessionParams {
                    kind: SessionKind::Interactive,
                    title: None,
                    context: sample_context(),
                },
            )
            .await
            .unwrap();
            (tmp, pool, session)
        }

        async fn add_message(
            pool: &crate::db::DbPool,
            session_id: &str,
            text: &str,
        ) -> AssistantMessage {
            create_message(
                pool,
                crate::assistant::repository::CreateMessageParams {
                    session_id: session_id.to_string(),
                    role: MessageRole::User,
                    content: vec![ContentPart::Text {
                        text: text.to_string(),
                    }],
                    provider_metadata: None,
                },
            )
            .await
            .unwrap()
        }

        async fn add_summary_message(
            pool: &crate::db::DbPool,
            session_id: &str,
            text: &str,
        ) -> AssistantMessage {
            create_message(
                pool,
                crate::assistant::repository::CreateMessageParams {
                    session_id: session_id.to_string(),
                    role: MessageRole::System,
                    content: vec![ContentPart::Text {
                        text: text.to_string(),
                    }],
                    provider_metadata: Some(serde_json::json!({
                        "source": COMPACTION_METADATA_SOURCE,
                    })),
                },
            )
            .await
            .unwrap()
        }

        async fn complete_compaction_over(
            pool: &crate::db::DbPool,
            session_id: &str,
            source_to: &AssistantMessage,
            summary: &AssistantMessage,
        ) {
            let compaction = create_compaction(
                pool,
                crate::assistant::repository::CreateCompactionParams {
                    session_id: session_id.to_string(),
                    trigger: CompactionTrigger::Automatic,
                    strategy: CompactionStrategy::LocalSummary,
                    source_from_message_id: None,
                    source_to_message_id: Some(source_to.id.clone()),
                    created_run_id: None,
                    protocol_id: "test-protocol".to_string(),
                    model_id: "test-model".to_string(),
                    input_message_count: 0,
                },
            )
            .await
            .unwrap();
            complete_compaction(pool, &compaction.id, &summary.id)
                .await
                .unwrap();
        }

        fn view_ids(history: &ProviderHistory) -> Vec<&str> {
            history.view.iter().map(|m| m.id.as_str()).collect()
        }

        /// The healthy compacted path: boundary row excluded, standing
        /// summary leads, stale summaries filtered, since-count right.
        #[tokio::test]
        async fn compacted_session_view_leads_with_summary_and_excludes_boundary_and_stale_summaries(
        ) {
            let (_tmp, pool, session) = db_session().await;

            // Pre-boundary history the first compaction summarizes.
            add_message(&pool, &session.id, "one").await;
            let m2 = add_message(&pool, &session.id, "two").await;
            add_message(&pool, &session.id, "three").await;
            let boundary = add_message(&pool, &session.id, "four").await;
            // Compaction #1 covers m1..m2 but its summary lands after `boundary`
            // in (created_at, id) order — realistic, since the summary row is
            // written after the window is chosen.
            let stale_summary = add_summary_message(&pool, &session.id, "summary one").await;
            complete_compaction_over(&pool, &session.id, &m2, &stale_summary).await;
            // Retained tail of the second compaction, plus one message that
            // arrives after the standing summary (counts toward hysteresis).
            let m5 = add_message(&pool, &session.id, "five").await;
            let summary = add_summary_message(&pool, &session.id, "summary two").await;
            complete_compaction_over(&pool, &session.id, &boundary, &summary).await;
            let m6 = add_message(&pool, &session.id, "six").await;

            let history = load_provider_history(&pool, &session.id).await.unwrap();

            // Boundary row itself (and everything before it) is gone; the
            // stale summary is filtered even though it sits after the
            // boundary in the total order; the standing summary leads.
            assert_eq!(
                view_ids(&history),
                vec![summary.id.as_str(), m5.id.as_str(), m6.id.as_str()]
            );
            assert_eq!(
                history.summary_message.as_ref().map(|m| m.id.as_str()),
                Some(summary.id.as_str())
            );
            // Only m6 was persisted after the standing summary, not the whole
            // tail (m5 predates it).
            assert_eq!(history.messages_since_compaction, Some(1));
            assert_eq!(
                history
                    .boundary
                    .as_ref()
                    .map(|(created_at, id)| (*created_at, id.as_str())),
                Some((boundary.created_at, boundary.id.as_str()))
            );
        }

        #[tokio::test]
        async fn never_compacted_session_returns_full_history() {
            let (_tmp, pool, session) = db_session().await;
            let m1 = add_message(&pool, &session.id, "one").await;
            let m2 = add_message(&pool, &session.id, "two").await;

            let history = load_provider_history(&pool, &session.id).await.unwrap();

            assert_eq!(view_ids(&history), vec![m1.id.as_str(), m2.id.as_str()]);
            assert!(history.summary_message.is_none());
            assert_eq!(history.messages_since_compaction, None);
            assert!(history.boundary.is_none());
        }

        /// A completed compaction whose summary row lives in another session
        /// is degenerate: the loader must fall back to the full-session view
        /// instead of splicing another session's rows into the tail.
        #[tokio::test]
        async fn degenerate_compaction_row_falls_back_to_full_history() {
            let (_tmp, pool, session) = db_session().await;
            let m1 = add_message(&pool, &session.id, "one").await;
            let m2 = add_message(&pool, &session.id, "two").await;

            let other = create_session(
                &pool,
                crate::assistant::repository::CreateSessionParams {
                    kind: SessionKind::Interactive,
                    title: None,
                    context: sample_context(),
                },
            )
            .await
            .unwrap();
            let foreign = add_message(&pool, &other.id, "elsewhere").await;
            complete_compaction_over(&pool, &session.id, &foreign, &foreign).await;

            let history = load_provider_history(&pool, &session.id).await.unwrap();

            assert_eq!(view_ids(&history), vec![m1.id.as_str(), m2.id.as_str()]);
            assert!(history.summary_message.is_none());
            assert_eq!(history.messages_since_compaction, None);
            assert!(history.boundary.is_none());
        }
    }
}

use futures::StreamExt;
use std::collections::HashSet;
use std::future::Future;
use std::ops::Range;
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

/// Estimated input tokens a request may hold before automatic compaction
/// runs. CLAI knows no per-model context capacity, so this is one engineering
/// default rather than a model limit: it leaves a 128k-token context room for
/// a 16k-token reply and for the estimator's error against the provider's own
/// tokenizer. Models with smaller windows fall back to forced context-limit
/// recovery, which uses the same compactor.
pub const ESTIMATED_INPUT_BUDGET_TOKENS: usize = 96_000;
/// Automatic compaction fires at 80% of the budget, so a compacted request
/// has headroom instead of compacting again on the next iteration.
const AUTO_COMPACTION_TRIGGER_TOKENS: usize = ESTIMATED_INPUT_BUDGET_TOKENS / 5 * 4;
/// Newest complete tool groups an automatic compaction keeps verbatim:
/// min(25% of the budget, 20k tokens). The newest group is kept regardless.
const VERBATIM_TAIL_TOKENS: usize = if ESTIMATED_INPUT_BUDGET_TOKENS / 4 < 20_000 {
    ESTIMATED_INPUT_BUDGET_TOKENS / 4
} else {
    20_000
};
/// Request framing per message (role, ids, JSON keys) that the text lacks.
const MESSAGE_FRAMING_TOKENS: usize = 4;
/// Fixed allowance per image part; base64 length is not a vision token count.
const IMAGE_TOKENS: usize = 1_600;
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

pub fn is_compaction_summary_message(message: &AssistantMessage) -> bool {
    message
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("source"))
        .and_then(|value| value.as_str())
        == Some(COMPACTION_METADATA_SOURCE)
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

#[derive(Debug, Clone)]
pub struct CompactionOutcome {
    pub compaction: AssistantCompaction,
    pub summary_message: AssistantMessage,
}

/// The conversation one run sends to the provider, owned in memory for the
/// whole run: `[standing summary] + the messages it does not cover`, in
/// `(created_at, id)` order. It is loaded once when the run starts; from then
/// on the run feeds it the exact rows it persists (`upsert`/`remove`) and the
/// queue rows it reads at request boundaries (`refresh_pending`), so history
/// is never re-read from the database while the run is active. Nothing here
/// is durable: the next run reconstructs the same view from persistence.
pub struct RunConversation {
    messages: Vec<AssistantMessage>,
    has_summary: bool,
    /// Queued user rows still pending delivery. They are pinned outside any
    /// compaction and reconciled against the queue at request boundaries.
    pending_ids: HashSet<String>,
    /// Boundary of the last automatic pass whose summary would not have
    /// shrunk the request; no automatic retry until the boundary moves.
    rejected_source_to: Option<String>,
}

impl RunConversation {
    /// The one history read of a run: every message plus the latest completed
    /// compaction, projected onto the provider view.
    pub async fn load(pool: &DbPool, session_id: &str) -> Result<Self, String> {
        let messages = repository::list_messages(pool, session_id).await?;
        let latest = repository::latest_completed_compaction(pool, session_id).await?;
        Ok(Self::from_history(&messages, latest.as_ref()))
    }

    pub fn from_history(
        messages: &[AssistantMessage],
        latest: Option<&AssistantCompaction>,
    ) -> Self {
        let messages = provider_view(messages, latest);
        let has_summary = messages.first().is_some_and(is_compaction_summary_message);
        Self {
            messages,
            has_summary,
            pending_ids: HashSet::new(),
            rejected_source_to: None,
        }
    }

    pub fn messages(&self) -> &[AssistantMessage] {
        &self.messages
    }

    pub fn summary(&self) -> Option<&AssistantMessage> {
        self.has_summary.then(|| &self.messages[0])
    }

    /// Messages after the standing summary.
    pub fn tail(&self) -> &[AssistantMessage] {
        &self.messages[self.summary_len()..]
    }

    pub fn latest_user_message(&self) -> Option<&AssistantMessage> {
        self.messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
    }

    /// Record a persisted row: replace an existing row with the same id
    /// (assistant placeholders are finalized in place), otherwise insert it at
    /// its `(created_at, id)` position after the summary.
    pub fn upsert(&mut self, message: AssistantMessage) {
        if let Some(existing) = self.messages.iter_mut().find(|m| m.id == message.id) {
            *existing = message;
            return;
        }
        let start = self.summary_len();
        let offset = self.messages[start..]
            .iter()
            .rposition(|m| {
                (m.created_at, m.id.as_str()) <= (message.created_at, message.id.as_str())
            })
            .map_or(0, |index| index + 1);
        self.messages.insert(start + offset, message);
    }

    pub fn remove(&mut self, message_id: &str) {
        self.messages.retain(|message| message.id != message_id);
        self.pending_ids.remove(message_id);
    }

    /// Reconcile with the queue's current pending rows: edits replace the
    /// row by id, newcomers are inserted, and a previously pending row that
    /// is neither pending nor delivered by this run was deleted by the user.
    pub fn refresh_pending(&mut self, pending: Vec<AssistantMessage>) {
        let fresh: HashSet<String> = pending.iter().map(|message| message.id.clone()).collect();
        self.messages.retain(|message| {
            fresh.contains(&message.id) || !self.pending_ids.contains(&message.id)
        });
        for message in pending {
            self.upsert(message);
        }
        self.pending_ids = fresh;
    }

    /// Pending queued rows in this conversation, in view order.
    pub fn pending_ids(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter(|message| self.pending_ids.contains(&message.id))
            .map(|message| message.id.clone())
            .collect()
    }

    pub fn mark_delivered(&mut self, message_ids: &[String]) {
        for id in message_ids {
            self.pending_ids.remove(id);
        }
    }

    fn summary_len(&self) -> usize {
        usize::from(self.has_summary)
    }

    /// Choose what a compaction summarizes: `[standing summary] + the oldest
    /// complete tool groups`, keeping the newest groups verbatim within
    /// `VERBATIM_TAIL_TOKENS` (only the newest group when `force`d), and never
    /// a pending queued row or anything after it. `None` when no raw message
    /// is eligible: a summary is never re-summarized on its own.
    fn plan_compaction(&self, force: bool) -> Option<CompactionPlan> {
        let summary_len = self.summary_len();
        let tail = &self.messages[summary_len..];
        let groups = tool_groups(tail);
        let newest = groups.last()?;
        let tokens: Vec<usize> = tail.iter().map(message_tokens).collect();
        let group_tokens = |group: &Range<usize>| tokens[group.clone()].iter().sum::<usize>();

        let tail_budget = if force { 0 } else { VERBATIM_TAIL_TOKENS };
        let mut kept = group_tokens(newest);
        let mut keep_from = groups.len() - 1;
        while keep_from > 0 && kept + group_tokens(&groups[keep_from - 1]) <= tail_budget {
            keep_from -= 1;
            kept += group_tokens(&groups[keep_from]);
        }
        let mut cut = groups[keep_from].start;
        if let Some(pending_at) = tail
            .iter()
            .position(|message| self.pending_ids.contains(&message.id))
        {
            let pinned = groups.iter().find(|group| group.contains(&pending_at))?;
            cut = cut.min(pinned.start);
        }
        if cut == 0 {
            return None;
        }

        let source_to_message_id = tail[cut - 1].id.clone();
        if !force && self.rejected_source_to.as_deref() == Some(source_to_message_id.as_str()) {
            return None;
        }
        let messages = self.messages[..summary_len + cut].to_vec();
        let prefix_tokens =
            self.summary().map(message_tokens).unwrap_or(0) + tokens[..cut].iter().sum::<usize>();
        Some(CompactionPlan {
            source_from_message_id: messages[0].id.clone(),
            source_to_message_id,
            messages,
            prefix_tokens,
        })
    }

    fn apply_compaction(&mut self, consumed: usize, summary_message: AssistantMessage) {
        self.messages.drain(..consumed);
        self.messages.insert(0, summary_message);
        self.has_summary = true;
        self.rejected_source_to = None;
    }
}

struct CompactionPlan {
    /// `[standing summary] + eligible raw prefix`: the summarizer input.
    messages: Vec<AssistantMessage>,
    source_from_message_id: String,
    /// The last raw message the new summary covers.
    source_to_message_id: String,
    prefix_tokens: usize,
}

/// `[latest valid summary] + non-summary messages after its boundary`, or
/// every non-summary message when the compaction row cannot be placed
/// (referenced rows deleted or belonging to another session).
fn provider_view(
    messages: &[AssistantMessage],
    latest: Option<&AssistantCompaction>,
) -> Vec<AssistantMessage> {
    let raw = |messages: &[AssistantMessage]| {
        messages
            .iter()
            .filter(|message| !is_compaction_summary_message(message))
            .cloned()
            .collect::<Vec<_>>()
    };
    let summary_and_boundary = latest.and_then(|compaction| {
        let summary = messages.iter().find(|message| {
            Some(message.id.as_str()) == compaction.summary_message_id.as_deref()
        })?;
        let boundary = messages.iter().position(|message| {
            Some(message.id.as_str()) == compaction.source_to_message_id.as_deref()
        })?;
        Some((summary, boundary))
    });
    match summary_and_boundary {
        Some((summary, boundary)) => {
            let mut view = vec![summary.clone()];
            view.extend(raw(&messages[boundary + 1..]));
            view
        }
        None => raw(messages),
    }
}

/// Partition messages into the groups a compaction cut must not split: an
/// assistant message together with the tool messages answering the calls it
/// issued. Everything else is its own group. Results follow their call, so a
/// group is a contiguous range.
pub(crate) fn tool_groups(messages: &[AssistantMessage]) -> Vec<Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    while start < messages.len() {
        let mut end = start + 1;
        let issued = tool_use_ids(&messages[start]);
        if !issued.is_empty() {
            while end < messages.len() && messages[end].role == MessageRole::Tool {
                let answered = tool_result_ids(&messages[end]);
                if answered.is_empty() || !answered.iter().all(|id| issued.contains(id)) {
                    break;
                }
                end += 1;
            }
        }
        groups.push(start..end);
        start = end;
    }
    groups
}

fn tool_use_ids(message: &AssistantMessage) -> HashSet<&str> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolUse { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_result_ids(message: &AssistantMessage) -> Vec<&str> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect()
}

/// Token count of `text` under the fixed `cl100k_base` encoding, which serves
/// every provider: an estimate of the provider's count, not its tokenizer's
/// answer. The encoding is embedded in the crate and initialized once.
pub fn text_tokens(text: &str) -> usize {
    tiktoken_rs::cl100k_base_singleton().count_ordinary(text)
}

/// A message as the provider receives it: text and thinking, tool calls and
/// results at full size, a fixed allowance per image, plus framing.
fn message_tokens(message: &AssistantMessage) -> usize {
    message
        .content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } | ContentPart::Thinking { text, .. } => text_tokens(text),
            ContentPart::ToolUse {
                tool_name,
                arguments,
                ..
            } => text_tokens(tool_name) + text_tokens(&json_string(arguments)),
            ContentPart::ToolResult { payload, .. } => text_tokens(&json_string(payload)),
            ContentPart::Image { .. } => IMAGE_TOKENS,
        })
        .sum::<usize>()
        + MESSAGE_FRAMING_TOKENS
}

/// Estimated tokens of the request about to be sent: system prompt, tool
/// definitions, and every message of the conversation.
pub fn estimate_request_tokens(
    messages: &[AssistantMessage],
    system_prompt: &str,
    tools: &[ToolDefinition],
) -> usize {
    let tool_tokens: usize = tools
        .iter()
        .map(|tool| {
            text_tokens(&tool.name)
                + text_tokens(&tool.description)
                + text_tokens(&json_string(&tool.input_schema))
        })
        .sum();
    text_tokens(system_prompt) + tool_tokens + messages.iter().map(message_tokens).sum::<usize>()
}

pub fn should_auto_compact(
    conversation: &RunConversation,
    system_prompt: &str,
    tools: &[ToolDefinition],
) -> bool {
    estimate_request_tokens(conversation.messages(), system_prompt, tools)
        >= AUTO_COMPACTION_TRIGGER_TOKENS
}

/// Compact the run's conversation in place: summarize the eligible prefix
/// (with the previous summary folded in once), persist the compaction and
/// its summary message exactly as before, then replace the prefix with the
/// summary. The conversation is left untouched when nothing is eligible or
/// persistence fails.
#[allow(clippy::too_many_arguments)]
pub async fn compact_conversation(
    pool: &DbPool,
    session: &AssistantSession,
    connection: &ProviderConnection,
    summary_working_dir: Option<&Path>,
    trigger: CompactionTrigger,
    run_id: Option<&str>,
    force: bool,
    conversation: &mut RunConversation,
) -> Result<Option<CompactionOutcome>, String> {
    compact_with(
        pool,
        &session.id,
        &connection.protocol_id,
        &connection.model_id,
        trigger,
        run_id,
        force,
        conversation,
        |messages| async move {
            summarize_window(session, connection, summary_working_dir, run_id, &messages).await
        },
    )
    .await
}

pub async fn compact_for_context_limit_recovery(
    pool: &DbPool,
    session: &AssistantSession,
    connection: &ProviderConnection,
    summary_working_dir: Option<&Path>,
    run_id: &str,
    conversation: &mut RunConversation,
) -> Result<Option<CompactionOutcome>, String> {
    compact_conversation(
        pool,
        session,
        connection,
        summary_working_dir,
        CompactionTrigger::ErrorRecovery,
        Some(run_id),
        true,
        conversation,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn compact_with<F, Fut>(
    pool: &DbPool,
    session_id: &str,
    protocol_id: &str,
    model_id: &str,
    trigger: CompactionTrigger,
    run_id: Option<&str>,
    force: bool,
    conversation: &mut RunConversation,
    summarize: F,
) -> Result<Option<CompactionOutcome>, String>
where
    F: FnOnce(Vec<AssistantMessage>) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let Some(plan) = conversation.plan_compaction(force) else {
        return Ok(None);
    };
    let strategy = if providers::is_cli_provider(protocol_id) {
        CompactionStrategy::SessionRotationSummary
    } else {
        CompactionStrategy::LocalSummary
    };

    let summary = summarize(plan.messages.clone()).await?;
    let summary_text = summary_message_text(&summary);
    if !force && text_tokens(&summary_text) >= plan.prefix_tokens {
        // The summary would not make the request smaller: keep the raw
        // messages and do not ask again until older context becomes eligible.
        conversation.rejected_source_to = Some(plan.source_to_message_id);
        return Ok(None);
    }

    let compaction = repository::create_compaction(
        pool,
        CreateCompactionParams {
            session_id: session_id.to_string(),
            trigger: trigger.clone(),
            strategy: strategy.clone(),
            source_from_message_id: Some(plan.source_from_message_id.clone()),
            source_to_message_id: Some(plan.source_to_message_id.clone()),
            created_run_id: run_id.map(str::to_string),
            protocol_id: protocol_id.to_string(),
            model_id: model_id.to_string(),
            input_message_count: plan.messages.len() as i64,
        },
    )
    .await?;

    let summary_message = repository::create_message(
        pool,
        CreateMessageParams {
            session_id: session_id.to_string(),
            role: MessageRole::System,
            content: vec![ContentPart::Text { text: summary_text }],
            provider_metadata: Some(serde_json::json!({
                "source": COMPACTION_METADATA_SOURCE,
                "compactionId": compaction.id,
                "trigger": trigger,
                "strategy": strategy,
                "sourceFromMessageId": plan.source_from_message_id,
                "sourceToMessageId": plan.source_to_message_id,
                "createdAt": chrono::Utc::now().timestamp_millis(),
            })),
        },
    )
    .await?;

    let compaction =
        repository::complete_compaction(pool, &compaction.id, &summary_message.id).await?;
    conversation.apply_compaction(plan.messages.len(), summary_message.clone());

    Ok(Some(CompactionOutcome {
        compaction,
        summary_message,
    }))
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

fn json_string(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn truncate_json(value: &serde_json::Value, max_chars: usize) -> String {
    let rendered = json_string(value);
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

    fn conversation(messages: Vec<AssistantMessage>) -> RunConversation {
        RunConversation::from_history(&messages, None)
    }

    fn summary_msg(id: &str, body: &str) -> AssistantMessage {
        let mut summary = msg(
            id,
            MessageRole::System,
            vec![text(&summary_message_text(body))],
        );
        summary.provider_metadata = Some(serde_json::json!({
            "source": COMPACTION_METADATA_SOURCE,
        }));
        summary
    }

    /// A user message of roughly `tokens` estimated tokens.
    fn heavy_msg(id: &str, tokens: usize) -> AssistantMessage {
        msg(id, MessageRole::User, vec![text(&" a".repeat(tokens))])
    }

    fn ids(messages: &[AssistantMessage]) -> Vec<&str> {
        messages.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn token_estimate_uses_the_tokenizer_not_a_character_ratio() {
        assert_eq!(text_tokens("hello world"), 2);
        assert_eq!(text_tokens(""), 0);
        // CJK and emoji cost several tokens per character; a chars/4 ratio
        // would under-count them badly.
        assert!(text_tokens("日本語のテキスト") >= 8);
        assert!(text_tokens("{\"a\":1}") >= 4);
    }

    #[test]
    fn request_estimate_counts_prompt_tools_payloads_images_and_framing() {
        let tool = ToolDefinition {
            name: "probe".to_string(),
            description: "look around".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let image = msg(
            "img",
            MessageRole::User,
            vec![ContentPart::Image {
                id: "i".to_string(),
                path: "p.png".to_string(),
                media_type: "image/png".to_string(),
                filename: None,
                width: None,
                height: None,
            }],
        );
        let fat = msg(
            "fat",
            MessageRole::Tool,
            vec![ContentPart::ToolResult {
                tool_call_id: "c".to_string(),
                payload: serde_json::Value::String(" a".repeat(1000)),
                started_at: None,
                completed_at: None,
            }],
        );

        assert_eq!(
            estimate_request_tokens(&[image], "", &[]),
            IMAGE_TOKENS + MESSAGE_FRAMING_TOKENS
        );
        assert!(estimate_request_tokens(&[fat], "", &[]) >= 1000);
        assert!(estimate_request_tokens(&[], "system prompt", &[]) > 0);
        assert!(estimate_request_tokens(&[], "", &[tool]) > 0);
    }

    #[test]
    fn automatic_trigger_is_token_pressure_not_message_count() {
        let many_small = conversation(filler("f", 200));
        assert!(!should_auto_compact(&many_small, "", &[]));

        let few_large = conversation(vec![
            heavy_msg("a", 30_000),
            heavy_msg("b", 30_000),
            heavy_msg("c", 30_000),
        ]);
        assert!(should_auto_compact(&few_large, "", &[]));
    }

    /// An assistant issuing `n` parallel calls whose results each weigh
    /// roughly `tokens_each` tokens.
    fn heavy_tool_group(prefix: &str, n: usize, tokens_each: usize) -> Vec<AssistantMessage> {
        let mut group = tool_group(prefix, n);
        for message in group.iter_mut().skip(1) {
            let ContentPart::ToolResult { payload, .. } = &mut message.content[0] else {
                unreachable!("tool_group emits results after the call");
            };
            *payload = serde_json::Value::String(" a".repeat(tokens_each));
        }
        group
    }

    #[test]
    fn tail_selection_keeps_recent_whole_groups_within_the_tail_budget() {
        let mut messages = vec![heavy_msg("old", 30_000)];
        messages.extend(heavy_tool_group("g1", 3, 5_000));
        messages.push(heavy_msg("mid", 10_000));
        messages.extend(tool_group("g2", 2));
        let conversation = conversation(messages);

        let plan = conversation.plan_compaction(false).expect("plan");

        // g2 + `mid` fit in 20k; g1 (~15k) does not, and it moves whole.
        assert_eq!(
            ids(&plan.messages),
            vec!["old", "g1asst", "g1res0", "g1res1", "g1res2"]
        );
        assert_eq!(plan.source_from_message_id, "old");
        assert_eq!(plan.source_to_message_id, "g1res2");
    }

    #[test]
    fn the_newest_group_is_kept_even_when_it_alone_exceeds_the_tail_budget() {
        let conversation = conversation(vec![heavy_msg("a", 100), heavy_msg("huge", 40_000)]);

        let plan = conversation.plan_compaction(false).expect("plan");

        assert_eq!(ids(&plan.messages), vec!["a"]);
    }

    #[test]
    fn forced_compaction_keeps_only_the_newest_group() {
        let mut messages = filler("f", 4);
        messages.extend(tool_group("g", 2));
        let conversation = conversation(messages);

        let plan = conversation.plan_compaction(true).expect("plan");

        assert_eq!(ids(&plan.messages), vec!["f0", "f1", "f2", "f3"]);
    }

    #[test]
    fn pending_queued_rows_are_pinned_outside_the_compacted_prefix() {
        let mut conversation = conversation(filler("f", 6));
        conversation.refresh_pending(vec![conversation.messages()[2].clone()]);

        let plan = conversation.plan_compaction(true).expect("plan");
        assert_eq!(ids(&plan.messages), vec!["f0", "f1"]);

        conversation.refresh_pending(vec![conversation.messages()[0].clone()]);
        assert!(conversation.plan_compaction(true).is_none());
    }

    #[test]
    fn a_summary_alone_is_never_resummarized() {
        let conversation = RunConversation {
            messages: vec![
                summary_msg("s", &"x".repeat(SUMMARY_MESSAGE_MAX_CHARS)),
                heavy_msg("only", 90_000),
            ],
            has_summary: true,
            pending_ids: HashSet::new(),
            rejected_source_to: None,
        };

        assert!(should_auto_compact(&conversation, "", &[]));
        assert!(conversation.plan_compaction(false).is_none());
        assert!(conversation.plan_compaction(true).is_none());
        assert_eq!(conversation.summary().map(|m| m.id.as_str()), Some("s"));
    }

    #[test]
    fn upsert_replaces_by_id_and_inserts_in_created_at_order() {
        let mut conversation = conversation(vec![
            AssistantMessage {
                created_at: 10,
                ..msg("a", MessageRole::User, vec![text("a")])
            },
            AssistantMessage {
                created_at: 30,
                ..msg("c", MessageRole::User, vec![text("c")])
            },
        ]);

        conversation.upsert(AssistantMessage {
            created_at: 20,
            ..msg("b", MessageRole::Assistant, vec![text("")])
        });
        conversation.upsert(AssistantMessage {
            created_at: 20,
            ..msg("b", MessageRole::Assistant, vec![text("final")])
        });
        conversation.upsert(AssistantMessage {
            created_at: 40,
            ..msg("d", MessageRole::Tool, vec![text("d")])
        });

        assert_eq!(ids(conversation.messages()), vec!["a", "b", "c", "d"]);
        assert!(
            matches!(&conversation.messages()[1].content[0], ContentPart::Text { text } if text == "final")
        );
    }

    #[test]
    fn refresh_pending_applies_edits_arrivals_deletions_and_delivery() {
        let mut conversation = conversation(vec![AssistantMessage {
            created_at: 10,
            ..msg("u1", MessageRole::User, vec![text("hello")])
        }]);
        let q1 = AssistantMessage {
            created_at: 20,
            ..msg("q1", MessageRole::User, vec![text("queued")])
        };
        let q2 = AssistantMessage {
            created_at: 30,
            ..msg("q2", MessageRole::User, vec![text("second")])
        };

        conversation.refresh_pending(vec![q1.clone(), q2.clone()]);
        assert_eq!(ids(conversation.messages()), vec!["u1", "q1", "q2"]);
        assert_eq!(conversation.pending_ids(), vec!["q1", "q2"]);

        // q1 edited, q2 deleted by the user.
        let edited = AssistantMessage {
            content: vec![text("queued, edited")],
            ..q1.clone()
        };
        conversation.refresh_pending(vec![edited]);
        assert_eq!(ids(conversation.messages()), vec!["u1", "q1"]);
        assert!(
            matches!(&conversation.messages()[1].content[0], ContentPart::Text { text } if text == "queued, edited")
        );

        // Delivered by this run: no longer pending, but part of history.
        conversation.mark_delivered(&["q1".to_string()]);
        conversation.refresh_pending(Vec::new());
        assert_eq!(ids(conversation.messages()), vec!["u1", "q1"]);
        assert!(conversation.pending_ids().is_empty());
    }

    /// Restart reconstruction from stored rows: the standing summary leads,
    /// the boundary row and everything before it are gone, a stale summary
    /// stored after the boundary is filtered, and equal timestamps do not
    /// disturb the boundary lookup because it is by id.
    #[test]
    fn from_history_reconstructs_the_view_from_the_latest_completed_compaction() {
        let mut messages = filler("m", 4);
        messages.push(summary_msg("stale", "first pass"));
        messages.push(msg("m4", MessageRole::User, vec![text("five")]));
        messages.push(summary_msg("standing", "second pass"));
        messages.push(msg("m5", MessageRole::Assistant, vec![text("six")]));
        let compaction = AssistantCompaction {
            id: "c".to_string(),
            session_id: "s".to_string(),
            trigger: CompactionTrigger::Automatic,
            strategy: CompactionStrategy::LocalSummary,
            status: crate::assistant::types::CompactionStatus::Completed,
            source_from_message_id: Some("m0".to_string()),
            source_to_message_id: Some("m3".to_string()),
            summary_message_id: Some("standing".to_string()),
            created_run_id: None,
            protocol_id: "p".to_string(),
            model_id: "m".to_string(),
            input_message_count: 4,
            created_at: 0,
            completed_at: Some(0),
            error: None,
        };

        let conversation = RunConversation::from_history(&messages, Some(&compaction));
        assert_eq!(ids(conversation.messages()), vec!["standing", "m4", "m5"]);
        assert_eq!(
            conversation.summary().map(|m| m.id.as_str()),
            Some("standing")
        );

        // A compaction whose rows are gone falls back to raw history.
        let orphan = AssistantCompaction {
            source_to_message_id: Some("missing".to_string()),
            ..compaction
        };
        let conversation = RunConversation::from_history(&messages, Some(&orphan));
        assert_eq!(
            ids(conversation.messages()),
            vec!["m0", "m1", "m2", "m3", "m4", "m5"]
        );
        assert!(conversation.summary().is_none());
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

    /// DB-backed run-flow tests against a real migrated sqlite pool (same
    /// harness `repository_tests` uses). The engine and CLI loops need a
    /// Tauri app handle, so the data flow they perform is driven here through
    /// the same repository writes and `RunConversation` calls.
    mod run_flow {
        use super::super::*;
        use super::text;
        use crate::assistant::repository::{
            complete_compaction, create_compaction, create_message, create_run, create_session,
            create_user_message_with_content, delete_pending_queued_message,
            list_pending_queued_messages, mark_queued_messages_delivered, update_message_content,
            update_pending_queued_message,
        };
        use crate::assistant::types::{SessionContext, SessionKind};
        use crate::config::ExecutionCapabilityConfig;
        use crate::db::test_support::workspace_pool;
        use std::sync::atomic::{AtomicI64, Ordering};
        use std::sync::{Arc, Mutex};

        /// Rows written back-to-back share a millisecond and would then be
        /// ordered by random id; stamp strictly increasing timestamps so the
        /// tests assert on insertion order.
        static CLOCK: AtomicI64 = AtomicI64::new(1_000_000);

        async fn stamp(pool: &crate::db::DbPool, message: AssistantMessage) -> AssistantMessage {
            let created_at = CLOCK.fetch_add(1_000, Ordering::SeqCst);
            sqlx::query("UPDATE assistant_messages SET created_at = ? WHERE id = ?")
                .bind(created_at)
                .bind(&message.id)
                .execute(pool)
                .await
                .unwrap();
            AssistantMessage {
                created_at,
                ..message
            }
        }

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

        async fn add(
            pool: &crate::db::DbPool,
            session_id: &str,
            role: MessageRole,
            content: Vec<ContentPart>,
        ) -> AssistantMessage {
            let message = create_message(
                pool,
                crate::assistant::repository::CreateMessageParams {
                    session_id: session_id.to_string(),
                    role,
                    content,
                    provider_metadata: None,
                },
            )
            .await
            .unwrap();
            stamp(pool, message).await
        }

        async fn add_queued(
            pool: &crate::db::DbPool,
            session_id: &str,
            t: &str,
        ) -> AssistantMessage {
            let message = create_user_message_with_content(
                pool,
                session_id.to_string(),
                vec![text(t)],
                Some("conn-1"),
            )
            .await
            .unwrap();
            stamp(pool, message).await
        }

        async fn add_text(pool: &crate::db::DbPool, session_id: &str, t: &str) -> AssistantMessage {
            add(pool, session_id, MessageRole::User, vec![text(t)]).await
        }

        fn view_ids(conversation: &RunConversation) -> Vec<String> {
            conversation
                .messages()
                .iter()
                .map(|m| m.id.clone())
                .collect()
        }

        /// A summarizer that records what it was asked to summarize.
        fn recording_summarizer(
            calls: &Arc<Mutex<Vec<Vec<String>>>>,
            summary: &'static str,
        ) -> impl FnOnce(Vec<AssistantMessage>) -> std::future::Ready<Result<String, String>>
        {
            let calls = calls.clone();
            move |messages| {
                calls
                    .lock()
                    .unwrap()
                    .push(messages.iter().map(|m| m.id.clone()).collect());
                std::future::ready(Ok(summary.to_string()))
            }
        }

        /// Several assistant/tool iterations: the conversation loaded once at
        /// run start, fed only the rows the run persists, ends identical to
        /// what the next run reconstructs from the database.
        #[tokio::test]
        async fn run_owned_conversation_mirrors_persisted_rows_without_rereading_history() {
            let (_tmp, pool, session) = db_session().await;
            add_text(&pool, &session.id, "do the thing").await;
            let mut conversation = RunConversation::load(&pool, &session.id).await.unwrap();

            for i in 0..3 {
                let placeholder =
                    add(&pool, &session.id, MessageRole::Assistant, vec![text("")]).await;
                conversation.upsert(placeholder.clone());
                let call_id = format!("call{i}");
                let finalized = update_message_content(
                    &pool,
                    &placeholder.id,
                    &[ContentPart::ToolUse {
                        tool_call_id: call_id.clone(),
                        tool_name: "probe".to_string(),
                        arguments: serde_json::json!({"i": i}),
                    }],
                )
                .await
                .unwrap();
                conversation.upsert(finalized);
                let result = add(
                    &pool,
                    &session.id,
                    MessageRole::Tool,
                    vec![ContentPart::ToolResult {
                        tool_call_id: call_id,
                        payload: serde_json::json!({"ok": i % 2 == 0}),
                        started_at: None,
                        completed_at: None,
                    }],
                )
                .await;
                conversation.upsert(result);
            }

            let reloaded = RunConversation::load(&pool, &session.id).await.unwrap();
            assert_eq!(view_ids(&conversation), view_ids(&reloaded));
            assert_eq!(conversation.messages().len(), 7);
            assert_eq!(
                serde_json::to_value(&conversation.messages()[1].content).unwrap(),
                serde_json::to_value(&reloaded.messages()[1].content).unwrap(),
                "the finalized row replaced the placeholder in memory"
            );
        }

        #[tokio::test]
        async fn queue_refresh_tracks_pending_edits_deletions_and_delivery_in_the_database() {
            let (_tmp, pool, session) = db_session().await;
            add_text(&pool, &session.id, "first").await;
            let mut conversation = RunConversation::load(&pool, &session.id).await.unwrap();

            let queued = add_queued(&pool, &session.id, "queued").await;
            let pending = |rows: Vec<crate::assistant::repository::QueuedUserMessage>| {
                rows.into_iter().map(|q| q.message).collect::<Vec<_>>()
            };
            conversation.refresh_pending(pending(
                list_pending_queued_messages(&pool, &session.id)
                    .await
                    .unwrap(),
            ));
            assert_eq!(conversation.pending_ids(), vec![queued.id.clone()]);

            update_pending_queued_message(&pool, &session.id, &queued.id, "edited".to_string())
                .await
                .unwrap();
            conversation.refresh_pending(pending(
                list_pending_queued_messages(&pool, &session.id)
                    .await
                    .unwrap(),
            ));
            assert!(matches!(
                &conversation.messages()[1].content[0],
                ContentPart::Text { text } if text == "edited"
            ));

            // Sent to the provider: marked delivered, stays in history.
            let run = create_run(
                &pool,
                crate::assistant::repository::CreateRunParams {
                    session_id: session.id.clone(),
                    status: crate::assistant::types::RunStatus::Running,
                    trigger: crate::assistant::types::RunTrigger::UserMessage,
                    connection_id: "conn-1".to_string(),
                    protocol_id: "openai".to_string(),
                    model_id: "m".to_string(),
                    error: None,
                },
            )
            .await
            .unwrap();
            let delivered = conversation.pending_ids();
            mark_queued_messages_delivered(&pool, &session.id, &run.id, &delivered)
                .await
                .unwrap();
            conversation.mark_delivered(&delivered);

            let deleted = add_queued(&pool, &session.id, "never sent").await;
            conversation.refresh_pending(pending(
                list_pending_queued_messages(&pool, &session.id)
                    .await
                    .unwrap(),
            ));
            assert_eq!(conversation.messages().len(), 3);
            delete_pending_queued_message(&pool, &session.id, &deleted.id)
                .await
                .unwrap();
            conversation.refresh_pending(pending(
                list_pending_queued_messages(&pool, &session.id)
                    .await
                    .unwrap(),
            ));

            assert_eq!(
                view_ids(&conversation),
                view_ids(&RunConversation::load(&pool, &session.id).await.unwrap())
            );
            assert_eq!(conversation.messages().len(), 2);
            assert!(conversation.pending_ids().is_empty());
        }

        /// Two automatic compactions: the second summarizes `[S1 + messages
        /// after S1's boundary]` only, S1 appears exactly once, nothing older
        /// than the boundary is re-read, and a restart reconstructs the same
        /// view from the database.
        #[tokio::test]
        async fn consecutive_compactions_chain_from_the_previous_summary_and_survive_restart() {
            let (_tmp, pool, session) = db_session().await;
            let mut raw = Vec::new();
            for i in 0..12 {
                raw.push(add_text(&pool, &session.id, &format!("{i}{}", " a".repeat(8_000))).await);
            }
            let mut conversation = RunConversation::load(&pool, &session.id).await.unwrap();
            assert!(should_auto_compact(&conversation, "", &[]));
            let calls = Arc::new(Mutex::new(Vec::new()));

            let first = compact_with(
                &pool,
                &session.id,
                "openai",
                "m",
                CompactionTrigger::Automatic,
                None,
                false,
                &mut conversation,
                recording_summarizer(&calls, "summary one"),
            )
            .await
            .unwrap()
            .expect("first compaction");

            // 20k tail budget keeps the newest two 8k messages verbatim.
            let raw_ids: Vec<String> = raw.iter().map(|m| m.id.clone()).collect();
            assert_eq!(calls.lock().unwrap()[0], raw_ids[..10].to_vec());
            assert_eq!(
                first.compaction.source_to_message_id.as_deref(),
                Some(raw_ids[9].as_str())
            );
            let mut expected = vec![first.summary_message.id.clone()];
            expected.extend(raw_ids[10..].iter().cloned());
            assert_eq!(view_ids(&conversation), expected);
            assert!(!should_auto_compact(&conversation, "", &[]));

            for i in 12..22 {
                let message =
                    add_text(&pool, &session.id, &format!("{i}{}", " a".repeat(8_000))).await;
                conversation.upsert(message.clone());
                raw.push(message);
            }
            assert!(should_auto_compact(&conversation, "", &[]));

            let second = compact_with(
                &pool,
                &session.id,
                "openai",
                "m",
                CompactionTrigger::Automatic,
                None,
                false,
                &mut conversation,
                recording_summarizer(&calls, "summary two"),
            )
            .await
            .unwrap()
            .expect("second compaction");

            let raw_ids: Vec<String> = raw.iter().map(|m| m.id.clone()).collect();
            let mut expected_input = vec![first.summary_message.id.clone()];
            expected_input.extend(raw_ids[10..20].iter().cloned());
            assert_eq!(calls.lock().unwrap()[1], expected_input);
            assert_eq!(
                second.compaction.source_to_message_id.as_deref(),
                Some(raw_ids[19].as_str())
            );
            let mut expected_view = vec![second.summary_message.id.clone()];
            expected_view.extend(raw_ids[20..].iter().cloned());
            assert_eq!(view_ids(&conversation), expected_view);

            let reloaded = RunConversation::load(&pool, &session.id).await.unwrap();
            assert_eq!(view_ids(&reloaded), expected_view);
            assert_eq!(
                reloaded.summary().map(|m| m.id.as_str()),
                Some(second.summary_message.id.as_str())
            );
        }

        /// Forced recovery (context-limit retry, `/compact`) uses the same
        /// in-memory conversation: everything but the newest group is
        /// summarized, and the retry sees the summary without a reload.
        #[tokio::test]
        async fn error_recovery_compacts_the_in_memory_conversation() {
            let (_tmp, pool, session) = db_session().await;
            let u1 = add_text(&pool, &session.id, "hello").await;
            let a1 = add(
                &pool,
                &session.id,
                MessageRole::Assistant,
                vec![ContentPart::ToolUse {
                    tool_call_id: "c1".to_string(),
                    tool_name: "probe".to_string(),
                    arguments: serde_json::Value::Null,
                }],
            )
            .await;
            let t1 = add(
                &pool,
                &session.id,
                MessageRole::Tool,
                vec![ContentPart::ToolResult {
                    tool_call_id: "c1".to_string(),
                    payload: serde_json::Value::Null,
                    started_at: None,
                    completed_at: None,
                }],
            )
            .await;
            let a2 = add(
                &pool,
                &session.id,
                MessageRole::Assistant,
                vec![text("partial")],
            )
            .await;
            let mut conversation = RunConversation::load(&pool, &session.id).await.unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));

            let outcome = compact_with(
                &pool,
                &session.id,
                "openai",
                "m",
                CompactionTrigger::ErrorRecovery,
                None,
                true,
                &mut conversation,
                recording_summarizer(&calls, "recovered"),
            )
            .await
            .unwrap()
            .expect("forced compaction");

            assert_eq!(
                calls.lock().unwrap()[0],
                vec![u1.id.clone(), a1.id.clone(), t1.id.clone()]
            );
            assert_eq!(
                view_ids(&conversation),
                vec![outcome.summary_message.id.clone(), a2.id.clone()]
            );
            assert_eq!(
                view_ids(&conversation),
                view_ids(&RunConversation::load(&pool, &session.id).await.unwrap())
            );
        }

        #[tokio::test]
        async fn a_non_shrinking_summary_is_rejected_and_not_retried_on_the_same_boundary() {
            let (_tmp, pool, session) = db_session().await;
            let tiny = add_text(&pool, &session.id, "hi").await;
            add_text(&pool, &session.id, &" a".repeat(25_000)).await;
            let mut conversation = RunConversation::load(&pool, &session.id).await.unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let bloated: &'static str = Box::leak("x ".repeat(4_000).into_boxed_str());

            let outcome = compact_with(
                &pool,
                &session.id,
                "openai",
                "m",
                CompactionTrigger::Automatic,
                None,
                false,
                &mut conversation,
                recording_summarizer(&calls, bloated),
            )
            .await
            .unwrap();
            assert!(outcome.is_none());
            assert_eq!(calls.lock().unwrap()[0], vec![tiny.id.clone()]);
            assert_eq!(conversation.messages().len(), 2, "history unchanged");

            let again = compact_with(
                &pool,
                &session.id,
                "openai",
                "m",
                CompactionTrigger::Automatic,
                None,
                false,
                &mut conversation,
                recording_summarizer(&calls, bloated),
            )
            .await
            .unwrap();
            assert!(again.is_none());
            assert_eq!(calls.lock().unwrap().len(), 1, "no second summarizer call");
            assert!(
                RunConversation::load(&pool, &session.id)
                    .await
                    .unwrap()
                    .summary()
                    .is_none(),
                "nothing persisted"
            );
        }

        /// A completed compaction whose rows live in another session must
        /// not splice foreign rows into the view.
        #[tokio::test]
        async fn load_falls_back_to_raw_history_for_a_degenerate_compaction_row() {
            let (_tmp, pool, session) = db_session().await;
            let m1 = add_text(&pool, &session.id, "one").await;
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
            let foreign = add_text(&pool, &other.id, "elsewhere").await;
            let compaction = create_compaction(
                &pool,
                crate::assistant::repository::CreateCompactionParams {
                    session_id: session.id.clone(),
                    trigger: CompactionTrigger::Automatic,
                    strategy: CompactionStrategy::LocalSummary,
                    source_from_message_id: None,
                    source_to_message_id: Some(foreign.id.clone()),
                    created_run_id: None,
                    protocol_id: "p".to_string(),
                    model_id: "m".to_string(),
                    input_message_count: 0,
                },
            )
            .await
            .unwrap();
            complete_compaction(&pool, &compaction.id, &foreign.id)
                .await
                .unwrap();

            let conversation = RunConversation::load(&pool, &session.id).await.unwrap();

            assert_eq!(view_ids(&conversation), vec![m1.id]);
            assert!(conversation.summary().is_none());
        }
    }
}

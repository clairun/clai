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
const AUTO_COMPACTION_MESSAGE_CHARS: usize = 120_000;
const SUMMARY_TRANSCRIPT_MAX_CHARS: usize = 96_000;
const SUMMARY_MAX_OUTPUT_TOKENS: u32 = 4096;
const SUMMARY_TOOL_CALL_MAX_CHARS: usize = 4_000;
const SUMMARY_TOOL_RESULT_MAX_CHARS: usize = 8_000;

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

pub async fn provider_history_messages(
    pool: &DbPool,
    session_id: &str,
    messages: &[AssistantMessage],
) -> Result<Vec<AssistantMessage>, String> {
    let latest = repository::latest_completed_compaction(pool, session_id).await?;
    Ok(provider_history_messages_with_compaction(
        messages,
        latest.as_ref(),
    ))
}

pub async fn latest_compaction_summary_text(
    pool: &DbPool,
    session_id: &str,
) -> Result<Option<String>, String> {
    let Some(compaction) = repository::latest_completed_compaction(pool, session_id).await? else {
        return Ok(None);
    };
    let Some(summary_message_id) = compaction.summary_message_id.as_deref() else {
        return Ok(None);
    };
    let Some(message) = repository::get_message(pool, summary_message_id).await? else {
        return Ok(None);
    };
    Ok(Some(content_text(&message.content)))
}

pub fn should_auto_compact(messages: &[AssistantMessage], tools: &[ToolDefinition]) -> bool {
    let non_compaction_messages = messages
        .iter()
        .filter(|message| !is_compaction_summary_message(message))
        .count();
    if non_compaction_messages < MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES {
        return false;
    }

    estimate_history_chars(messages, tools) >= AUTO_COMPACTION_MESSAGE_CHARS
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
    let messages = repository::list_messages(pool, &session.id).await?;
    let latest = repository::latest_completed_compaction(pool, &session.id).await?;
    let provider_view = provider_history_messages_with_compaction(&messages, latest.as_ref());
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
    let summary = neutralize_archived_tool_syntax(&summary);

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
        .cloned()
        .map(sanitize_compaction_summary_message_for_prompt);
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

fn sanitize_compaction_summary_message_for_prompt(
    mut message: AssistantMessage,
) -> AssistantMessage {
    if !is_compaction_summary_message(&message) {
        return message;
    }
    for part in &mut message.content {
        if let ContentPart::Text { text } = part {
            *text = neutralize_archived_tool_syntax(text);
        }
    }
    message
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
            | ProviderEvent::MessageComplete
            | ProviderEvent::Usage { .. } => {}
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

Preserve:
- user goals and constraints
- concrete decisions and assumptions
- files, commands, code changes, test results, errors, and unresolved tasks
- tool results that are still relevant
- any instructions that remain binding

When preserving past tool activity, describe it as already-completed history in prose. Do not copy literal invocation/result transcript syntax, XML-like invocation wrappers, JSON tool-call wrappers, or standalone result blocks into the summary.

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

fn summary_message_text(summary: &str) -> String {
    format!("{}\n\n{}", SUMMARY_MESSAGE_PREAMBLE, summary.trim())
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
            ContentPart::Text { text } => Some(neutralize_archived_tool_syntax(text)),
            ContentPart::Thinking { .. } => None,
            ContentPart::ToolUse {
                tool_name,
                arguments,
                ..
            } => Some(format!(
                "Archived tool request (already completed; do not copy as a current action): `{}` arguments {}",
                tool_name,
                truncate_json(arguments, tool_call_max)
            )),
            ContentPart::ToolResult { payload, .. } => Some(format!(
                "Archived tool result (already completed): {}",
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

/// Fresh CLI sessions and compaction summaries are rendered as prompt text, not
/// real provider tool messages. Neutralise tool-invocation-looking fragments so
/// the next model sees archival context instead of syntax to continue.
pub(crate) fn neutralize_archived_tool_syntax(text: &str) -> String {
    let looks_relevant = text.contains("[tool ")
        || text.contains("[tool_")
        || text.contains("<invoke")
        || text.contains("</invoke>")
        || text.contains("<parameter")
        || text.contains("</parameter>")
        || text.contains("{\"name\":\"")
        || text.contains("{\"name\": \"");
    if !looks_relevant {
        return text.to_string();
    }

    let looked_like_wrapper = text.contains("<invoke")
        || text.contains("</invoke>")
        || text.contains("{\"name\":\"")
        || text.contains("{\"name\": \"");
    let mut out = text.to_string();
    for (from, to) in [
        ("[tool call:", "[archived tool request:"),
        ("[tool_call:", "[archived tool request:"),
        ("[tool result:", "[archived tool result:"),
        ("[tool_result:", "[archived tool result:"),
        ("<invoke", "archived-invoke"),
        ("</invoke>", "/archived-invoke"),
        ("<parameter", "archived-parameter"),
        ("</parameter>", "/archived-parameter"),
        ("{\"name\":\"", "{archived_tool_name:\""),
        ("{\"name\": \"", "{archived_tool_name: \""),
    ] {
        out = out.replace(from, to);
    }
    if looked_like_wrapper {
        for (from, to) in [
            ("\nResult:", "\nArchived result:"),
            ("\r\nResult:", "\r\nArchived result:"),
            ("<br>\nResult:", "<br>\nArchived result:"),
            ("<br>Result:", "<br>Archived result:"),
        ] {
            out = out.replace(from, to);
        }
    }
    out
}

fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn estimate_history_chars(messages: &[AssistantMessage], tools: &[ToolDefinition]) -> usize {
    let message_chars: usize = messages
        .iter()
        .filter(|message| !is_compaction_summary_message(message))
        .map(|message| {
            render_content_parts(
                &message.content,
                SUMMARY_TOOL_CALL_MAX_CHARS,
                SUMMARY_TOOL_RESULT_MAX_CHARS,
            )
            .len()
                + 16
        })
        .sum();
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
    message_chars + tool_chars
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
    use crate::assistant::types::{CompactionStatus, ContentPart, MessageRole};

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
        view[0].content = vec![text(&"x".repeat(AUTO_COMPACTION_MESSAGE_CHARS))];

        assert!(!should_auto_compact(&view, &[]));
    }

    #[test]
    fn automatic_compaction_requires_the_estimated_size_threshold() {
        let mut view = filler("f", MIN_AUTOMATIC_COMPACT_MESSAGES + RECENT_TAIL_MESSAGES);

        assert!(!should_auto_compact(&view, &[]));

        view[0].content = vec![text(&"x".repeat(AUTO_COMPACTION_MESSAGE_CHARS))];

        assert!(should_auto_compact(&view, &[]));
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

    #[test]
    fn transcript_for_summary_renders_tool_history_as_archival_context() {
        let messages = vec![
            msg(
                "bad-text",
                MessageRole::Assistant,
                vec![text(
                    r#"<invoke name="bash_exec"></invoke>
Result:
{"name":"bash_exec","input":{"command":"date"}}"#,
                )],
            ),
            msg(
                "call",
                MessageRole::Assistant,
                vec![ContentPart::ToolUse {
                    tool_call_id: "call-1".to_string(),
                    tool_name: "bash_exec".to_string(),
                    arguments: serde_json::json!({ "command": "date" }),
                }],
            ),
            msg(
                "result",
                MessageRole::Tool,
                vec![ContentPart::ToolResult {
                    tool_call_id: "call-1".to_string(),
                    payload: serde_json::json!({ "stdout": "today" }),
                    started_at: None,
                    completed_at: None,
                }],
            ),
        ];

        let transcript = transcript_for_summary(&messages);

        for forbidden in [
            "[tool call:",
            "[tool result:",
            "<invoke",
            "</invoke>",
            "{\"name\":\"bash_exec\"",
            "\nResult:",
        ] {
            assert!(
                !transcript.contains(forbidden),
                "summary transcript preserved {forbidden}: {transcript}"
            );
        }
        assert!(transcript.contains("Archived tool request"));
        assert!(transcript.contains("Archived tool result"));
        assert!(transcript.contains("Archived result:"));
        assert!(transcript.contains("bash_exec"));
        assert!(transcript.contains("date"));
    }

    #[test]
    fn provider_history_sanitizes_existing_compaction_summary_for_prompt() {
        let mut summary = msg(
            "summary",
            MessageRole::System,
            vec![text(
                r#"[tool call: bash_exec {"command":"date"}]
<invoke name="bash_exec"></invoke>
Result:
{"name":"bash_exec","input":{"command":"date"}}"#,
            )],
        );
        summary.provider_metadata =
            Some(serde_json::json!({ "source": COMPACTION_METADATA_SOURCE }));
        let messages = vec![
            msg("old", MessageRole::User, vec![text("old")]),
            summary,
            msg("new", MessageRole::User, vec![text("new")]),
        ];
        let latest = AssistantCompaction {
            id: "c".to_string(),
            session_id: "s".to_string(),
            trigger: CompactionTrigger::Manual,
            strategy: CompactionStrategy::SessionRotationSummary,
            status: CompactionStatus::Completed,
            source_from_message_id: Some("old".to_string()),
            source_to_message_id: Some("old".to_string()),
            summary_message_id: Some("summary".to_string()),
            created_run_id: None,
            protocol_id: "p".to_string(),
            model_id: "m".to_string(),
            input_message_count: 1,
            created_at: 0,
            completed_at: Some(0),
            error: None,
        };

        let provider = provider_history_messages_with_compaction(&messages, Some(&latest));
        let rendered = content_text(&provider[0].content);

        assert_eq!(provider.len(), 2);
        assert!(rendered.contains("archived tool request"));
        assert!(rendered.contains("Archived result:"));
        for forbidden in [
            "[tool call:",
            "<invoke",
            "{\"name\":\"bash_exec\"",
            "\nResult:",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "provider summary preserved {forbidden}: {rendered}"
            );
        }
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

    #[test]
    fn summary_message_text_includes_recovery_guidance() {
        let out = summary_message_text("the summary body");
        assert!(out.contains("the summary body"));
        assert!(out.contains(".clai/memory/"));
        assert!(out.contains(".clai/data.sqlite"));
        assert!(out.contains("history_query"));
    }
}

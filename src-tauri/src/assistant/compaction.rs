use futures::StreamExt;

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
// Per-tool-payload caps when rendering a transcript. The deterministic digest
// (the seed for a rotated CLI session) keeps tool payloads tiny: a digest only
// needs to know *which* tool ran and roughly what it returned. Keeping full
// tool JSON crowds the actual conversation (goals, decisions) out of the char
// budget and was the main reason past digests were unusable seeds. The
// model-summary path keeps richer caps since the model reads the transcript
// once and distills it.
const DIGEST_TOOL_CALL_MAX_CHARS: usize = 200;
const DIGEST_TOOL_RESULT_MAX_CHARS: usize = 400;
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

    let strategy = if providers::is_cli_provider(&connection.provider_id) {
        CompactionStrategy::SessionRotationSummary
    } else {
        CompactionStrategy::LocalSummary
    };

    let compaction = repository::create_compaction(
        pool,
        CreateCompactionParams {
            session_id: session.id.clone(),
            trigger: trigger.clone(),
            strategy: strategy.clone(),
            source_from_message_id: window.source_from_message_id.clone(),
            source_to_message_id: window.source_to_message_id.clone(),
            created_run_id: run_id.map(str::to_string),
            provider_id: connection.provider_id.clone(),
            model_id: connection.model_id.clone(),
            input_message_count: window.messages.len() as i64,
        },
    )
    .await?;

    let summary_result = summarize_window(
        session,
        connection,
        &compaction.id,
        &strategy,
        &window.messages,
    )
    .await;

    let summary = match summary_result {
        Ok(summary) => summary,
        Err(error) => {
            tracing::warn!(
                session_id = %session.id,
                compaction_id = %compaction.id,
                error = %error,
                "Model-generated compaction summary failed; using deterministic fallback"
            );
            fallback_summary(&window.messages)
        }
    };

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

    let messages = compactable[..compact_count].to_vec();
    let source_from_message_id = messages.first().map(|message| message.id.clone());
    let source_to_message_id = messages.last().map(|message| message.id.clone());

    Some(CompactionWindow {
        messages,
        source_from_message_id,
        source_to_message_id,
    })
}

async fn summarize_window(
    session: &AssistantSession,
    connection: &ProviderConnection,
    compaction_id: &str,
    strategy: &CompactionStrategy,
    messages: &[AssistantMessage],
) -> Result<String, String> {
    if matches!(strategy, CompactionStrategy::SessionRotationSummary) {
        return Ok(fallback_summary(messages));
    }

    let adapter = providers::resolve_adapter(&connection.provider_id).map_err(|e| e.to_string())?;
    let transcript = transcript_for_summary(messages);
    let request = CompletionRequest {
        run_id: format!("compaction-{}", compaction_id),
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
    };

    let mut stream = adapter
        .stream_completion(connection, request)
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

const SUMMARY_SYSTEM_PROMPT: &str = r#"Summarize the previous conversation so another assistant can continue it with minimal context.

Preserve:
- user goals and constraints
- concrete decisions and assumptions
- files, commands, code changes, test results, errors, and unresolved tasks
- tool results that are still relevant
- any instructions that remain binding

Do not include filler, greetings, or obsolete intermediate details. Do not invent facts. Write a compact but complete continuation summary."#;

fn summary_message_text(summary: &str) -> String {
    format!(
        "Conversation summary generated by CLAI compaction. Treat this as the \
         authoritative summary of the compacted earlier messages. If you are \
         missing context needed to continue, recover it before acting rather \
         than asking the user to repeat anything: your durable state is in \
         `.clai/memory/` and the full verbatim history (every message and tool \
         result) is in `.clai/data.sqlite` — query it with the read-only \
         `history_query` tool (no approval needed) to recover specifics.\n\n{}",
        summary.trim()
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

fn fallback_summary(messages: &[AssistantMessage]) -> String {
    let rendered = render_messages(
        messages,
        DIGEST_TOOL_CALL_MAX_CHARS,
        DIGEST_TOOL_RESULT_MAX_CHARS,
    );
    let body = select_head_and_tail(&rendered, SUMMARY_TRANSCRIPT_MAX_CHARS);
    format!(
        "Deterministic compaction summary: the previous conversation was \
         compacted without a model-generated summary. Below is a noise-reduced \
         transcript digest — tool payloads are truncated, and when the history \
         is too long the opening exchanges (the original goal) and the most \
         recent exchanges are kept while the middle is dropped. Continue from \
         it, and recover anything that was elided from `.clai/memory/` or by \
         querying the full history with the read-only `history_query` tool \
         (it reads `.clai/data.sqlite` and needs no approval).\n\n{}",
        body
    )
}

/// Join rendered messages within `budget`, always at whole-message boundaries.
/// When they don't all fit, keep a head slice (so the opening user goal — or,
/// in a chained compaction, the prior summary — survives) and a tail slice (so
/// the most recent exchanges survive), dropping the middle. Never cuts inside a
/// message, so the seed never opens mid-sentence in a stale tool payload.
fn select_head_and_tail(rendered: &[String], budget: usize) -> String {
    const SEP_LEN: usize = 2; // "\n\n"
    let total: usize = rendered.iter().map(|s| s.len() + SEP_LEN).sum();
    if total <= budget {
        return rendered.join("\n\n");
    }

    let head_budget = budget / 3;
    let tail_budget = budget - head_budget;

    // Head: take whole messages from the front until the head budget is hit
    // (always at least one, so the original goal is never fully dropped).
    let mut head_end = 0;
    let mut acc = 0;
    while head_end < rendered.len() {
        let next = acc + rendered[head_end].len() + SEP_LEN;
        if next > head_budget && head_end > 0 {
            break;
        }
        acc = next;
        head_end += 1;
    }

    // Tail: take whole messages from the back until the tail budget is hit,
    // never crossing into the head region.
    let mut tail_start = rendered.len();
    acc = 0;
    while tail_start > head_end {
        let candidate = tail_start - 1;
        let next = acc + rendered[candidate].len() + SEP_LEN;
        if next > tail_budget && tail_start < rendered.len() {
            break;
        }
        acc = next;
        tail_start -= 1;
    }

    // Head and tail meet (everything fits across the two slices): no omission.
    if tail_start <= head_end {
        return rendered.join("\n\n");
    }

    let omitted = tail_start - head_end;
    let suffix = if omitted == 1 { "" } else { "s" };
    format!(
        "{}\n\n[... {} middle message{} omitted during compaction; recover the \
         full verbatim history with the read-only `history_query` tool \
         (`.clai/data.sqlite`) ...]\n\n{}",
        rendered[..head_end].join("\n\n"),
        omitted,
        suffix,
        rendered[tail_start..].join("\n\n")
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
            format!(
                "[{} message {}]\n{}",
                role,
                message.id,
                render_content_parts(&message.content, tool_call_max, tool_result_max)
            )
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
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn select_head_and_tail_keeps_everything_within_budget() {
        let rendered = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let out = select_head_and_tail(&rendered, 10_000);
        assert_eq!(out, "a\n\nb\n\nc");
        assert!(!out.contains("omitted during compaction"));
    }

    #[test]
    fn select_head_and_tail_preserves_head_and_tail_drops_middle() {
        // Each entry ~30 chars; a small budget forces an omission.
        let rendered: Vec<String> = (0..20)
            .map(|i| format!("MESSAGE-{i:02}-xxxxxxxxxxxxxxxxx"))
            .collect();
        let out = select_head_and_tail(&rendered, 120);
        // Opening goal (first message) survives.
        assert!(out.contains("MESSAGE-00"), "head dropped: {out}");
        // Most recent message survives.
        assert!(out.contains("MESSAGE-19"), "tail dropped: {out}");
        // The middle is dropped with a pointer to the verbatim history.
        assert!(out.contains("omitted during compaction"), "{out}");
        assert!(out.contains(".clai/data.sqlite"), "{out}");
    }

    #[test]
    fn fallback_summary_truncates_tool_payloads() {
        let huge = "X".repeat(50_000);
        let messages = vec![
            msg(
                "m1",
                MessageRole::User,
                vec![text("Original goal: fix the routing bug")],
            ),
            msg(
                "m2",
                MessageRole::Tool,
                vec![ContentPart::ToolResult {
                    tool_call_id: "t1".to_string(),
                    payload: serde_json::json!({ "stdout": huge }),
                    started_at: None,
                    completed_at: None,
                }],
            ),
        ];
        let out = fallback_summary(&messages);
        // The opening goal is kept verbatim...
        assert!(out.contains("Original goal: fix the routing bug"), "{out}");
        // ...but the 50k tool payload is truncated far below its original size.
        assert!(out.contains("[truncated]"), "tool payload not truncated");
        assert!(
            out.len() < 5_000,
            "digest unexpectedly large: {} chars",
            out.len()
        );
    }

    #[test]
    fn fallback_summary_points_at_recovery_sources() {
        let messages = vec![msg("m1", MessageRole::User, vec![text("hello")])];
        let out = fallback_summary(&messages);
        assert!(out.contains(".clai/memory/"));
        assert!(out.contains(".clai/data.sqlite"));
        assert!(out.contains("history_query"));
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

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

const STDERR_TAIL_LINES: usize = 20;

use serde_json::Value;
use tauri::Manager;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::assistant::engine::{
    build_system_prompt, build_trigger_message, AssistantDeps, AssistantEngineError, RunTurnInput,
};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::local_mcp::{self, ToolBinding};
use crate::assistant::providers::cli::CLAUDE_CODE_PROVIDER_ID;
use crate::assistant::repository::{
    self, CreateMessageParams, CreateRunParams, CreateToolCallParams,
};
use crate::assistant::types::{
    AssistantMessage, AssistantSession, ContentPart, MessageRole, ProviderConnection,
    ProviderInputMessage, RunNotice, RunStatus, RunUsage, ToolCallStatus,
};
use crate::AppState;

const CLAUDE_DISABLED_TOOLS: &str = "Bash,Read,Edit,Write,Glob,Grep,WebFetch,WebSearch,Task,TodoWrite,NotebookEdit,NotebookRead,LSP";

pub async fn run_session_turn(
    deps: &AssistantDeps,
    input: RunTurnInput,
) -> Result<(), AssistantEngineError> {
    let mut session = repository::get_session(&deps.pool, &input.session_id)
        .await?
        .ok_or_else(|| AssistantEngineError::SessionNotFound(input.session_id.clone()))?;

    let connection = deps
        .app
        .try_state::<AppState>()
        .and_then(|state| {
            state
                .config_manager
                .lock()
                .ok()?
                .get_provider_connection(&input.connection_id)
        })
        .ok_or_else(|| AssistantEngineError::ProviderNotConfigured(input.connection_id.clone()))?;

    let run_id = resolve_run_id(deps, &session, &connection, &input).await?;

    let run = repository::update_run_status(&deps.pool, &run_id, RunStatus::Running, None).await?;
    let _ = emit_event(
        &deps.app,
        &session,
        Some(&run_id),
        AssistantUiEvent::RunStarted { run },
    );

    if input.cancel_token.is_cancelled() {
        cancel_run(deps, &session, &run_id, None).await?;
        return Ok(());
    }

    if connection.provider_id != CLAUDE_CODE_PROVIDER_ID {
        let message = format!(
            "CLI provider '{}' is registered but not implemented yet",
            connection.provider_id
        );
        fail_run(deps, &session, &run_id, None, &message).await?;
        return Err(AssistantEngineError::Provider(
            crate::assistant::providers::types::ProviderError::RequestFailed(message),
        ));
    }

    let (cli_session_id, is_new_session) = ensure_cli_session_id(deps, &mut session).await?;
    let mcp_runtime = local_mcp::ensure_started(deps).await?;
    let notices = Arc::new(Mutex::new(Vec::<RunNotice>::new()));
    let session_grants = Arc::new(Mutex::new(Vec::new()));
    let token = mcp_runtime
        .bind_run(ToolBinding {
            session_id: session.id.clone(),
            run_id: run_id.clone(),
            cancel_token: input.cancel_token.clone(),
            inter_agent_call_depth: input.inter_agent_call_depth,
            notices: notices.clone(),
            session_grants,
        })
        .await;
    let mcp_config_path = match write_mcp_config(mcp_runtime.url(), &token) {
        Ok(path) => path,
        Err(error) => {
            mcp_runtime.unbind_token(&token).await;
            let message = error.message();
            fail_run(deps, &session, &run_id, None, &message).await?;
            return Err(AssistantEngineError::Provider(
                crate::assistant::providers::types::ProviderError::RequestFailed(message),
            ));
        }
    };

    let run_result = run_claude_turn(
        deps,
        &session,
        &connection,
        &run_id,
        &cli_session_id,
        is_new_session,
        &mcp_config_path,
        &input.cancel_token,
        &input.trigger,
    )
    .await;

    mcp_runtime.unbind_token(&token).await;
    let _ = std::fs::remove_file(&mcp_config_path);

    match run_result {
        Ok(usage) => {
            let notices = notices
                .lock()
                .map(|mut n| std::mem::take(&mut *n))
                .unwrap_or_default();
            let final_status = if notices.is_empty() {
                RunStatus::Completed
            } else {
                RunStatus::CompletedWithWarnings
            };
            let run = repository::complete_run(
                &deps.pool,
                &run_id,
                final_status,
                usage.as_ref(),
                None,
                &notices,
            )
            .await?;
            let _ = emit_event(
                &deps.app,
                &session,
                Some(&run_id),
                AssistantUiEvent::RunCompleted { run },
            );
            Ok(())
        }
        Err(LocalAgentRunError::Cancelled { usage }) => {
            cancel_run(deps, &session, &run_id, usage.as_ref()).await
        }
        Err(LocalAgentRunError::Failed { message, usage }) => {
            let resolved_message = if !is_new_session && is_session_lost_error(&message) {
                clear_cli_session_id(deps, &mut session).await?;
                "The Claude session was lost (likely because a previous turn failed before Claude \
                 stored it). Send your message again and a fresh session will be started."
                    .to_string()
            } else {
                message
            };
            fail_run(deps, &session, &run_id, usage.as_ref(), &resolved_message).await?;
            Err(AssistantEngineError::Provider(
                crate::assistant::providers::types::ProviderError::RequestFailed(resolved_message),
            ))
        }
    }
}

fn is_session_lost_error(message: &str) -> bool {
    message.contains("No conversation found with session ID")
}

async fn clear_cli_session_id(
    deps: &AssistantDeps,
    session: &mut AssistantSession,
) -> Result<(), AssistantEngineError> {
    session.context.cli_session_id = None;
    session.updated_at = chrono::Utc::now().timestamp_millis();
    *session = repository::update_session(&deps.pool, session).await?;
    Ok(())
}

async fn resolve_run_id(
    deps: &AssistantDeps,
    session: &AssistantSession,
    connection: &ProviderConnection,
    input: &RunTurnInput,
) -> Result<String, AssistantEngineError> {
    match &input.run_id {
        Some(id) => {
            let existing_run = repository::get_run(&deps.pool, id).await?.ok_or_else(|| {
                AssistantEngineError::Persistence(format!("run not found: {}", id))
            })?;
            if existing_run.connection_id != input.connection_id {
                return Err(AssistantEngineError::RunConnectionMismatch(id.clone()));
            }
            Ok(id.clone())
        }
        None => {
            let run = repository::create_run(
                &deps.pool,
                CreateRunParams {
                    session_id: session.id.clone(),
                    status: RunStatus::Queued,
                    trigger: input.trigger.clone(),
                    connection_id: connection.id.clone(),
                    provider_id: connection.provider_id.clone(),
                    model_id: connection.model_id.clone(),
                    usage: None,
                    error: None,
                },
            )
            .await?;
            Ok(run.id)
        }
    }
}

async fn ensure_cli_session_id(
    deps: &AssistantDeps,
    session: &mut AssistantSession,
) -> Result<(String, bool), AssistantEngineError> {
    if let Some(id) = &session.context.cli_session_id {
        return Ok((id.clone(), false));
    }

    let id = Uuid::new_v4().to_string();
    session.context.cli_session_id = Some(id.clone());
    session.updated_at = chrono::Utc::now().timestamp_millis();
    *session = repository::update_session(&deps.pool, session).await?;
    Ok((id, true))
}

#[allow(clippy::too_many_arguments)]
async fn run_claude_turn(
    deps: &AssistantDeps,
    session: &AssistantSession,
    connection: &ProviderConnection,
    run_id: &str,
    cli_session_id: &str,
    is_new_session: bool,
    mcp_config_path: &PathBuf,
    cancel_token: &CancellationToken,
    trigger: &crate::assistant::types::RunTrigger,
) -> Result<Option<RunUsage>, LocalAgentRunError> {
    let prompt = prepare_prompt(deps, session, run_id, trigger).await?;
    let system_prompt = system_prompt_text(session, trigger).await;
    let assistant_message = repository::create_message(
        &deps.pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: String::new(),
            }],
            provider_metadata: Some(serde_json::json!({
                "source": "claude-code",
            })),
        },
    )
    .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::MessageCreated {
            message: assistant_message.clone(),
        },
    );

    let binary = connection
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("claude");
    let mut command = Command::new(binary);
    command
        .arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--include-partial-messages")
        .arg("--verbose");
    if is_new_session {
        command.arg("--session-id").arg(cli_session_id);
    } else {
        command.arg("--resume").arg(cli_session_id);
    }
    command
        .arg("--mcp-config")
        .arg(mcp_config_path)
        .arg("--strict-mcp-config")
        .arg("--tools")
        .arg("")
        .arg("--disallowedTools")
        .arg(CLAUDE_DISABLED_TOOLS)
        .arg("--permission-mode")
        .arg("bypassPermissions")
        .arg("--disable-slash-commands")
        .arg("--system-prompt")
        .arg(system_prompt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !connection.model_id.trim().is_empty() {
        command.arg("--model").arg(connection.model_id.trim());
    }

    // Pin the subprocess cwd to the agent's workspace root. Without
    // this, Claude Code inherits CLAI's launch cwd — which on a dev
    // machine is the CLAI repo itself, causing Claude Code's
    // auto-memory loader (`~/.claude/projects/<hash-of-cwd>/memory/`)
    // to pull in memory from unrelated projects. Pinning cwd keys
    // memory to the workspace, giving each agent a clean per-workspace
    // context. Skipped when the session has no workspace_id (transient
    // sessions) or the workspace is no longer in the index.
    if let Some(workspace_id) = session.context.workspace_id.as_deref() {
        if let Some(root) = deps
            .app
            .state::<crate::AppState>()
            .workspace_root(workspace_id)
        {
            command.current_dir(&root);
        }
    }

    let mut child = command.spawn().map_err(|e| {
        LocalAgentRunError::failed(format!(
            "Failed to launch `{}`: {}. Is Claude Code installed and on PATH?",
            binary, e
        ))
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| LocalAgentRunError::failed(format!("Failed to write prompt: {}", e)))?;
        drop(stdin);
    }
    let stderr_tail: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    if let Some(stderr) = child.stderr.take() {
        spawn_stderr_logger(run_id.to_string(), stderr, stderr_tail.clone());
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| LocalAgentRunError::failed("Claude stdout was not captured"))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut state = ClaudeStreamState::new();
    let mut usage: Option<RunUsage> = None;
    let mut result_error: Option<String> = None;

    loop {
        let line = tokio::select! {
            _ = cancel_token.cancelled() => {
                let _ = child.kill().await;
                finalize_assistant_message(deps, session, run_id, &assistant_message, &state)
                    .await?;
                return Err(LocalAgentRunError::Cancelled { usage });
            }
            next = lines.next_line() => next
        }
        .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;

        let Some(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(|e| {
            LocalAgentRunError::failed(format!("Invalid Claude stream-json event: {}", e))
        })?;
        handle_claude_event(
            deps,
            session,
            run_id,
            &assistant_message,
            &value,
            &mut state,
            &mut usage,
            &mut result_error,
        )
        .await?;
    }

    let status = child
        .wait()
        .await
        .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;
    finalize_assistant_message(deps, session, run_id, &assistant_message, &state).await?;

    if let Some(message) = result_error {
        let enriched = append_stderr_tail(&message, &stderr_tail);
        return Err(LocalAgentRunError::Failed {
            message: enriched,
            usage,
        });
    }
    if !status.success() {
        let base = format!("Claude Code exited with status {}", status);
        return Err(LocalAgentRunError::Failed {
            message: append_stderr_tail(&base, &stderr_tail),
            usage,
        });
    }
    Ok(usage)
}

fn append_stderr_tail(message: &str, tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    let Ok(buffer) = tail.lock() else {
        return message.to_string();
    };
    if buffer.is_empty() {
        return message.to_string();
    }
    let snippet: Vec<String> = buffer.iter().cloned().collect();
    format!("{}\n--- stderr ---\n{}", message, snippet.join("\n"))
}

async fn prepare_prompt(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    trigger: &crate::assistant::types::RunTrigger,
) -> Result<String, LocalAgentRunError> {
    if let Some(trigger_content) = build_trigger_message(session, trigger) {
        let boundary_msg = repository::create_message(
            &deps.pool,
            CreateMessageParams {
                session_id: session.id.clone(),
                role: trigger_content.role.clone(),
                content: trigger_content.content.clone(),
                provider_metadata: Some(serde_json::json!({
                    "source": "claude-code-trigger",
                })),
            },
        )
        .await?;
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::MessageCreated {
                message: boundary_msg,
            },
        );
        return Ok(provider_message_text(&trigger_content));
    }

    let messages = repository::list_messages(&deps.pool, &session.id).await?;
    messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(message_text)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| LocalAgentRunError::failed("No user message found for Claude Code run"))
}

async fn system_prompt_text(
    session: &AssistantSession,
    trigger: &crate::assistant::types::RunTrigger,
) -> String {
    let tool_defs = crate::assistant::tools::available_tools(&session.context, &[]);
    provider_message_text(&build_system_prompt(&session.context, &tool_defs, trigger))
}

/// In-flight state of a Claude Code stream.
///
/// The stream interleaves text, thinking, and tool_use content blocks
/// (each indexed by `content_block` index). To preserve order in the
/// persisted assistant message we keep a single ordered `Vec<ContentPart>`
/// and remember which content_block index maps to which open part.
///
/// `persisted_tool_use_ids` lets us safely consume tool_use blocks from
/// either the streamed (`stream_event` deltas) or the complete
/// (`"type":"assistant"`) message envelope without double-persisting.
/// `pending_tool_results` buffers `tool_result` blocks that arrive
/// before their `tool_use` (rare, but possible when Claude Code emits
/// tool_use only in the complete assistant message): we replay the
/// buffer as soon as the matching tool_use is registered.
/// `last_update_emit_at` throttles `AssistantMessageUpdated` emissions
/// — without it a tool-heavy turn fires one full message-replacement
/// event per tool_use and React re-renders the entire chat tree each
/// time, wedging WebKit on long runs.
struct ClaudeStreamState {
    parts: Vec<ContentPart>,
    open_blocks: HashMap<u64, OpenBlock>,
    persisted_tool_use_ids: std::collections::HashSet<String>,
    pending_tool_results: HashMap<String, Value>,
    last_update_emit_at: Option<std::time::Instant>,
}

/// Minimum gap between consecutive `AssistantMessageUpdated` emissions.
/// The DB write still happens on every flush (so the persisted state is
/// always up to date), but the frontend gets coalesced updates at most
/// ~5/sec. The turn-final `AssistantMessageCompleted` always fires
/// regardless, so the user sees the final state immediately on
/// completion.
const ASSISTANT_UPDATE_EMIT_THROTTLE_MS: u128 = 200;

enum OpenBlock {
    /// Text block currently being streamed. Deltas append to
    /// `parts[parts_index]`.
    Text { parts_index: usize },
    /// Thinking block currently being streamed. Deltas append to
    /// `parts[parts_index]`.
    Thinking { parts_index: usize },
    /// Tool-use block being streamed. JSON input chunks accumulate in
    /// `accumulated_json` until `content_block_stop`, at which point we
    /// parse it, persist the tool_call record with the Claude-supplied
    /// `tool_call_id`, and push a `ContentPart::ToolUse` onto `parts`.
    ToolUse {
        tool_call_id: String,
        tool_name: String,
        accumulated_json: String,
    },
}

impl ClaudeStreamState {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            open_blocks: HashMap::new(),
            persisted_tool_use_ids: std::collections::HashSet::new(),
            pending_tool_results: HashMap::new(),
            last_update_emit_at: None,
        }
    }

    /// True iff the message accumulated any prose (non-empty Text part).
    /// Used by the `result` fallback to decide whether to inject the
    /// `result` summary text.
    fn has_text(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, ContentPart::Text { text } if !text.is_empty()))
    }

    fn last_part_is_text(&self) -> bool {
        matches!(self.parts.last(), Some(ContentPart::Text { .. }))
    }

    fn last_part_text_ends_with(&self, suffix: &str) -> bool {
        match self.parts.last() {
            Some(ContentPart::Text { text }) => text.ends_with(suffix),
            _ => false,
        }
    }

    fn append_to_last_text(&mut self, extra: &str) {
        if let Some(ContentPart::Text { text }) = self.parts.last_mut() {
            text.push_str(extra);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_claude_event(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    value: &Value,
    state: &mut ClaudeStreamState,
    usage: &mut Option<RunUsage>,
    result_error: &mut Option<String>,
) -> Result<(), LocalAgentRunError> {
    match value.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            let event = value.get("event").unwrap_or(&Value::Null);
            handle_stream_event(
                deps,
                session,
                run_id,
                assistant_message,
                event,
                state,
                usage,
            )
            .await?;
        }
        Some("assistant") => {
            // Complete (non-partial) assistant message. We treat this
            // as the authoritative source of tool_use blocks — Claude
            // Code's `--include-partial-messages` is documented to
            // stream partial *text* deltas, but tool_use blocks may
            // only appear in this complete envelope. Adopting tool_use
            // blocks here, idempotent against the stream-event path
            // (skip ids we already saw), guarantees we never miss them.
            let message = value.get("message").unwrap_or(&Value::Null);
            adopt_complete_assistant_message(
                deps,
                session,
                run_id,
                assistant_message,
                message,
                state,
            )
            .await?;
        }
        Some("user") => {
            // Claude Code reports tool execution outputs by emitting the
            // matching tool_result blocks inside a synthesized `user`
            // message in the stream. We close out the corresponding
            // tool_call records here.
            let message = value.get("message").unwrap_or(&Value::Null);
            if let Some(content) = message.get("content").and_then(Value::as_array) {
                for block in content {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    handle_tool_result(deps, session, run_id, state, block).await?;
                }
            }
        }
        Some("result") => {
            if let Some(parsed) = usage_from_value(value.get("usage")) {
                *usage = Some(parsed);
            }
            if value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let errors_first = value
                    .get("errors")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(Value::as_str);
                *result_error = Some(
                    errors_first
                        .or_else(|| value.get("result").and_then(Value::as_str))
                        .or_else(|| value.get("error").and_then(Value::as_str))
                        .unwrap_or("Claude Code run failed")
                        .to_string(),
                );
            } else if !state.has_text() {
                if let Some(text) = value.get("result").and_then(Value::as_str) {
                    // Open a fresh text part for the trailing summary so
                    // it doesn't get appended to a stale tool_use part.
                    state.parts.push(ContentPart::Text {
                        text: text.to_string(),
                    });
                    let _ = emit_event(
                        &deps.app,
                        session,
                        Some(run_id),
                        AssistantUiEvent::AssistantDelta {
                            message_id: assistant_message.id.clone(),
                            text: text.to_string(),
                        },
                    );
                }
            }
        }
        Some("error") => {
            *result_error = Some(
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Claude Code stream error")
                    .to_string(),
            );
        }
        _ => {}
    }

    Ok(())
}

async fn handle_stream_event(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    event: &Value,
    state: &mut ClaudeStreamState,
    usage: &mut Option<RunUsage>,
) -> Result<(), LocalAgentRunError> {
    let event_type = event.get("type").and_then(Value::as_str);
    let block_index = event.get("index").and_then(Value::as_u64).unwrap_or(0);

    match event_type {
        Some("content_block_start") => {
            let block = event.get("content_block").unwrap_or(&Value::Null);
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    // Consecutive text blocks merge into one Text part
                    // with a "\n\n" paragraph separator so the
                    // downstream UI's `getTextContent` (which joins text
                    // parts with no separator) still renders breaks
                    // between Claude's individual text emissions.
                    if state.last_part_is_text() {
                        let separator = if state.last_part_text_ends_with("\n\n") {
                            ""
                        } else if state.last_part_text_ends_with("\n") {
                            "\n"
                        } else {
                            "\n\n"
                        };
                        if !separator.is_empty() {
                            state.append_to_last_text(separator);
                            let _ = emit_event(
                                &deps.app,
                                session,
                                Some(run_id),
                                AssistantUiEvent::AssistantDelta {
                                    message_id: assistant_message.id.clone(),
                                    text: separator.to_string(),
                                },
                            );
                        }
                        let parts_index = state.parts.len() - 1;
                        state
                            .open_blocks
                            .insert(block_index, OpenBlock::Text { parts_index });
                    } else {
                        state.parts.push(ContentPart::Text {
                            text: String::new(),
                        });
                        let parts_index = state.parts.len() - 1;
                        state
                            .open_blocks
                            .insert(block_index, OpenBlock::Text { parts_index });
                    }
                }
                Some("thinking") => {
                    state.parts.push(ContentPart::Thinking {
                        text: String::new(),
                    });
                    let parts_index = state.parts.len() - 1;
                    state
                        .open_blocks
                        .insert(block_index, OpenBlock::Thinking { parts_index });
                }
                Some("tool_use") => {
                    let tool_call_id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    let tool_name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    state.open_blocks.insert(
                        block_index,
                        OpenBlock::ToolUse {
                            tool_call_id,
                            tool_name,
                            accumulated_json: String::new(),
                        },
                    );
                }
                _ => {}
            }
        }
        Some("content_block_delta") => {
            let delta = event.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        // Route to the Text part that content_block_start
                        // opened at this block_index. If we never saw a
                        // start (defensive — rare for Claude Code with
                        // partial messages enabled), fall back to the
                        // last Text part or create one.
                        let parts_index = match state.open_blocks.get(&block_index) {
                            Some(OpenBlock::Text { parts_index }) => Some(*parts_index),
                            _ => {
                                if state.last_part_is_text() {
                                    Some(state.parts.len() - 1)
                                } else {
                                    state.parts.push(ContentPart::Text {
                                        text: String::new(),
                                    });
                                    Some(state.parts.len() - 1)
                                }
                            }
                        };
                        if let Some(idx) = parts_index {
                            if let Some(ContentPart::Text { text: t }) = state.parts.get_mut(idx) {
                                t.push_str(text);
                            }
                        }
                        let _ = emit_event(
                            &deps.app,
                            session,
                            Some(run_id),
                            AssistantUiEvent::AssistantDelta {
                                message_id: assistant_message.id.clone(),
                                text: text.to_string(),
                            },
                        );
                    }
                }
                Some("thinking_delta") | Some("signature_delta") => {
                    if let Some(text) = delta
                        .get("thinking")
                        .or_else(|| delta.get("text"))
                        .and_then(Value::as_str)
                    {
                        let parts_index = match state.open_blocks.get(&block_index) {
                            Some(OpenBlock::Thinking { parts_index }) => Some(*parts_index),
                            _ => {
                                if matches!(state.parts.last(), Some(ContentPart::Thinking { .. }))
                                {
                                    Some(state.parts.len() - 1)
                                } else {
                                    state.parts.push(ContentPart::Thinking {
                                        text: String::new(),
                                    });
                                    Some(state.parts.len() - 1)
                                }
                            }
                        };
                        if let Some(idx) = parts_index {
                            if let Some(ContentPart::Thinking { text: t }) =
                                state.parts.get_mut(idx)
                            {
                                t.push_str(text);
                            }
                        }
                        let _ = emit_event(
                            &deps.app,
                            session,
                            Some(run_id),
                            AssistantUiEvent::AssistantThinkingDelta {
                                message_id: assistant_message.id.clone(),
                                text: text.to_string(),
                            },
                        );
                    }
                }
                Some("input_json_delta") => {
                    if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                        if let Some(OpenBlock::ToolUse {
                            accumulated_json, ..
                        }) = state.open_blocks.get_mut(&block_index)
                        {
                            accumulated_json.push_str(partial);
                        }
                    }
                }
                _ => {}
            }
        }
        Some("content_block_stop") => {
            if let Some(OpenBlock::ToolUse {
                tool_call_id,
                tool_name,
                accumulated_json,
            }) = state.open_blocks.remove(&block_index)
            {
                if !state.persisted_tool_use_ids.contains(&tool_call_id) {
                    let params: Value = if accumulated_json.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&accumulated_json)
                            .unwrap_or_else(|_| serde_json::json!({}))
                    };

                    persist_tool_use(
                        deps,
                        session,
                        run_id,
                        assistant_message,
                        state,
                        &tool_call_id,
                        &tool_name,
                        params,
                    )
                    .await?;
                }
            }
        }
        Some("message_start") | Some("message_delta") => {
            if let Some(parsed) = usage_from_value(
                event
                    .get("usage")
                    .or_else(|| event.get("message").and_then(|m| m.get("usage"))),
            ) {
                *usage = Some(parsed);
            }
        }
        _ => {}
    }

    Ok(())
}

/// Parse a single tool_result content block (from a synthesized "user"
/// stream message) and close out the matching tool_call record.
///
/// If we haven't yet seen the matching `tool_use` (which can happen
/// when Claude Code emits tool_use only in the trailing
/// `"type":"assistant"` complete-message envelope rather than as
/// streamed `content_block_start`/`content_block_stop` deltas), the
/// raw block is buffered and replayed once `persist_tool_use` registers
/// the id.
async fn handle_tool_result(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    state: &mut ClaudeStreamState,
    block: &Value,
) -> Result<(), LocalAgentRunError> {
    let tool_use_id = match block.get("tool_use_id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return Ok(()),
    };
    if !state.persisted_tool_use_ids.contains(&tool_use_id) {
        // No matching tool_use yet — buffer and replay later.
        state
            .pending_tool_results
            .insert(tool_use_id, block.clone());
        return Ok(());
    }
    apply_tool_result(deps, session, run_id, &tool_use_id, block).await
}

/// Persist a tool_use block (from either streamed or complete envelopes)
/// and emit its `ToolCallStarted` UI event. Idempotent: callers should
/// guard with `state.persisted_tool_use_ids.contains(id)` before calling,
/// but if the DB INSERT fails because the row already exists we just
/// surface the error to the caller (it's unexpected at that point).
///
/// Crucially, this also flushes the growing assistant message content
/// to the DB and emits `AssistantMessageUpdated` so the chat surfaces
/// tool calls *live*. Without this step, mid-turn tool_use parts
/// stay in memory only and the UI sees just an empty bubble until the
/// whole Claude Code turn finalizes — which can take a long time when
/// Claude makes many tool calls in a row.
#[allow(clippy::too_many_arguments)]
async fn persist_tool_use(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &mut ClaudeStreamState,
    tool_call_id: &str,
    tool_name: &str,
    params: Value,
) -> Result<(), LocalAgentRunError> {
    let invocation = repository::create_tool_call(
        &deps.pool,
        CreateToolCallParams {
            id: tool_call_id.to_string(),
            run_id: run_id.to_string(),
            session_id: session.id.clone(),
            tool_name: tool_name.to_string(),
            params: params.clone(),
            status: ToolCallStatus::Running,
        },
    )
    .await
    .map_err(LocalAgentRunError::failed)?;

    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::ToolCallStarted {
            tool_call: invocation,
        },
    );

    state.parts.push(ContentPart::ToolUse {
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        arguments: params,
    });
    state
        .persisted_tool_use_ids
        .insert(tool_call_id.to_string());

    // Flush the running parts vec into the assistant message so the
    // chat UI's tool_use renderer can pick it up immediately. The DB
    // write happens every call; the AssistantMessageUpdated event is
    // throttled inside flush_assistant_message_content to avoid an
    // event storm on tool-heavy turns.
    flush_assistant_message_content(deps, session, run_id, assistant_message, state).await?;

    // If a tool_result arrived before this tool_use was registered (e.g.
    // tool_use only present in the complete assistant message), flush it
    // now.
    if let Some(pending) = state.pending_tool_results.remove(tool_call_id) {
        apply_tool_result(deps, session, run_id, tool_call_id, &pending).await?;
    }

    Ok(())
}

/// Push the in-memory `state.parts` to the assistant message row and,
/// when not throttled, emit `AssistantMessageUpdated`.
///
/// The DB UPDATE runs on every call (cheap, single-row write) so the
/// persisted state is always current. The frontend event is coalesced
/// to at most one emission per `ASSISTANT_UPDATE_EMIT_THROTTLE_MS` — on
/// a tool-heavy turn (e.g. 35 sequential tool_uses) un-throttled
/// emissions would re-render the entire chat tree dozens of times,
/// pinning WebKit at 100%+ CPU. The turn-final
/// `AssistantMessageCompleted` is always emitted by
/// `finalize_assistant_message`, so the user sees the final state
/// immediately when the run ends regardless of throttling.
async fn flush_assistant_message_content(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &mut ClaudeStreamState,
) -> Result<(), LocalAgentRunError> {
    let content: Vec<ContentPart> = state
        .parts
        .iter()
        .filter(|p| !matches!(p, ContentPart::Text { text } if text.is_empty()))
        .cloned()
        .collect();
    if content.is_empty() {
        return Ok(());
    }
    let updated =
        match repository::update_message_content(&deps.pool, &assistant_message.id, &content).await
        {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    message_id = %assistant_message.id,
                    "Failed to flush assistant message content mid-turn"
                );
                return Ok(());
            }
        };

    let now = std::time::Instant::now();
    let should_emit = match state.last_update_emit_at {
        None => true,
        Some(last) => now.duration_since(last).as_millis() >= ASSISTANT_UPDATE_EMIT_THROTTLE_MS,
    };
    if should_emit {
        state.last_update_emit_at = Some(now);
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::AssistantMessageUpdated { message: updated },
        );
    }
    Ok(())
}

/// Walks a complete `"type":"assistant"` envelope and consumes any
/// content blocks the streamed deltas didn't already cover. Tool_use is
/// the main reason this exists — partial-message streaming in Claude
/// Code may not include tool_use deltas, so we treat the complete
/// envelope as authoritative for that. Text/thinking blocks are skipped
/// here because the streamed path already accumulated them; re-adding
/// would double up.
async fn adopt_complete_assistant_message(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    message: &Value,
    state: &mut ClaudeStreamState,
) -> Result<(), LocalAgentRunError> {
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return Ok(());
    };
    for block in content {
        let Some(block_type) = block.get("type").and_then(Value::as_str) else {
            continue;
        };
        if block_type != "tool_use" {
            continue;
        }
        let tool_use_id = match block.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue,
        };
        if state.persisted_tool_use_ids.contains(&tool_use_id) {
            continue;
        }
        let tool_name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
        persist_tool_use(
            deps,
            session,
            run_id,
            assistant_message,
            state,
            &tool_use_id,
            &tool_name,
            input,
        )
        .await?;
    }
    Ok(())
}

/// Update the persisted tool_call record from a `tool_result` block,
/// emit the matching `ToolCallCompleted` / `ToolCallFailed` UI event,
/// and create the Tool-role message whose `ContentPart::ToolResult`
/// carries the payload the chat UI renders.
async fn apply_tool_result(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    tool_use_id: &str,
    block: &Value,
) -> Result<(), LocalAgentRunError> {
    let is_error = block
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let payload = block.get("content").cloned().unwrap_or(Value::Null);

    let (status, result_arg, error_arg) = if is_error {
        let error_text = extract_text_from_tool_result(&payload)
            .unwrap_or_else(|| "Tool execution failed".to_string());
        (ToolCallStatus::Failed, None, Some(error_text))
    } else {
        (ToolCallStatus::Completed, Some(payload.clone()), None)
    };

    let updated = match repository::update_tool_call(
        &deps.pool,
        tool_use_id,
        status.clone(),
        result_arg.as_ref(),
        error_arg.as_deref(),
    )
    .await
    {
        Ok(tc) => tc,
        Err(err) => {
            tracing::warn!(
                tool_use_id = %tool_use_id,
                error = %err,
                "Claude tool_result update failed even after tool_use was registered"
            );
            return Ok(());
        }
    };

    let started_at = updated.started_at;
    let completed_at = updated.completed_at;

    let ui_event = if is_error {
        AssistantUiEvent::ToolCallFailed { tool_call: updated }
    } else {
        AssistantUiEvent::ToolCallCompleted { tool_call: updated }
    };
    let _ = emit_event(&deps.app, session, Some(run_id), ui_event);

    let result_message = repository::create_message(
        &deps.pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id: tool_use_id.to_string(),
                payload,
                started_at: Some(started_at),
                completed_at,
            }],
            provider_metadata: Some(serde_json::json!({
                "source": "claude-code",
            })),
        },
    )
    .await?;

    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::MessageCreated {
            message: result_message,
        },
    );

    Ok(())
}

/// Best-effort extraction of a text representation from a tool_result's
/// `content` field. Claude Code emits it as either a string or an array
/// of content blocks (each typically `{ "type": "text", "text": "..." }`).
fn extract_text_from_tool_result(payload: &Value) -> Option<String> {
    if let Some(s) = payload.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = payload.as_array() {
        let mut buf = String::new();
        for block in arr {
            if let Some(t) = block.get("text").and_then(Value::as_str) {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(t);
            }
        }
        if !buf.is_empty() {
            return Some(buf);
        }
    }
    None
}

async fn finalize_assistant_message(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &ClaudeStreamState,
) -> Result<(), LocalAgentRunError> {
    // Build the final content from the ordered parts vec. Drop empty
    // Text parts (left over when a turn was purely tool calls — the
    // assistant_message row was seeded with a placeholder empty Text
    // that we no longer need). If everything came out empty (e.g. a
    // run that errored before any content was emitted) we still write
    // a single empty Text to keep the message row schema consistent
    // with what the UI expects.
    let mut content: Vec<ContentPart> = state
        .parts
        .iter()
        .filter(|p| !matches!(p, ContentPart::Text { text } if text.is_empty()))
        .cloned()
        .collect();
    if content.is_empty() {
        content.push(ContentPart::Text {
            text: String::new(),
        });
    }

    let updated =
        repository::update_message_content(&deps.pool, &assistant_message.id, &content).await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::AssistantMessageCompleted { message: updated },
    );
    Ok(())
}

fn provider_message_text(message: &ProviderInputMessage) -> String {
    message
        .content
        .iter()
        .filter_map(content_part_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn message_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(content_part_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn content_part_text(part: &ContentPart) -> Option<String> {
    match part {
        ContentPart::Text { text } | ContentPart::Thinking { text } => Some(text.clone()),
        ContentPart::ToolUse {
            tool_name,
            arguments,
            ..
        } => Some(format!("Tool use `{}`: {}", tool_name, arguments)),
        ContentPart::ToolResult { payload, .. } => Some(format!("Tool result: {}", payload)),
    }
}

fn usage_from_value(value: Option<&Value>) -> Option<RunUsage> {
    let value = value?;
    let input_tokens = value.get("input_tokens").and_then(Value::as_u64);
    let output_tokens = value.get("output_tokens").and_then(Value::as_u64);
    let reasoning_tokens = value.get("reasoning_tokens").and_then(Value::as_u64);
    let total_tokens = match (input_tokens, output_tokens, reasoning_tokens) {
        (None, None, None) => None,
        _ => Some(
            input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0) + reasoning_tokens.unwrap_or(0),
        ),
    };
    Some(RunUsage {
        input_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
    })
}

fn write_mcp_config(url: &str, token: &str) -> Result<PathBuf, LocalAgentRunError> {
    let dir = dirs::config_dir()
        .ok_or_else(|| LocalAgentRunError::failed("Could not determine config directory"))?
        .join("clai")
        .join("tmp");
    std::fs::create_dir_all(&dir).map_err(|e| LocalAgentRunError::failed(e.to_string()))?;
    let path = dir.join(format!("claude-mcp-{}.json", Uuid::new_v4()));
    let config = serde_json::json!({
        "mcpServers": {
            "clai": {
                "type": "http",
                "url": url,
                "headers": {
                    "Authorization": format!("Bearer {}", token)
                }
            }
        }
    });
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&config)
            .map_err(|e| LocalAgentRunError::failed(e.to_string()))?,
    )
    .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path)
            .map_err(|e| LocalAgentRunError::failed(e.to_string()))?
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&path, permissions)
            .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;
    }

    Ok(path)
}

fn spawn_stderr_logger(
    run_id: String,
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
) {
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(run_id = %run_id, stderr = %line, "Claude Code stderr");
            if let Ok(mut buffer) = tail.lock() {
                if buffer.len() == STDERR_TAIL_LINES {
                    buffer.pop_front();
                }
                buffer.push_back(line);
            }
        }
    });
}

async fn fail_run(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
    error_msg: &str,
) -> Result<(), AssistantEngineError> {
    let run = repository::complete_run(
        &deps.pool,
        run_id,
        RunStatus::Failed,
        usage,
        Some(error_msg),
        &[],
    )
    .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::RunFailed { run },
    );
    Ok(())
}

async fn cancel_run(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
) -> Result<(), AssistantEngineError> {
    let run = repository::complete_run(&deps.pool, run_id, RunStatus::Cancelled, usage, None, &[])
        .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::RunCancelled { run },
    );
    Ok(())
}

enum LocalAgentRunError {
    Cancelled {
        usage: Option<RunUsage>,
    },
    Failed {
        message: String,
        usage: Option<RunUsage>,
    },
}

impl LocalAgentRunError {
    fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
            usage: None,
        }
    }

    fn message(self) -> String {
        match self {
            LocalAgentRunError::Cancelled { .. } => "run cancelled".to_string(),
            LocalAgentRunError::Failed { message, .. } => message,
        }
    }
}

impl From<String> for LocalAgentRunError {
    fn from(value: String) -> Self {
        LocalAgentRunError::failed(value)
    }
}

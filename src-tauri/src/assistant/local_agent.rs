use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

const STDERR_TAIL_LINES: usize = 20;

use serde_json::Value;
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
use crate::assistant::repository::{self, CreateMessageParams, CreateRunParams};
use crate::assistant::types::{
    AssistantMessage, AssistantSession, ContentPart, MessageRole, ProviderConnection,
    ProviderInputMessage, RunNotice, RunStatus, RunUsage,
};

const CLAUDE_DISABLED_TOOLS: &str = "Bash,Read,Edit,Write,Glob,Grep,WebFetch,WebSearch,Task,TodoWrite,NotebookEdit,NotebookRead,LSP";

pub async fn run_session_turn(
    deps: &AssistantDeps,
    input: RunTurnInput,
) -> Result<(), AssistantEngineError> {
    let mut session = repository::get_session(&deps.pool, &input.session_id)
        .await?
        .ok_or_else(|| AssistantEngineError::SessionNotFound(input.session_id.clone()))?;

    let connection = repository::get_provider_connection(&deps.pool, &input.connection_id)
        .await?
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
    let token = mcp_runtime
        .bind_run(ToolBinding {
            session_id: session.id.clone(),
            run_id: run_id.clone(),
            cancel_token: input.cancel_token.clone(),
            inter_agent_call_depth: input.inter_agent_call_depth,
            notices: notices.clone(),
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
    let mut accumulated_text = String::new();
    let mut accumulated_thinking = String::new();
    let mut usage: Option<RunUsage> = None;
    let mut result_error: Option<String> = None;

    loop {
        let line = tokio::select! {
            _ = cancel_token.cancelled() => {
                let _ = child.kill().await;
                finalize_assistant_message(
                    deps,
                    session,
                    run_id,
                    &assistant_message,
                    &accumulated_text,
                    &accumulated_thinking,
                ).await?;
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
            &mut accumulated_text,
            &mut accumulated_thinking,
            &mut usage,
            &mut result_error,
        )
        .await?;
    }

    let status = child
        .wait()
        .await
        .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;
    finalize_assistant_message(
        deps,
        session,
        run_id,
        &assistant_message,
        &accumulated_text,
        &accumulated_thinking,
    )
    .await?;

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

#[allow(clippy::too_many_arguments)]
async fn handle_claude_event(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    value: &Value,
    accumulated_text: &mut String,
    accumulated_thinking: &mut String,
    usage: &mut Option<RunUsage>,
    result_error: &mut Option<String>,
) -> Result<(), LocalAgentRunError> {
    match value.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            let event = value.get("event").unwrap_or(&Value::Null);
            match event.get("type").and_then(Value::as_str) {
                Some("content_block_start") => {
                    let block = event.get("content_block").unwrap_or(&Value::Null);
                    if block.get("type").and_then(Value::as_str) == Some("text")
                        && !accumulated_text.is_empty()
                        && !accumulated_text.ends_with("\n\n")
                    {
                        let separator = if accumulated_text.ends_with('\n') {
                            "\n"
                        } else {
                            "\n\n"
                        };
                        accumulated_text.push_str(separator);
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
                }
                Some("content_block_delta") => {
                    let delta = event.get("delta").unwrap_or(&Value::Null);
                    match delta.get("type").and_then(Value::as_str) {
                        Some("text_delta") => {
                            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                accumulated_text.push_str(text);
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
                                accumulated_thinking.push_str(text);
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
                        _ => {}
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
            } else if accumulated_text.is_empty() {
                if let Some(text) = value.get("result").and_then(Value::as_str) {
                    accumulated_text.push_str(text);
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

async fn finalize_assistant_message(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    accumulated_text: &str,
    accumulated_thinking: &str,
) -> Result<(), LocalAgentRunError> {
    let mut content = Vec::new();
    if !accumulated_thinking.is_empty() {
        content.push(ContentPart::Thinking {
            text: accumulated_thinking.to_string(),
        });
    }
    content.push(ContentPart::Text {
        text: accumulated_text.to_string(),
    });
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

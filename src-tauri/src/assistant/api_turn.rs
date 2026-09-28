use crate::assistant::{
    conversation::content_text, conversation::RunConversation,
    providers::types::is_context_limit_error,
};
use futures::StreamExt;
use std::path::PathBuf;
use tauri::Manager;

use crate::assistant::engine::{AssistantDeps, AssistantEngineError, RunTurnInput};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::providers;
use crate::assistant::providers::types::{ProviderAdapter, ProviderError};
use crate::assistant::repository;
use crate::assistant::repository::CreateMessageParams;
use crate::assistant::run_lifecycle::{
    cancel_run, complete_run_with_notices, fail_run, record_tool_call_result,
    record_tool_call_started, resolve_run_id, MissingToolCall, ToolCallOutcome,
};
use crate::assistant::system_prompt::{build_system_prompt, live_agent_description};
use crate::assistant::tools::{self, ToolExecutionContext};
use crate::assistant::turn_common::{
    build_trigger_message, discard_unanswered_run_input, run_produced_no_content,
};
use crate::assistant::types::{
    AssistantMessage, AssistantSession, CompactionTrigger, CompletionRequest, ContentPart,
    MessageRole, ProviderConnection, ProviderEvent, ProviderInputMessage, RunStatus,
    ToolDefinition, ToolInvocationDraft,
};
use crate::assistant::{compaction, compaction_service};
use crate::AppState;

pub async fn run_session_turn(
    deps: &AssistantDeps,
    input: RunTurnInput,
    session: AssistantSession,
    connection: ProviderConnection,
    workspace_root: Option<PathBuf>,
) -> Result<(), AssistantEngineError> {
    run_api_session_turn_with_adapter(deps, input, session, connection, workspace_root, None).await
}

#[allow(
    clippy::cognitive_complexity,
    reason = "the turn orchestrator retains terminal status branches"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the turn orchestrator still exceeds the 100-line budget"
)]
async fn run_api_session_turn_with_adapter(
    deps: &AssistantDeps,
    input: RunTurnInput,
    session: AssistantSession,
    connection: ProviderConnection,
    workspace_root: Option<PathBuf>,
    injected_adapter: Option<&dyn ProviderAdapter>,
) -> Result<(), AssistantEngineError> {
    let Some((run_id, system_message, tool_defs, mut state, resolved_adapter)) = start_api_turn(
        deps,
        &input,
        &session,
        &connection,
        workspace_root,
        injected_adapter,
    )
    .await?
    else {
        return Ok(());
    };
    let adapter = injected_adapter.unwrap_or_else(|| resolved_adapter.as_deref().unwrap());

    // No iteration cap: the agent runs as long as the LLM keeps emitting
    // tool calls. The cancel token is the only stop — surfaced as the
    // "Stop" button in the UI and any explicit cancel from upstream.
    // Provider-side context-length limits will surface as errors and
    // exit via fail_run; this loop itself imposes no ceiling.
    loop {
        if input.cancel_token.is_cancelled() {
            cancel_run(deps, &session, &run_id).await?;
            return Ok(());
        }

        let (request, queued_message_ids_in_request) = prepare_api_request(
            deps,
            &session,
            &connection,
            &run_id,
            &system_message,
            &tool_defs,
            &mut state,
        )
        .await?;

        // Call the provider
        let stream_result = adapter.stream_completion(&connection, request).await;

        let stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                if !state.retried_after_context_compaction && is_context_limit_error(&e.to_string())
                {
                    state.retried_after_context_compaction = true;
                    if try_context_limit_recovery(deps, &session, &connection, &run_id, &mut state)
                        .await?
                    {
                        continue;
                    }
                }
                return fail_opening_api_stream(deps, &input, &session, &run_id, &state, e).await;
            }
        };

        let assistant_message = accept_api_stream(
            deps,
            &session,
            &run_id,
            &queued_message_ids_in_request,
            &mut state,
        )
        .await?;

        let (stream_exit, tool_calls, produced_no_content) = consume_and_finalize_api_stream(
            stream,
            deps,
            &input,
            &session,
            &run_id,
            &assistant_message.id,
            &mut state.conversation,
        )
        .await?;

        match stream_exit {
            StreamExit::Completed => {}
            StreamExit::Cancelled => {
                cancel_run(deps, &session, &run_id).await?;
                return Ok(());
            }
            StreamExit::ProviderReported { message } => {
                fail_terminal_api_stream(
                    deps,
                    &input,
                    &session,
                    &run_id,
                    &state,
                    (&assistant_message.id, produced_no_content),
                    &message,
                )
                .await?;
                return Ok(());
            }
            StreamExit::StreamFailed { message } => {
                let message = fail_terminal_api_stream(
                    deps,
                    &input,
                    &session,
                    &run_id,
                    &state,
                    (&assistant_message.id, produced_no_content),
                    &message,
                )
                .await?;
                return Err(AssistantEngineError::Provider(
                    ProviderError::RequestFailed(message),
                ));
            }
        }

        // If no tool calls, we're done
        if tool_calls.is_empty() {
            break;
        }

        if matches!(
            execute_api_tool_calls(deps, &session, &run_id, &tool_calls, &mut state).await?,
            ToolLoopOutcome::Cancelled
        ) {
            return Ok(());
        }

        // Continue loop — will call API again with tool results in message history.
        state.iteration += 1;
    }

    let notices = state.tool_context.take_notices();
    complete_run_with_notices(deps, &session, &run_id, &notices).await?;

    Ok(())
}

async fn consume_and_finalize_api_stream(
    stream: ProviderStream,
    deps: &AssistantDeps,
    input: &RunTurnInput,
    session: &AssistantSession,
    run_id: &str,
    assistant_message_id: &str,
    conversation: &mut RunConversation,
) -> Result<(StreamExit, Vec<ToolInvocationDraft>, bool), AssistantEngineError> {
    let (exit, parts, calls) = consume_api_stream(
        stream,
        &input.cancel_token,
        deps,
        session,
        run_id,
        assistant_message_id,
    )
    .await;
    let produced_no_content = finalize_api_message(
        deps,
        session,
        run_id,
        assistant_message_id,
        &exit,
        parts,
        conversation,
    )
    .await?;
    Ok((exit, calls, produced_no_content))
}

async fn fail_opening_api_stream(
    deps: &AssistantDeps,
    input: &RunTurnInput,
    session: &AssistantSession,
    run_id: &str,
    state: &ApiTurnState,
    error: ProviderError,
) -> Result<(), AssistantEngineError> {
    let provider_message = error.to_string();
    let failure =
        failure_message_with_compaction_context(&provider_message, &state.compaction_attempt);
    fail_run(deps, session, run_id, &failure).await?;
    if state.iteration == 0 {
        discard_unanswered_run_input(
            deps,
            session,
            run_id,
            input.trigger_message_id.as_deref(),
            None,
        )
        .await;
    }
    Err(provider_error_with_compaction_context(error, &provider_message, failure).into())
}

async fn fail_terminal_api_stream(
    deps: &AssistantDeps,
    input: &RunTurnInput,
    session: &AssistantSession,
    run_id: &str,
    state: &ApiTurnState,
    assistant_message: (&str, bool),
    provider_message: &str,
) -> Result<String, AssistantEngineError> {
    let message =
        failure_message_with_compaction_context(provider_message, &state.compaction_attempt);
    fail_run(deps, session, run_id, &message).await?;
    if state.iteration == 0 && assistant_message.1 {
        discard_unanswered_run_input(
            deps,
            session,
            run_id,
            input.trigger_message_id.as_deref(),
            Some(assistant_message.0),
        )
        .await;
    }
    Ok(message)
}

async fn accept_api_stream(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    queued_message_ids_in_request: &[String],
    state: &mut ApiTurnState,
) -> Result<AssistantMessage, AssistantEngineError> {
    if let Err(e) = repository::mark_queued_messages_delivered(
        &deps.pool,
        &session.id,
        run_id,
        queued_message_ids_in_request,
    )
    .await
    {
        fail_run(deps, session, run_id, &e).await?;
        return Err(AssistantEngineError::Persistence(e));
    }
    state
        .conversation
        .mark_delivered(queued_message_ids_in_request);
    if !queued_message_ids_in_request.is_empty() {
        // The queued messages just became part of this run's request —
        // tell the FE so their "Queued" chips clear.
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::QueuedMessagesDelivered {
                message_ids: queued_message_ids_in_request.to_vec(),
            },
        );
    }

    // Create assistant message placeholder
    let assistant_message = state
        .conversation
        .create_message(
            &deps.pool,
            CreateMessageParams {
                session_id: session.id.clone(),
                role: MessageRole::Assistant,
                content: vec![ContentPart::Text {
                    text: String::new(),
                }],
                provider_metadata: None,
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

    Ok(assistant_message)
}

async fn start_api_turn(
    deps: &AssistantDeps,
    input: &RunTurnInput,
    session: &AssistantSession,
    connection: &ProviderConnection,
    workspace_root: Option<PathBuf>,
    injected_adapter: Option<&dyn ProviderAdapter>,
) -> Result<
    Option<(
        String,
        ProviderInputMessage,
        Vec<ToolDefinition>,
        ApiTurnState,
        Option<Box<dyn ProviderAdapter>>,
    )>,
    AssistantEngineError,
> {
    // Get or create the run
    let run_id = resolve_run_id(deps, session, connection, input).await?;

    // Transition run to Running
    let run = repository::update_run_status(&deps.pool, &run_id, RunStatus::Running, None).await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(&run_id),
        AssistantUiEvent::RunStarted { run },
    );

    if input.cancel_token.is_cancelled() {
        cancel_run(deps, session, &run_id).await?;
        return Ok(None);
    }

    let resolved_adapter = if injected_adapter.is_none() {
        Some(providers::resolve_adapter(&connection.protocol_id)?)
    } else {
        None
    };

    // Get available tools for this session's context
    let external_tools = {
        let state = deps.app.state::<crate::AppState>();
        let mut manager = state.mcp_client_manager.lock().await;
        manager
            .list_tools_for_servers(&session.context.mcp_server_ids)
            .await
    };
    let tool_defs = tools::available_tools(&session.context, &external_tools);

    // Build execution context for tool calls
    let notices = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // Run-scoped filesystem grants accepted via fs_request_grant stay visible
    // to subsequent tool calls through this shared context.
    let session_grants = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let session_allowed_command_prefixes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let session_blocked_command_prefixes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    // === Tool execution loop ===
    // Build system prompt (prepended to every API call, not persisted).
    // The agent description (user-set seed + skill content) is computed
    // fresh from the workspace config on every turn — see
    // workspace_agent_runtime_description for the rationale.
    let agent_description = live_agent_description(&deps.app, &session.context);
    let system_message = build_system_prompt(
        &session.context,
        agent_description.as_deref(),
        &tool_defs,
        &input.trigger,
        workspace_root.as_deref(),
    );

    let tool_context = ToolExecutionContext {
        session_id: session.id.clone(),
        run_id: run_id.clone(),
        tool_call_id: None,
        cancel_token: input.cancel_token.clone(),
        workspace_id: session.context.workspace_id.clone(),
        mcp_server_ids: session.context.mcp_server_ids.clone(),
        agent_workspace_id: session.context.agent_workspace_id.clone(),
        workspace_root: workspace_root.clone(),
        automation_id: session.context.automation_id.clone(),
        workspace_agents: session.context.workspace_agents.clone(),
        inter_agent_call_depth: input.inter_agent_call_depth,
        execution: session.context.execution.clone(),
        notices,
        session_grants,
        session_allowed_command_prefixes,
        session_blocked_command_prefixes,
    };

    // The run's conversation, read from the database once. Every row this run
    // persists below is appended to it; history is not re-read per iteration.
    let mut state = ApiTurnState {
        conversation: RunConversation::load(&deps.pool, &session.id).await?,
        iteration: 0,
        retried_after_context_compaction: false,
        compaction_attempt: compaction::CompactionAttempt::NotAttempted,
        tool_context,
    };

    // Persist the trigger message as a run boundary marker so the LLM can see
    // where one run ends and the next begins. Without this, the LLM sees old
    // tool results from prior runs and may skip re-running tools.
    if let Some(trigger_content) = build_trigger_message(session, &input.trigger) {
        let boundary_msg = state
            .conversation
            .create_message(
                &deps.pool,
                CreateMessageParams {
                    session_id: session.id.clone(),
                    role: trigger_content.role.clone(),
                    content: trigger_content.content.clone(),
                    provider_metadata: None,
                },
            )
            .await?;
        let _ = emit_event(
            &deps.app,
            session,
            Some(&run_id),
            AssistantUiEvent::MessageCreated {
                message: boundary_msg,
            },
        );
    }

    Ok(Some((
        run_id,
        system_message,
        tool_defs,
        state,
        resolved_adapter,
    )))
}

struct ApiTurnState {
    conversation: RunConversation,
    iteration: usize,
    retried_after_context_compaction: bool,
    compaction_attempt: compaction::CompactionAttempt,
    tool_context: ToolExecutionContext,
}

async fn prepare_api_request(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
    connection: &crate::assistant::types::ProviderConnection,
    run_id: &str,
    system_message: &ProviderInputMessage,
    tool_defs: &[crate::assistant::types::ToolDefinition],
    state: &mut ApiTurnState,
) -> Result<(CompletionRequest, Vec<String>), AssistantEngineError> {
    let system_prompt_text = content_text(&system_message.content);
    // Reconcile queued user input (sent, edited, or deleted while the run
    // was busy) before sizing and shaping the request; pending rows are
    // never summarized away. Then normalize before sending: drop empty
    // assistant placeholders, drop tool messages whose tool_call_id has no
    // matching tool_use in the preceding assistant turn, and merge
    // consecutive same-role messages. The DB stays the source of truth;
    // this only shapes what the provider sees so a mid-stream hangup or
    // stacked user typing can't poison subsequent runs.
    let pending = repository::list_pending_queued_messages(&deps.pool, &session.id).await?;
    state
        .conversation
        .refresh_pending(pending.into_iter().map(|queued| queued.message).collect());
    if compaction::should_auto_compact(&state.conversation, &system_prompt_text, tool_defs) {
        match compaction_service::compact_conversation(
            &deps.pool,
            session,
            connection,
            None,
            CompactionTrigger::Automatic,
            Some(run_id),
            compaction::verbatim_tail_tokens(&system_prompt_text, tool_defs),
            &mut state.conversation,
        )
        .await
        {
            Ok(Some(outcome)) => {
                state.compaction_attempt.record_success();
                let _ = emit_event(
                    &deps.app,
                    session,
                    Some(run_id),
                    AssistantUiEvent::SessionCompacted {
                        compaction: outcome.compaction,
                        summary_message: outcome.summary_message,
                    },
                );
            }
            // Nothing eligible yet; the forced paths can still compact more.
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    session_id = %session.id,
                    run_id = %run_id,
                    error = %error,
                    "Automatic assistant history compaction failed"
                );
                state.compaction_attempt.record_failure(&error);
            }
        }
        // Summarization may have waited while the user edited or deleted queued input.
        let pending = repository::list_pending_queued_messages(&deps.pool, &session.id).await?;
        state
            .conversation
            .refresh_pending(pending.into_iter().map(|queued| queued.message).collect());
    }
    let queued_message_ids_in_request = state.conversation.pending_ids();
    let normalized = normalize_history_for_provider(state.conversation.messages());
    let supports_images = providers::connection_supports_images(connection);
    // Drop image parts from history when the active connection can't accept
    // them (e.g. user switched to a non-vision provider mid-conversation).
    // The gate stops *new* image attachments; this keeps replayed history
    // valid for the current provider. CLI providers already render images as
    // a text placeholder upstream, so this only bites the HTTP path.
    let normalized = if supports_images {
        normalized
    } else {
        strip_unsupported_images(normalized)
    };

    let mut provider_messages = vec![system_message.clone()];
    provider_messages.extend(normalized);

    // Resolve image files to inline base64 for image-capable API providers.
    // CLI providers ingest images via their own mechanism (file paths in the
    // CLI invocation), not this HTTP request, so they don't need inlining.
    let images = if supports_images && !providers::is_cli_provider(&connection.protocol_id) {
        resolve_request_images(deps, session, &provider_messages).await
    } else {
        std::collections::HashMap::new()
    };

    let request = CompletionRequest {
        run_id: run_id.to_string(),
        session_id: session.id.clone(),
        model_id: connection.model_id.clone(),
        messages: provider_messages,
        tools: tool_defs.to_vec(),
        temperature: None,
        max_output_tokens: None,
        images,
    };

    Ok((request, queued_message_ids_in_request))
}

async fn try_context_limit_recovery(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
    connection: &crate::assistant::types::ProviderConnection,
    run_id: &str,
    state: &mut ApiTurnState,
) -> Result<bool, AssistantEngineError> {
    match compaction_service::compact_for_context_limit_recovery(
        &deps.pool,
        session,
        connection,
        None,
        run_id,
        &mut state.conversation,
    )
    .await
    {
        Ok(Some(outcome)) => {
            state.compaction_attempt.record_success();
            let _ = emit_event(
                &deps.app,
                session,
                Some(run_id),
                AssistantUiEvent::SessionCompacted {
                    compaction: outcome.compaction,
                    summary_message: outcome.summary_message,
                },
            );
            return Ok(true);
        }
        Ok(None) => {
            tracing::warn!(
                session_id = %session.id,
                run_id = %run_id,
                "Context-limit recovery found no compactable assistant history"
            );
            state.compaction_attempt.record_nothing_to_compact();
        }
        Err(error) => {
            tracing::warn!(
                session_id = %session.id,
                run_id = %run_id,
                error = %error,
                "Context-limit recovery compaction failed"
            );
            state.compaction_attempt.record_failure(&error);
        }
    }
    Ok(false)
}

async fn finalize_api_message(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
    run_id: &str,
    message_id: &str,
    exit: &StreamExit,
    parts: Vec<ContentPart>,
    conversation: &mut RunConversation,
) -> Result<bool, AssistantEngineError> {
    let produced_no_content = run_produced_no_content(&parts);
    if exit_keeps_streamed_message(exit, produced_no_content) {
        let final_content = final_content_parts(parts);
        let updated_message = conversation
            .update_message_content(&deps.pool, message_id, &final_content)
            .await?;
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::AssistantMessageCompleted {
                message: updated_message,
            },
        );
    }
    Ok(produced_no_content)
}

enum ToolLoopOutcome {
    Continue,
    Cancelled,
}

async fn execute_api_tool_calls(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
    run_id: &str,
    calls: &[ToolInvocationDraft],
    state: &mut ApiTurnState,
) -> Result<ToolLoopOutcome, AssistantEngineError> {
    tracing::info!(
        "Assistant engine: executing {} tool call(s) (iteration {})",
        calls.len(),
        state.iteration + 1
    );

    // Execute each tool call
    for tc in calls {
        if state.tool_context.cancel_token.is_cancelled() {
            cancel_run(deps, session, run_id).await?;
            return Ok(ToolLoopOutcome::Cancelled);
        }

        state.tool_context.tool_call_id = Some(tc.tool_call_id.clone());

        // Persist tool call
        record_tool_call_started(
            deps,
            session,
            run_id,
            &tc.tool_call_id,
            &tc.tool_name,
            tc.params.clone(),
        )
        .await?;

        let tool_result = tokio::select! {
            _ = state.tool_context.cancel_token.cancelled() => {
                cancel_run(deps, session, run_id).await?;
                return Ok(ToolLoopOutcome::Cancelled);
            }
            result = tools::execute_tool(
                deps,
                &state.tool_context,
                &tc.tool_name,
                tc.params.clone()
            ) => result,
        };

        // A failed tool still gets its result persisted, so the provider
        // sees why the call failed on the next request.
        let outcome = match tool_result {
            Ok(result) => ToolCallOutcome::Completed { payload: result },
            Err(ref error) => ToolCallOutcome::Failed {
                payload: serde_json::json!({ "error": error }),
                error: Some(error.as_str()),
            },
        };
        record_tool_call_result(
            deps,
            session,
            run_id,
            &tc.tool_call_id,
            outcome,
            None,
            MissingToolCall::Propagate,
            &mut state.conversation,
        )
        .await?;
    }

    Ok(ToolLoopOutcome::Continue)
}

type ProviderStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>;

async fn consume_api_stream(
    stream: ProviderStream,
    cancel: &tokio_util::sync::CancellationToken,
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    message_id: &str,
) -> (StreamExit, Vec<ContentPart>, Vec<ToolInvocationDraft>) {
    consume_api_stream_with_sink(stream, cancel, message_id, |event| {
        let _ = emit_event(&deps.app, session, Some(run_id), event);
    })
    .await
}

async fn consume_api_stream_with_sink(
    mut stream: ProviderStream,
    cancel: &tokio_util::sync::CancellationToken,
    message_id: &str,
    mut emit: impl FnMut(AssistantUiEvent),
) -> (StreamExit, Vec<ContentPart>, Vec<ToolInvocationDraft>) {
    let mut content_parts: Vec<ContentPart> = Vec::new();
    let mut tool_calls: Vec<ToolInvocationDraft> = Vec::new();

    let stream_exit = loop {
        match tokio::select! {
            _ = cancel.cancelled() => None,
            next = stream.next() => next,
        } {
            None if cancel.is_cancelled() => {
                break StreamExit::Cancelled;
            }
            Some(Ok(event)) => match event {
                ProviderEvent::MessageStart => {}
                ProviderEvent::TextDelta { text } => {
                    push_text_delta(&mut content_parts, &text);
                    emit(AssistantUiEvent::AssistantDelta {
                        message_id: message_id.to_string(),
                        text,
                    });
                }
                ProviderEvent::ThinkingDelta { text } => {
                    push_thinking_delta(&mut content_parts, &text);
                    emit(AssistantUiEvent::AssistantThinkingDelta {
                        message_id: message_id.to_string(),
                        text,
                    });
                }
                ProviderEvent::ThinkingSignature { signature } => {
                    // Anthropic sends the signature in its own event after a
                    // thinking block's text; bind it to that block so it can
                    // be replayed verbatim. A later thinking_delta opens a new
                    // block, so each block keeps its own signature.
                    set_thinking_signature(&mut content_parts, signature);
                }
                ProviderEvent::ToolCallReady { tool_call } => {
                    // Record it in content (in position) and in tool_calls
                    // (for the execution loop below).
                    content_parts.push(ContentPart::ToolUse {
                        tool_call_id: tool_call.tool_call_id.clone(),
                        tool_name: tool_call.tool_name.clone(),
                        arguments: tool_call.params.clone(),
                    });
                    tool_calls.push(tool_call);
                }
                ProviderEvent::ToolCallDelta { .. } => {
                    // Could emit live UI updates here in the future
                }
                ProviderEvent::MessageComplete => {
                    // Finalization happens once after the stream loop exits
                    // so we also capture in-memory state (text + tool_calls)
                    // when the provider hangs up before sending [DONE].
                }
                ProviderEvent::ProviderError { message } => {
                    break StreamExit::ProviderReported { message };
                }
            },
            Some(Err(e)) => {
                break StreamExit::StreamFailed {
                    message: e.to_string(),
                };
            }
            None => break StreamExit::Completed,
        }
    };

    (stream_exit, content_parts, tool_calls)
}

// The existing helper tests still precede their private functions; keep the
// test module in place to avoid moving unrelated code in this refactor.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::turn_common::message_contains_image;
    use crate::assistant::types::ContentPart;
    use crate::assistant::types::RunTrigger;
    use crate::assistant::types::SessionContext;
    use crate::assistant::types::SessionKind;

    #[tokio::test]
    async fn partial_assistant_output_survives_database_reload() {
        use crate::assistant::repository::CreateSessionParams;
        use crate::assistant::types::SessionContext;
        use crate::db::test_support::workspace_pool;

        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(
            &pool,
            CreateSessionParams {
                kind: SessionKind::Interactive,
                title: None,
                context: SessionContext {
                    workspace_id: None,
                    tool_scopes: vec![],
                    mcp_server_ids: vec![],
                    execution: Default::default(),
                    cli_session_id: None,
                    cli_session_provider: None,
                    automation_id: None,
                    agent_workspace_id: None,
                    automation_name: None,
                    inter_agent_call: None,
                    workspace_agents: vec![],
                },
            },
        )
        .await
        .unwrap();
        let mut conversation = RunConversation::load(&pool, &session.id).await.unwrap();
        let placeholder = conversation
            .create_message(
                &pool,
                CreateMessageParams {
                    session_id: session.id.clone(),
                    role: MessageRole::Assistant,
                    content: vec![text("")],
                    provider_metadata: None,
                },
            )
            .await
            .unwrap();
        let partial = vec![text("partial answer")];
        assert!(exit_keeps_streamed_message(
            &StreamExit::StreamFailed {
                message: "connection reset".into(),
            },
            run_produced_no_content(&partial),
        ));
        conversation
            .update_message_content(&pool, &placeholder.id, &final_content_parts(partial))
            .await
            .unwrap();

        let reloaded = RunConversation::load(&pool, &session.id).await.unwrap();
        let provider = normalize_history_for_provider(reloaded.messages());
        assert_eq!(provider.len(), 1);
        assert!(
            matches!(&provider[0].content[0], ContentPart::Text { text } if text == "partial answer")
        );
    }

    #[tokio::test]
    async fn streamed_text_and_tool_result_replay_in_order() {
        use crate::assistant::repository::CreateSessionParams;
        use crate::db::test_support::workspace_pool;

        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(
            &pool,
            CreateSessionParams {
                kind: SessionKind::Interactive,
                title: None,
                context: SessionContext {
                    workspace_id: None,
                    tool_scopes: vec![],
                    mcp_server_ids: vec![],
                    execution: Default::default(),
                    cli_session_id: None,
                    cli_session_provider: None,
                    automation_id: None,
                    agent_workspace_id: None,
                    automation_name: None,
                    inter_agent_call: None,
                    workspace_agents: vec![],
                },
            },
        )
        .await
        .unwrap();
        let mut conversation = RunConversation::load(&pool, &session.id).await.unwrap();
        let assistant = conversation
            .create_message(
                &pool,
                CreateMessageParams {
                    session_id: session.id.clone(),
                    role: MessageRole::Assistant,
                    content: vec![text("")],
                    provider_metadata: None,
                },
            )
            .await
            .unwrap();
        let call = ToolInvocationDraft {
            tool_call_id: "call-1".into(),
            tool_name: "test".into(),
            params: serde_json::json!({"x": 1}),
        };
        let stream: ProviderStream = Box::pin(futures::stream::iter(vec![
            Ok(ProviderEvent::TextDelta {
                text: "answer".into(),
            }),
            Ok(ProviderEvent::ToolCallReady { tool_call: call }),
        ]));
        let mut deltas = Vec::new();
        let (exit, parts, calls) = consume_api_stream_with_sink(
            stream,
            &tokio_util::sync::CancellationToken::new(),
            &assistant.id,
            |event| deltas.push(event),
        )
        .await;
        assert!(matches!(exit, StreamExit::Completed));
        assert!(
            matches!(&deltas[..], [AssistantUiEvent::AssistantDelta { text, .. }] if text == "answer")
        );
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            &parts[..],
            [ContentPart::Text { .. }, ContentPart::ToolUse { .. }]
        ));
        conversation
            .update_message_content(&pool, &assistant.id, &parts)
            .await
            .unwrap();
        conversation
            .create_message(
                &pool,
                CreateMessageParams {
                    session_id: session.id.clone(),
                    role: MessageRole::Tool,
                    content: vec![ContentPart::ToolResult {
                        tool_call_id: "call-1".into(),
                        payload: serde_json::json!({"ok": true}),
                        started_at: None,
                        completed_at: None,
                    }],
                    provider_metadata: None,
                },
            )
            .await
            .unwrap();
        let rows = repository::list_messages(&pool, &session.id).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0].role, MessageRole::Assistant));
        assert!(matches!(rows[1].role, MessageRole::Tool));
        let replay = normalize_history_for_provider(&rows);
        assert!(matches!(
            &replay[0].content[..],
            [ContentPart::Text { .. }, ContentPart::ToolUse { .. }]
        ));
        assert!(matches!(
            &replay[1].content[..],
            [ContentPart::ToolResult { .. }]
        ));
    }

    #[tokio::test]
    async fn cancellation_after_delta_and_stream_error_keep_partial_text() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let stream_cancel = cancel.clone();
        let stream: ProviderStream = Box::pin(futures::stream::unfold(0, move |n| {
            let stream_cancel = stream_cancel.clone();
            async move {
                if n == 0 {
                    Some((
                        Ok(ProviderEvent::TextDelta {
                            text: "partial".into(),
                        }),
                        1,
                    ))
                } else {
                    stream_cancel.cancel();
                    futures::future::pending().await
                }
            }
        }));
        let mut events = Vec::new();
        let (exit, parts, _) =
            consume_api_stream_with_sink(stream, &cancel, "assistant", |event| events.push(event))
                .await;
        assert!(matches!(exit, StreamExit::Cancelled));
        assert!(matches!(&parts[..], [ContentPart::Text { text }] if text == "partial"));
        assert_eq!(events.len(), 1);
        assert!(exit_keeps_streamed_message(
            &exit,
            run_produced_no_content(&parts)
        ));

        let stream: ProviderStream = Box::pin(futures::stream::iter(vec![
            Ok(ProviderEvent::TextDelta {
                text: "partial".into(),
            }),
            Err(ProviderError::RequestFailed("context length".into())),
        ]));
        let (exit, parts, _) = consume_api_stream_with_sink(
            stream,
            &tokio_util::sync::CancellationToken::new(),
            "assistant",
            |_| {},
        )
        .await;
        assert!(matches!(exit, StreamExit::StreamFailed { .. }));
        assert!(matches!(&parts[..], [ContentPart::Text { text }] if text == "partial"));
        assert!(exit_keeps_streamed_message(
            &exit,
            run_produced_no_content(&parts)
        ));
    }

    fn text(t: &str) -> ContentPart {
        ContentPart::Text {
            text: t.to_string(),
        }
    }

    /// R3.2. Pressing Stop mid-answer used to return straight out of the
    /// stream loop, so everything already streamed was never written back to
    /// the assistant row and the answer the user watched arrive disappeared on
    /// the next load. A cancelled turn that produced content must be persisted.
    #[test]
    fn cancelled_turn_with_streamed_content_is_still_persisted() {
        assert!(exit_keeps_streamed_message(
            &StreamExit::Cancelled,
            /* produced_no_content */ false
        ));
    }

    /// Same rule for a mid-stream provider error: whatever was already on
    /// screen is real output and belongs in the row.
    #[test]
    fn failed_turn_with_streamed_content_is_still_persisted() {
        assert!(exit_keeps_streamed_message(
            &StreamExit::ProviderReported {
                message: "overloaded".to_string(),
            },
            false
        ));
        assert!(exit_keeps_streamed_message(
            &StreamExit::StreamFailed {
                message: "connection reset".to_string(),
            },
            false
        ));
    }

    /// The one case that is not finalized: nothing was streamed, so writing
    /// back would persist the same empty `Text` the row already holds and
    /// emit a completion event with nothing in it.
    #[test]
    fn contentless_cancel_or_failure_does_not_finalize_the_placeholder() {
        for exit in [
            StreamExit::Cancelled,
            StreamExit::ProviderReported {
                message: "boom".to_string(),
            },
            StreamExit::StreamFailed {
                message: "boom".to_string(),
            },
        ] {
            assert!(
                !exit_keeps_streamed_message(&exit, /* produced_no_content */ true),
                "{exit:?} produced nothing, so its placeholder must stay untouched"
            );
        }
    }

    /// A normal end is always finalized, even with no content: the row must
    /// hold exactly one (possibly empty) part, and tool-only turns land here.
    #[test]
    fn completed_turn_is_always_finalized() {
        assert!(exit_keeps_streamed_message(&StreamExit::Completed, true));
        assert!(exit_keeps_streamed_message(&StreamExit::Completed, false));
    }

    #[test]
    fn final_content_parts_guarantees_one_part_when_nothing_streamed() {
        let parts = final_content_parts(Vec::new());
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], ContentPart::Text { text } if text.is_empty()));
    }

    /// Arrival order is the model's real text↔thinking↔tool interleaving and
    /// must survive verbatim — including a half-streamed sentence, which is
    /// exactly what a cancelled turn persists.
    #[test]
    fn final_content_parts_preserves_arrival_order_and_partial_text() {
        let parts = final_content_parts(vec![
            ContentPart::Thinking {
                text: "let me check".to_string(),
                signature: None,
            },
            text("Here is the half-writ"),
        ]);
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], ContentPart::Thinking { .. }));
        assert!(matches!(&parts[1], ContentPart::Text { text } if text == "Here is the half-writ"));
    }

    #[test]
    fn message_contains_image_detects_image_parts() {
        assert!(message_contains_image(&[
            ContentPart::Text {
                text: "see this".to_string()
            },
            ContentPart::Image {
                id: "img-1".to_string(),
                path: "img-1.png".to_string(),
                media_type: "image/png".to_string(),
                filename: None,
                width: None,
                height: None,
            },
        ]));
        assert!(!message_contains_image(&[ContentPart::Text {
            text: "no image".to_string()
        }]));
        assert!(!message_contains_image(&[]));
    }

    #[test]
    fn strip_unsupported_images_replaces_image_parts_with_placeholder() {
        let messages = vec![ProviderInputMessage {
            role: MessageRole::User,
            content: vec![
                ContentPart::Text {
                    text: "look at this".to_string(),
                },
                ContentPart::Image {
                    id: "img1".to_string(),
                    path: ".clai/images/img1.png".to_string(),
                    media_type: "image/png".to_string(),
                    filename: None,
                    width: None,
                    height: None,
                },
            ],
        }];

        let out = strip_unsupported_images(messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content.len(), 2);
        assert!(matches!(&out[0].content[0], ContentPart::Text { text } if text == "look at this"));
        assert!(
            matches!(&out[0].content[1], ContentPart::Text { text } if text == "[image omitted]"),
            "image part should become a text placeholder"
        );
    }

    fn user_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            id: format!("msg-user-{}", text.len()),
            session_id: "session".to_string(),
            role: MessageRole::User,
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            created_at: 0,
            provider_metadata: None,
        }
    }

    fn assistant_message_with_text(text: &str) -> AssistantMessage {
        assistant_message_with_content(vec![ContentPart::Text {
            text: text.to_string(),
        }])
    }

    fn assistant_message_with_content(content: Vec<ContentPart>) -> AssistantMessage {
        AssistantMessage {
            id: format!("msg-assistant-{}", content.len()),
            session_id: "session".to_string(),
            role: MessageRole::Assistant,
            content,
            created_at: 0,
            provider_metadata: None,
        }
    }

    fn tool_message(tool_call_id: &str) -> AssistantMessage {
        AssistantMessage {
            id: format!("msg-tool-{}", tool_call_id),
            session_id: "session".to_string(),
            role: MessageRole::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                payload: serde_json::json!({ "ok": true }),
                started_at: None,
                completed_at: None,
            }],
            created_at: 0,
            provider_metadata: None,
        }
    }

    #[test]
    fn normalize_drops_orphan_tool_messages() {
        let messages = vec![
            user_message("write a file"),
            assistant_message_with_text("Asking a question, no tool calls"),
            tool_message("call_orphan"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[0].role, MessageRole::User);
        assert_eq!(normalized[1].role, MessageRole::Assistant);
    }

    #[test]
    fn normalize_keeps_tool_messages_when_assistant_has_matching_tool_use() {
        let messages = vec![
            user_message("write a file"),
            assistant_message_with_content(vec![
                ContentPart::Text {
                    text: "Writing now".into(),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_a".into(),
                    tool_name: "bash_exec".into(),
                    arguments: serde_json::json!({}),
                },
            ]),
            tool_message("call_a"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[2].role, MessageRole::Tool);
    }

    #[test]
    fn normalize_drops_empty_assistant_placeholder_and_merges_users() {
        // Reproduces the corruption from the failing scheduled run: an empty
        // assistant placeholder followed by stacked user messages typed while
        // earlier runs were failing, plus a scheduled-run boundary appended on
        // top by the engine.
        let messages = vec![
            user_message("first user question"),
            assistant_message_with_text(""),
            user_message("did you read me?"),
            user_message("--- New scheduled run at 2026-05-17 ---"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].role, MessageRole::User);
        let ContentPart::Text { text } = &normalized[0].content[0] else {
            panic!("expected text content");
        };
        assert!(text.contains("first user question"));
        assert!(text.contains("did you read me?"));
        assert!(text.contains("New scheduled run"));
    }

    #[test]
    fn normalize_drops_orphan_tools_with_matching_id_too_far_back() {
        // Tool message whose tool_call_id exists in an earlier assistant turn
        // but the immediately preceding assistant has only text. This is the
        // 20:40 corruption from the production session: the model emitted text
        // and tool_calls in one turn but the persisted assistant has text only,
        // so the tool rows after it have no anchor.
        let messages = vec![
            user_message("do work"),
            assistant_message_with_content(vec![ContentPart::ToolUse {
                tool_call_id: "call_old".into(),
                tool_name: "bash_exec".into(),
                arguments: serde_json::json!({}),
            }]),
            tool_message("call_old"),
            user_message("any update?"),
            assistant_message_with_text("Here's a question without tool calls"),
            tool_message("call_stranded"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        // user → assistant(tool_use) → tool(call_old) → user → assistant(text)
        // The stranded tool message at the tail is dropped.
        assert_eq!(normalized.len(), 5);
        assert_eq!(normalized[2].role, MessageRole::Tool);
        assert!(matches!(
            &normalized[2].content[0],
            ContentPart::ToolResult { tool_call_id, .. } if tool_call_id == "call_old"
        ));
        assert_eq!(normalized[4].role, MessageRole::Assistant);
    }

    #[test]
    fn normalize_strips_local_mcp_qualifier_from_claude_recorded_history() {
        // Rows persisted by pre-normalization Claude Code runs carry the
        // CLI-side qualified names. Replay must hand the provider the
        // canonical names or the model mimics the qualified ones after a
        // provider switch (the "not allowed for this session" bug).
        let messages = vec![
            user_message("fetch something"),
            assistant_message_with_content(vec![
                ContentPart::ToolUse {
                    tool_call_id: "call_a".into(),
                    tool_name: "mcp__clai__web_fetch".into(),
                    arguments: serde_json::json!({"url": "https://example.com"}),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_b".into(),
                    tool_name: "mcp__clai__mcp__1c47ed2d__search".into(),
                    arguments: serde_json::json!({}),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_c".into(),
                    tool_name: "bash_exec".into(),
                    arguments: serde_json::json!({}),
                },
            ]),
            // Every tool_use needs a result or the orphan-stripping pass
            // removes it before we can assert on its name.
            tool_message("call_a"),
            tool_message("call_b"),
            tool_message("call_c"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        let names: Vec<&str> = normalized[1]
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse { tool_name, .. } => Some(tool_name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            vec!["web_fetch", "mcp__1c47ed2d__search", "bash_exec"]
        );
    }

    #[test]
    fn normalize_preserves_happy_path_alternation() {
        let messages = vec![
            user_message("hi"),
            assistant_message_with_content(vec![
                ContentPart::Text {
                    text: "writing".into(),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_a".into(),
                    tool_name: "bash_exec".into(),
                    arguments: serde_json::json!({}),
                },
            ]),
            tool_message("call_a"),
            assistant_message_with_text("done"),
            user_message("thanks"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 5);
        assert_eq!(normalized[0].role, MessageRole::User);
        assert_eq!(normalized[1].role, MessageRole::Assistant);
        assert_eq!(normalized[2].role, MessageRole::Tool);
        assert_eq!(normalized[3].role, MessageRole::Assistant);
        assert_eq!(normalized[4].role, MessageRole::User);
    }

    #[test]
    fn normalize_merges_consecutive_user_messages_with_blank_line_separator() {
        let messages = vec![
            user_message("first"),
            user_message("second"),
            user_message("third"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 1);
        let ContentPart::Text { text } = &normalized[0].content[0] else {
            panic!("expected text content");
        };
        assert_eq!(text, "first\n\nsecond\n\nthird");
    }

    #[test]
    fn normalize_keeps_tools_grouped_after_their_assistant() {
        // Assistant emits two tool calls; both tool results follow. Both must
        // pass through because they match parts of the same preceding assistant.
        let messages = vec![
            user_message("do two things"),
            assistant_message_with_content(vec![
                ContentPart::ToolUse {
                    tool_call_id: "call_a".into(),
                    tool_name: "bash_exec".into(),
                    arguments: serde_json::json!({}),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_b".into(),
                    tool_name: "bash_exec".into(),
                    arguments: serde_json::json!({}),
                },
            ]),
            tool_message("call_a"),
            tool_message("call_b"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 4);
        assert_eq!(normalized[2].role, MessageRole::Tool);
        assert_eq!(normalized[3].role, MessageRole::Tool);
    }

    fn assistant_message_with_thinking(text: &str) -> AssistantMessage {
        assistant_message_with_content(vec![ContentPart::Thinking {
            text: text.to_string(),
            signature: None,
        }])
    }

    fn system_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            id: format!("msg-system-{}", text.len()),
            session_id: "session".to_string(),
            role: MessageRole::System,
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            created_at: 0,
            provider_metadata: None,
        }
    }

    fn make_session(automation_name: Option<&str>) -> crate::assistant::types::AssistantSession {
        crate::assistant::types::AssistantSession {
            id: "session".to_string(),
            kind: SessionKind::BackgroundJob,
            title: None,
            context: SessionContext {
                automation_name: automation_name.map(str::to_string),
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn normalize_strips_orphan_tool_use_from_assistant_in_final_pass() {
        // Assistant has two tool_calls but only one tool_result follows; the
        // orphan tool_use must be stripped while the assistant text and the
        // matched tool_use survive.
        let messages = vec![
            user_message("do two things"),
            assistant_message_with_content(vec![
                ContentPart::Text {
                    text: "writing".into(),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_a".into(),
                    tool_name: "bash_exec".into(),
                    arguments: serde_json::json!({}),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_b".into(),
                    tool_name: "bash_exec".into(),
                    arguments: serde_json::json!({}),
                },
            ]),
            tool_message("call_a"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[1].role, MessageRole::Assistant);
        let tool_ids: Vec<&str> = normalized[1]
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_ids, vec!["call_a"]);
        assert!(normalized[1]
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Text { text } if text == "writing")));
        assert_eq!(normalized[2].role, MessageRole::Tool);
    }

    #[test]
    fn normalize_drops_assistant_left_empty_by_orphan_strip() {
        // Assistant has only an orphan tool_use (no matching tool_result and no
        // text). After the final-invariant pass strips the orphan, the
        // assistant is empty and must be dropped entirely.
        let messages = vec![
            user_message("kick off"),
            assistant_message_with_content(vec![ContentPart::ToolUse {
                tool_call_id: "call_orphan".into(),
                tool_name: "bash_exec".into(),
                arguments: serde_json::json!({}),
            }]),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].role, MessageRole::User);
    }

    #[test]
    fn normalize_merges_consecutive_assistant_text_messages() {
        // Two assistant rows with no tool calls and no intervening user must
        // collapse into a single assistant message, content concatenated in
        // order.
        let messages = vec![
            user_message("hi"),
            assistant_message_with_text("first chunk"),
            assistant_message_with_text("second chunk"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[1].role, MessageRole::Assistant);
        let texts: Vec<&str> = normalized[1]
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["first chunk", "second chunk"]);
    }

    #[test]
    fn normalize_preserves_system_message_pass_through() {
        let messages = vec![
            system_message("system prelude"),
            user_message("hi"),
            assistant_message_with_text("hello"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].role, MessageRole::System);
        let ContentPart::Text { text } = &normalized[0].content[0] else {
            panic!("expected text content");
        };
        assert_eq!(text, "system prelude");
    }

    #[test]
    fn normalize_drops_tool_message_without_tool_result_part() {
        // Tool row with no ToolResult content (only stray text) — must be
        // dropped because we can't anchor it to an assistant tool_call_id.
        let bogus_tool = AssistantMessage {
            id: "msg-tool-bogus".to_string(),
            session_id: "session".to_string(),
            role: MessageRole::Tool,
            content: vec![ContentPart::Text {
                text: "not a tool result".into(),
            }],
            created_at: 0,
            provider_metadata: None,
        };
        let messages = vec![
            user_message("do work"),
            assistant_message_with_content(vec![ContentPart::ToolUse {
                tool_call_id: "call_a".into(),
                tool_name: "bash_exec".into(),
                arguments: serde_json::json!({}),
            }]),
            bogus_tool,
            tool_message("call_a"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        // The bogus tool row is dropped; the real tool result still attaches
        // to its assistant.
        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[2].role, MessageRole::Tool);
        assert!(matches!(
            &normalized[2].content[0],
            ContentPart::ToolResult { tool_call_id, .. } if tool_call_id == "call_a"
        ));
    }

    #[test]
    fn normalize_drops_assistant_with_only_empty_thinking_placeholder() {
        // `assistant_content_is_empty` treats a Thinking part as empty only
        // when its text is empty (matching the helper's matrix: a thinking
        // placeholder ingested before any reasoning text streamed in).
        // Such an assistant placeholder must be dropped so the surrounding
        // user messages can merge.
        let messages = vec![
            user_message("hi"),
            assistant_message_with_thinking(""),
            user_message("are you there?"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].role, MessageRole::User);
        let ContentPart::Text { text } = &normalized[0].content[0] else {
            panic!("expected text content");
        };
        assert!(text.contains("hi"));
        assert!(text.contains("are you there?"));
    }

    #[test]
    fn normalize_preserves_assistant_with_non_empty_thinking() {
        // A Thinking part with real reasoning text is NOT empty per
        // `assistant_content_is_empty` (text.is_empty() is the only check).
        // The assistant message must pass through so providers that require
        // the reasoning_content blob (e.g. LiteLLM-fronted OpenAI with
        // thinking enabled) still see it.
        let messages = vec![
            user_message("hi"),
            assistant_message_with_thinking("internal monologue"),
            user_message("are you there?"),
        ];

        let normalized = normalize_history_for_provider(&messages);

        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].role, MessageRole::User);
        assert_eq!(normalized[1].role, MessageRole::Assistant);
        assert!(matches!(
            &normalized[1].content[0],
            ContentPart::Thinking { text, .. } if text == "internal monologue"
        ));
        assert_eq!(normalized[2].role, MessageRole::User);
    }

    #[test]
    fn normalize_handles_empty_input() {
        let normalized = normalize_history_for_provider(&[]);
        assert!(normalized.is_empty());
    }

    #[test]
    fn assistant_content_is_empty_matrix() {
        // Empty content list → empty.
        assert!(assistant_content_is_empty(&[]));
        // Only-text with empty string → empty.
        assert!(assistant_content_is_empty(&[ContentPart::Text {
            text: String::new()
        }]));
        // Only-thinking with empty string → empty.
        assert!(assistant_content_is_empty(&[ContentPart::Thinking {
            text: String::new(),
            signature: None,
        }]));
        // Empty text + empty thinking → still empty.
        assert!(assistant_content_is_empty(&[
            ContentPart::Text {
                text: String::new()
            },
            ContentPart::Thinking {
                text: String::new(),
                signature: None,
            },
        ]));
        // Non-empty text → not empty.
        assert!(!assistant_content_is_empty(&[ContentPart::Text {
            text: "hi".into()
        }]));
        // Non-empty thinking → not empty.
        assert!(!assistant_content_is_empty(&[ContentPart::Thinking {
            text: "ponder".into(),
            signature: None,
        }]));
        // ToolUse alone is always non-empty.
        assert!(!assistant_content_is_empty(&[ContentPart::ToolUse {
            tool_call_id: "call_a".into(),
            tool_name: "bash_exec".into(),
            arguments: serde_json::json!({}),
        }]));
        // ToolResult alone is always non-empty (irrelevant for assistant but
        // the helper still treats it as not-empty).
        assert!(!assistant_content_is_empty(&[ContentPart::ToolResult {
            tool_call_id: "call_a".into(),
            payload: serde_json::json!({}),
            started_at: None,
            completed_at: None,
        }]));
    }

    #[test]
    fn append_text_with_separator_appends_blank_line_between_existing_and_new_text() {
        let mut target = vec![ContentPart::Text {
            text: "first".to_string(),
        }];
        let source = vec![ContentPart::Text {
            text: "second".to_string(),
        }];

        append_text_with_separator(&mut target, &source);

        assert_eq!(target.len(), 1);
        let ContentPart::Text { text } = &target[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "first\n\nsecond");
    }

    #[test]
    fn append_text_with_separator_pushes_text_part_when_target_has_no_text() {
        // Target has only a ToolUse part; the appended text must be added as
        // a new Text part, not merged into anything.
        let mut target = vec![ContentPart::ToolUse {
            tool_call_id: "call_a".into(),
            tool_name: "bash_exec".into(),
            arguments: serde_json::json!({}),
        }];
        let source = vec![ContentPart::Text {
            text: "hello".to_string(),
        }];

        append_text_with_separator(&mut target, &source);

        assert_eq!(target.len(), 2);
        assert!(matches!(&target[1], ContentPart::Text { text } if text == "hello"));
    }

    #[test]
    fn append_text_with_separator_skips_when_source_has_no_text() {
        // Source has only a non-text part → target untouched.
        let mut target = vec![ContentPart::Text {
            text: "keep me".to_string(),
        }];
        let source = vec![ContentPart::ToolUse {
            tool_call_id: "call_a".into(),
            tool_name: "bash_exec".into(),
            arguments: serde_json::json!({}),
        }];

        append_text_with_separator(&mut target, &source);

        assert_eq!(target.len(), 1);
        let ContentPart::Text { text } = &target[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "keep me");
    }

    #[test]
    fn append_text_with_separator_replaces_blank_target_text_without_separator() {
        // Target's last text part is empty — appended text must replace it
        // without a leading "\n\n" separator.
        let mut target = vec![ContentPart::Text {
            text: String::new(),
        }];
        let source = vec![ContentPart::Text {
            text: "fresh".to_string(),
        }];

        append_text_with_separator(&mut target, &source);

        assert_eq!(target.len(), 1);
        let ContentPart::Text { text } = &target[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "fresh");
    }

    #[test]
    fn append_text_with_separator_joins_multiple_source_text_parts_with_newline() {
        let mut target = vec![ContentPart::Text {
            text: "header".to_string(),
        }];
        let source = vec![
            ContentPart::Text {
                text: "line1".to_string(),
            },
            ContentPart::Text {
                text: "line2".to_string(),
            },
        ];

        append_text_with_separator(&mut target, &source);

        assert_eq!(target.len(), 1);
        let ContentPart::Text { text } = &target[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "header\n\nline1\nline2");
    }

    #[test]
    fn stream_parts_preserve_interleaved_order() {
        // think → answer → tool → think → answer, the way an interleaved
        // reasoning model streams it. Order and per-block signatures must hold.
        let mut parts: Vec<ContentPart> = Vec::new();
        push_thinking_delta(&mut parts, "let me think");
        set_thinking_signature(&mut parts, "sig-1".into());
        push_text_delta(&mut parts, "Here is ");
        push_text_delta(&mut parts, "the plan.");
        parts.push(ContentPart::ToolUse {
            tool_call_id: "call_1".into(),
            tool_name: "bash".into(),
            arguments: serde_json::json!({}),
        });
        push_thinking_delta(&mut parts, "tool worked");
        set_thinking_signature(&mut parts, "sig-2".into());
        push_text_delta(&mut parts, "Done.");

        assert_eq!(parts.len(), 5);
        assert!(matches!(
            &parts[0],
            ContentPart::Thinking { text, signature: Some(s) }
                if text == "let me think" && s == "sig-1"
        ));
        assert!(matches!(&parts[1], ContentPart::Text { text } if text == "Here is the plan."));
        assert!(matches!(&parts[2], ContentPart::ToolUse { .. }));
        assert!(matches!(
            &parts[3],
            ContentPart::Thinking { text, signature: Some(s) }
                if text == "tool worked" && s == "sig-2"
        ));
        assert!(matches!(&parts[4], ContentPart::Text { text } if text == "Done."));
    }

    #[test]
    fn stream_parts_coalesce_consecutive_deltas() {
        // Consecutive same-kind deltas merge into one part; a signed thinking
        // block is sealed so the next thinking delta opens a fresh block.
        let mut parts: Vec<ContentPart> = Vec::new();
        push_thinking_delta(&mut parts, "a");
        push_thinking_delta(&mut parts, "b");
        set_thinking_signature(&mut parts, "sig".into());
        push_thinking_delta(&mut parts, "c"); // new block (previous sealed)

        assert_eq!(parts.len(), 2);
        assert!(matches!(
            &parts[0],
            ContentPart::Thinking { text, signature: Some(s) } if text == "ab" && s == "sig"
        ));
        assert!(matches!(
            &parts[1],
            ContentPart::Thinking { text, signature: None } if text == "c"
        ));
    }

    #[test]
    fn build_trigger_message_scheduled_emits_user_marker_with_automation_name() {
        let session = make_session(Some("Health Monitor"));
        let message = build_trigger_message(&session, &RunTrigger::Scheduled)
            .expect("scheduled returns Some");

        assert_eq!(message.role, MessageRole::User);
        let ContentPart::Text { text } = &message.content[0] else {
            panic!("expected text");
        };
        assert!(text.contains("--- New scheduled run at "));
        assert!(text.contains("Health Monitor"));
        assert!(text.contains("Tool outputs above this marker are from previous runs."));
    }

    #[test]
    fn build_trigger_message_scheduled_falls_back_when_automation_name_missing() {
        // No automation_name → fallback string "automation" appears in the
        // marker.
        let session = make_session(None);
        let message = build_trigger_message(&session, &RunTrigger::Scheduled)
            .expect("scheduled returns Some");

        let ContentPart::Text { text } = &message.content[0] else {
            panic!("expected text");
        };
        assert!(text.contains("--- New scheduled run at "));
        assert!(text.contains("Run the next scheduled pass for automation now."));
    }

    #[test]
    fn build_trigger_message_manual_automation_emits_manual_marker() {
        let session = make_session(Some("Daily Report"));
        let message = build_trigger_message(&session, &RunTrigger::ManualAutomation)
            .expect("manual returns Some");

        assert_eq!(message.role, MessageRole::User);
        let ContentPart::Text { text } = &message.content[0] else {
            panic!("expected text");
        };
        assert!(text.contains("--- Manual run at "));
        assert!(text.contains("Daily Report"));
        assert!(text.contains("Run the automation Daily Report now"));
    }

    #[test]
    fn build_trigger_message_returns_none_for_user_driven_triggers() {
        let session = make_session(Some("ignored"));
        for trigger in &[
            RunTrigger::UserMessage,
            RunTrigger::Retry,
            RunTrigger::InterAgentCall,
            RunTrigger::WorkspaceTask,
        ] {
            assert!(
                build_trigger_message(&session, trigger).is_none(),
                "expected None for {:?}",
                trigger
            );
        }
    }
    // --- R-comp.6: only context-limit failures get the compaction preamble ----

    #[test]
    fn non_context_limit_failures_are_not_rewritten() {
        let attempt = compaction::CompactionAttempt::Failed("summariser died".to_string());
        let message = "401 Unauthorized: invalid API key";
        assert_eq!(
            failure_message_with_compaction_context(message, &attempt),
            message,
            "an auth failure has nothing to do with compaction"
        );
    }

    #[test]
    fn context_limit_failures_name_the_compaction_error() {
        let attempt = compaction::CompactionAttempt::Failed("summariser died".to_string());
        let text = failure_message_with_compaction_context("prompt is too long", &attempt);
        assert!(text.contains("summariser died"), "{text}");
        assert!(text.contains("prompt is too long"), "{text}");
    }

    #[test]
    fn returned_provider_error_uses_rewritten_context_limit_message() {
        let attempt = compaction::CompactionAttempt::Failed("summariser died".to_string());
        let provider_message = "provider request failed: prompt is too long".to_string();
        let failure = failure_message_with_compaction_context(&provider_message, &attempt);

        let error = provider_error_with_compaction_context(
            ProviderError::RequestFailed("prompt is too long".to_string()),
            &provider_message,
            failure,
        );

        let text = error.to_string();
        assert!(text.contains("summariser died"), "{text}");
        assert!(text.contains("prompt is too long"), "{text}");
    }

    #[test]
    fn returned_provider_error_preserves_unrewritten_errors() {
        let attempt = compaction::CompactionAttempt::Failed("summariser died".to_string());
        let provider_message = "provider request failed: 401 Unauthorized".to_string();
        let failure = failure_message_with_compaction_context(&provider_message, &attempt);

        let error = provider_error_with_compaction_context(
            ProviderError::RequestFailed("401 Unauthorized".to_string()),
            &provider_message,
            failure,
        );

        assert_eq!(error.to_string(), provider_message);
    }
}

/// Normalize persisted history into a provider-safe message sequence.
///
/// Persistence keeps every assistant placeholder, tool result, and user message —
/// that's good for the UI and for debugging, but a strict provider (e.g. vLLM
/// via litellm) will reject a request whose history has an unmatched `tool`
/// role or breaks the `assistant -> tool -> ...` pairing. Two classes of
/// corruption are common:
///
/// 1. Mid-stream hangups before `[DONE]`: the assistant row gets saved with
///    only the text content, but tool result rows for the (in-memory) tool
///    calls were already persisted just below.
/// 2. Stacked user typing while runs fail: multiple `user` rows pile up with
///    no assistant turn between them, plus the scheduled run-boundary marker
///    appends yet another `user` row on top.
///
/// This pass leaves the DB untouched and instead reshapes the provider view:
/// drop empty assistant placeholders, drop tool rows whose `tool_call_id`
/// isn't present in the preceding assistant's `ToolUse` parts, and merge
/// consecutive same-role messages (text concatenated with a blank-line
/// separator).
/// Rewrites `mcp__clai__*` tool names recorded by pre-normalization Claude
/// Code runs back to their canonical form. Without this, history replayed
/// after a provider switch teaches the model to mimic the qualified names
/// (which only existed inside the CLI's view of our local MCP server).
/// `persist_tool_use` strips the qualifier for new rows; this covers rows
/// that were persisted before that fix.
fn normalized_assistant_parts(parts: &[ContentPart]) -> Vec<ContentPart> {
    parts
        .iter()
        .cloned()
        .map(|part| match part {
            ContentPart::ToolUse {
                tool_call_id,
                tool_name,
                arguments,
            } => {
                let canonical = tools::strip_local_mcp_qualifier(&tool_name).to_string();
                ContentPart::ToolUse {
                    tool_call_id,
                    tool_name: canonical,
                    arguments,
                }
            }
            other => other,
        })
        .collect()
}

/// Replace `ContentPart::Image` parts with a short text placeholder so a
/// non-vision provider still receives coherent (if lossy) history. Used only
/// when `connection_supports_images` is false for the active connection.
/// Read each `ContentPart::Image` in `messages` from disk and return its bytes
/// base64-encoded, keyed by image id. Best-effort: a missing/unreadable file is
/// logged and skipped (the adapter then omits that image block but still sends
/// the surrounding text). Only called for image-capable API connections.
async fn resolve_request_images(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
    messages: &[ProviderInputMessage],
) -> std::collections::HashMap<String, crate::assistant::types::ResolvedImage> {
    use base64::Engine as _;
    let mut out: std::collections::HashMap<String, crate::assistant::types::ResolvedImage> =
        std::collections::HashMap::new();
    let Some(root) = session
        .context
        .workspace_id
        .as_deref()
        .and_then(|id| deps.app.state::<AppState>().workspace_root(id))
    else {
        return out;
    };
    for message in messages {
        for part in &message.content {
            if let ContentPart::Image {
                id,
                path,
                media_type,
                ..
            } = part
            {
                if out.contains_key(id) {
                    continue;
                }
                // Defense-in-depth: the send path base64-encodes these bytes
                // to the model, so the read is the real exfiltration sink.
                // Resolve through symlinks and refuse anything that escapes the
                // store (a non-store ref, a missing file, or a symlinked entry
                // pre-planted to point outside `.clai/images/`).
                let Some(full) = crate::assistant::image_store::resolve_store_path(&root, path)
                else {
                    tracing::warn!(image_id = %id, %path, "Rejecting non-store/escaping image path; skipping");
                    continue;
                };
                match tokio::fs::read(&full).await {
                    Ok(bytes) => {
                        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        out.insert(
                            id.clone(),
                            crate::assistant::types::ResolvedImage {
                                media_type: media_type.clone(),
                                data_base64,
                            },
                        );
                    }
                    Err(error) => tracing::warn!(
                        image_id = %id,
                        path = %full.display(),
                        %error,
                        "Failed to read image file for provider request; skipping"
                    ),
                }
            }
        }
    }
    out
}

fn strip_unsupported_images(messages: Vec<ProviderInputMessage>) -> Vec<ProviderInputMessage> {
    messages
        .into_iter()
        .map(|mut message| {
            for part in &mut message.content {
                if matches!(part, ContentPart::Image { .. }) {
                    *part = ContentPart::Text {
                        text: "[image omitted]".to_string(),
                    };
                }
            }
            message
        })
        .collect()
}

#[allow(
    clippy::cognitive_complexity,
    reason = "lint debt: cognitive complexity 41 against a budget of 25"
)]
#[expect(
    clippy::too_many_lines,
    reason = "lint debt: 123 lines against a 100-line budget; split it, do not raise the budget"
)]
fn normalize_history_for_provider(messages: &[AssistantMessage]) -> Vec<ProviderInputMessage> {
    let mut out: Vec<ProviderInputMessage> = Vec::new();

    for msg in messages {
        match msg.role {
            MessageRole::Assistant => {
                if assistant_content_is_empty(&msg.content) {
                    tracing::debug!(
                        message_id = %msg.id,
                        "Dropping empty assistant placeholder from provider history"
                    );
                    continue;
                }
                if let Some(last) = out.last_mut() {
                    if last.role == MessageRole::Assistant {
                        last.content
                            .extend(normalized_assistant_parts(&msg.content));
                        continue;
                    }
                }
                out.push(ProviderInputMessage {
                    role: MessageRole::Assistant,
                    content: normalized_assistant_parts(&msg.content),
                });
            }
            MessageRole::Tool => {
                // Find the assistant message that owns this tool_call_id
                // ANYWHERE in `out`, not just at the tail. This handles the
                // common-but-recently-painful case where a user typed a
                // message between an assistant's tool_calls and the tool
                // results landing — previously the predecessor check
                // failed, the tool result got dropped, and the next
                // provider call rejected the orphan tool_calls.
                //
                // When we do find the owning assistant, we insert the tool
                // message right after it (and after any already-emitted
                // sibling tool messages for the same group). The user
                // messages that were between get pushed past the tool
                // block — i.e., the assistant→tool invariant is preserved
                // at the cost of slightly delaying the user's interjection
                // in the provider's view. Same outcome a proper "queue
                // user messages while the LLM is running" UI would have
                // produced.
                let Some(target_tool_call_id) = msg.content.iter().find_map(|part| match part {
                    ContentPart::ToolResult { tool_call_id, .. } => Some(tool_call_id.clone()),
                    _ => None,
                }) else {
                    tracing::warn!(
                        message_id = %msg.id,
                        "Dropping tool message with no ToolResult part"
                    );
                    continue;
                };

                let owning_assistant_idx = out.iter().rposition(|m| {
                    m.role == MessageRole::Assistant
                        && m.content.iter().any(|p| {
                            matches!(p, ContentPart::ToolUse { tool_call_id: id, .. }
                                if id == &target_tool_call_id)
                        })
                });

                let Some(idx) = owning_assistant_idx else {
                    tracing::warn!(
                        session_id = %msg.session_id,
                        message_id = %msg.id,
                        tool_call_id = %target_tool_call_id,
                        "Dropping orphan tool message (no assistant in history claims this tool_call_id)"
                    );
                    continue;
                };

                // Skip past any tool messages already attached to this
                // assistant's group, so a multi-tool-call group accumulates
                // its results in order.
                let mut insert_at = idx + 1;
                while insert_at < out.len() && out[insert_at].role == MessageRole::Tool {
                    insert_at += 1;
                }
                if insert_at == out.len() {
                    out.push(ProviderInputMessage {
                        role: MessageRole::Tool,
                        content: msg.content.clone(),
                    });
                } else {
                    out.insert(
                        insert_at,
                        ProviderInputMessage {
                            role: MessageRole::Tool,
                            content: msg.content.clone(),
                        },
                    );
                }
            }
            MessageRole::User => {
                if let Some(last) = out.last_mut() {
                    if last.role == MessageRole::User {
                        append_text_with_separator(&mut last.content, &msg.content);
                        continue;
                    }
                }
                out.push(ProviderInputMessage {
                    role: MessageRole::User,
                    content: msg.content.clone(),
                });
            }
            MessageRole::System => {
                out.push(ProviderInputMessage {
                    role: MessageRole::System,
                    content: msg.content.clone(),
                });
            }
        }
    }

    // Final invariant pass: every `tool_use` in an assistant message must
    // have a matching tool_result later in `out`. If it doesn't, the tool
    // result was either never persisted (engine crash between exec and
    // write, or the user cancelled the run mid-tool) or got dropped by an
    // earlier normalizer iteration. Either way, sending it to a strict
    // provider triggers "tool_call_ids did not have response messages: X"
    // and stalls the whole conversation. Strip the orphan tool_use parts
    // so the assistant either continues with its text content or — if it
    // had only tool_calls — gets dropped as an empty assistant
    // placeholder by the standard pass below.
    let assistant_indices: Vec<usize> = out
        .iter()
        .enumerate()
        .filter_map(|(idx, m)| (m.role == MessageRole::Assistant).then_some(idx))
        .collect();
    for assistant_idx in assistant_indices {
        let tool_ids_in_assistant: Vec<String> = out[assistant_idx]
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::ToolUse { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect();
        for tool_call_id in tool_ids_in_assistant {
            let has_response = out.iter().skip(assistant_idx + 1).any(|m| {
                m.role == MessageRole::Tool
                    && m.content.iter().any(|p| {
                        matches!(p, ContentPart::ToolResult { tool_call_id: id, .. } if id == &tool_call_id)
                    })
            });
            if !has_response {
                tracing::warn!(
                    tool_call_id = %tool_call_id,
                    "Stripping orphan tool_use from assistant message (no tool_result in history)"
                );
                out[assistant_idx].content.retain(|p| {
                    !matches!(p, ContentPart::ToolUse { tool_call_id: id, .. } if id == &tool_call_id)
                });
            }
        }
    }

    // Drop assistant messages that became empty after stripping (or that
    // were empty placeholders ingested from a crashed run).
    out.retain(|m| !(m.role == MessageRole::Assistant && assistant_content_is_empty(&m.content)));

    out
}

/// Append a streamed text delta, coalescing into the trailing Text part so a
/// run of deltas becomes one part. A non-text part (thinking/tool) in between
/// starts a fresh text run, preserving interleaving.
fn push_text_delta(parts: &mut Vec<ContentPart>, text: &str) {
    if let Some(ContentPart::Text { text: existing }) = parts.last_mut() {
        existing.push_str(text);
    } else {
        parts.push(ContentPart::Text {
            text: text.to_string(),
        });
    }
}

/// Append a streamed thinking delta. Coalesces into the trailing thinking part
/// only while it is still open (unsigned); a signed block is sealed, so the
/// next delta starts a new thinking block — which keeps each block paired with
/// its own signature.
fn push_thinking_delta(parts: &mut Vec<ContentPart>, text: &str) {
    if let Some(ContentPart::Thinking {
        text: existing,
        signature: None,
    }) = parts.last_mut()
    {
        existing.push_str(text);
    } else {
        parts.push(ContentPart::Thinking {
            text: text.to_string(),
            signature: None,
        });
    }
}

/// Bind a signature to the open (unsigned) trailing thinking block. If the last
/// part isn't an open thinking block, the signature has nothing to attach to.
fn set_thinking_signature(parts: &mut [ContentPart], signature: String) {
    if let Some(ContentPart::Thinking {
        signature: slot @ None,
        ..
    }) = parts.last_mut()
    {
        *slot = Some(signature);
    } else {
        tracing::warn!("signature delta with no open thinking block; ignored");
    }
}

fn assistant_content_is_empty(content: &[ContentPart]) -> bool {
    content.iter().all(|part| match part {
        ContentPart::Text { text } => text.is_empty(),
        // A message with only thinking and no other content is
        // semantically empty from the user/provider standpoint —
        // there's no answer or action to take.
        ContentPart::Thinking { text, .. } => text.is_empty(),
        ContentPart::ToolUse { .. } | ContentPart::ToolResult { .. } => false,
        // An image is real content, not an empty turn.
        ContentPart::Image { .. } => false,
    })
}

fn append_text_with_separator(target: &mut Vec<ContentPart>, source: &[ContentPart]) {
    let source_text: String = source
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    if source_text.is_empty() {
        return;
    }

    if let Some(last_text) = target.iter_mut().rev().find_map(|part| match part {
        ContentPart::Text { text } => Some(text),
        _ => None,
    }) {
        if !last_text.is_empty() {
            last_text.push_str("\n\n");
        }
        last_text.push_str(&source_text);
    } else {
        target.push(ContentPart::Text { text: source_text });
    }
}

/// How the provider stream ended. Every arm of the stream loop breaks with one
/// of these instead of returning, so message finalization has exactly one call
/// site and cannot be skipped by a new early return (R3.2).
#[derive(Debug)]
enum StreamExit {
    /// The stream ended normally, or the provider hung up after emitting
    /// content. The turn continues into tool execution.
    Completed,
    /// The user pressed Stop.
    Cancelled,
    /// The provider reported an error inside the stream (`ProviderError`).
    ProviderReported { message: String },
    /// The stream itself yielded a transport error.
    StreamFailed { message: String },
}

/// Whether the assistant row should be written back from what was streamed.
///
/// Keep it whenever anything was streamed — a cancelled or failed turn still
/// showed the user real text, and that text is only in memory until this
/// returns true. The exception is a turn that produced nothing at all: the row
/// already holds the single empty `Text` that `final_content_parts` would
/// write, so finalizing it persists nothing and the
/// `AssistantMessageCompleted` is pure noise. (Some of those rows are then
/// deleted outright by `discard_unanswered_run_input`, but only on the failure
/// exits and only on the first iteration; the rest stay as empty placeholders,
/// which the chat list already hides.) `local_agent.rs` finalizes even this
/// case — harmless there, and the difference is invisible either way.
///
/// A normally-completed turn is always finalized, empty or not, because the
/// schema expects exactly one content part and a tool-only turn legitimately
/// has no text.
fn exit_keeps_streamed_message(exit: &StreamExit, produced_no_content: bool) -> bool {
    match exit {
        StreamExit::Completed => true,
        StreamExit::Cancelled
        | StreamExit::ProviderReported { .. }
        | StreamExit::StreamFailed { .. } => !produced_no_content,
    }
}

/// Content to persist for the assistant message, in arrival order, guaranteed
/// non-empty so the row never holds zero parts.
fn final_content_parts(parts: Vec<ContentPart>) -> Vec<ContentPart> {
    if parts.is_empty() {
        return vec![ContentPart::Text {
            text: String::new(),
        }];
    }
    parts
}

/// The user-facing failure text for a run that died on the provider's context
/// limit: name compaction as the reason the turn was not rescued. Any other
/// provider error passes through untouched.
fn failure_message_with_compaction_context(
    provider_message: &str,
    attempt: &compaction::CompactionAttempt,
) -> String {
    if is_context_limit_error(provider_message) {
        compaction::context_limit_failure_message("The request", provider_message, attempt)
    } else {
        provider_message.to_string()
    }
}

fn provider_error_with_compaction_context(
    error: ProviderError,
    provider_message: &str,
    failure_message: String,
) -> ProviderError {
    if failure_message == provider_message {
        error
    } else {
        ProviderError::RequestFailed(failure_message)
    }
}

use futures::StreamExt;
use std::collections::HashSet;
use tauri::AppHandle;
use tauri::Manager;
use thiserror::Error;

use crate::assistant::compaction;
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::providers;
use crate::assistant::providers::types::ProviderError;
use crate::assistant::repository;
use crate::assistant::repository::{CreateMessageParams, CreateRunParams, CreateToolCallParams};
use crate::assistant::runtime;
use crate::assistant::tools::{self, ToolExecutionContext};
use crate::assistant::types::{
    AssistantMessage, CompactionTrigger, CompletionRequest, ContentPart, MessageRole,
    ProviderEvent, ProviderInputMessage, RunId, RunStatus, RunTrigger, RunUsage, SessionId,
    ToolCallStatus, ToolInvocationDraft,
};
use crate::db::DbPool;
use crate::AppState;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct AssistantDeps {
    pub pool: DbPool,
    pub app: AppHandle,
}

#[derive(Debug, Clone)]
pub struct RunTurnInput {
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub trigger: RunTrigger,
    pub connection_id: String,
    pub cancel_token: CancellationToken,
    pub inter_agent_call_depth: Option<u32>,
    /// Id of the user message that triggered this run (Some only for the
    /// direct send path). If the run fails before the provider produces
    /// anything, this message is discarded — see
    /// `discard_unanswered_run_input`.
    pub trigger_message_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum AssistantEngineError {
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("provider not configured: {0}")]
    ProviderNotConfigured(String),
    #[error("run connection mismatch for run {0}")]
    RunConnectionMismatch(String),
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
    #[error("persistence error: {0}")]
    Persistence(String),
}

impl From<String> for AssistantEngineError {
    fn from(s: String) -> Self {
        AssistantEngineError::Persistence(s)
    }
}

pub async fn run_session_turn(
    deps: &AssistantDeps,
    input: RunTurnInput,
) -> Result<(), AssistantEngineError> {
    // Load session
    let session = repository::get_session(&deps.pool, &input.session_id)
        .await?
        .ok_or_else(|| AssistantEngineError::SessionNotFound(input.session_id.clone()))?;

    let app_state = deps.app.try_state::<AppState>();
    // Load provider connection from app config.
    let connection = app_state
        .as_ref()
        .and_then(|state| {
            state
                .config_manager
                .lock()
                .ok()?
                .get_provider_connection(&input.connection_id)
        })
        .ok_or_else(|| AssistantEngineError::ProviderNotConfigured(input.connection_id.clone()))?;
    let workspace_root = match session.context.agent_workspace_id.as_deref() {
        Some(workspace_id) => {
            let root = app_state
                .as_ref()
                .and_then(|state| state.workspace_root(workspace_id));
            if root.is_none() {
                return Err(AssistantEngineError::Persistence(format!(
                    "workspace {} no longer exists or failed to load",
                    workspace_id
                )));
            }
            root
        }
        None => None,
    };

    if providers::is_cli_provider(&connection.provider_id) {
        return crate::assistant::local_agent::run_session_turn(deps, input).await;
    }

    // Get or create the run
    let run_id = match &input.run_id {
        Some(id) => {
            let existing_run = repository::get_run(&deps.pool, id).await?.ok_or_else(|| {
                AssistantEngineError::Persistence(format!("run not found: {}", id))
            })?;
            if existing_run.connection_id != input.connection_id {
                return Err(AssistantEngineError::RunConnectionMismatch(id.clone()));
            }
            id.clone()
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
            run.id
        }
    };

    // Transition run to Running
    let run = repository::update_run_status(&deps.pool, &run_id, RunStatus::Running, None).await?;
    let _ = emit_event(
        &deps.app,
        &session,
        Some(&run_id),
        AssistantUiEvent::RunStarted { run },
    );

    if input.cancel_token.is_cancelled() {
        cancel_run(deps, &session, &run_id, usage_none(), None).await?;
        return Ok(());
    }

    // Resolve adapter
    let adapter = providers::resolve_adapter(&connection.provider_id)?;

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
    // Run-scoped filesystem grants accepted via fs_request_grant. Cloned
    // into each per-tool-call ToolExecutionContext so accepting a grant
    // mid-run is visible to subsequent tool calls in the same run.
    let session_grants = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let session_allowed_command_prefixes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let session_blocked_command_prefixes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut usage: Option<RunUsage> = None;

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
    );

    // Persist the trigger message as a run boundary marker so the LLM can see
    // where one run ends and the next begins. Without this, the LLM sees old
    // tool results from prior runs and may skip re-running tools.
    if let Some(trigger_content) = build_trigger_message(&session, &input.trigger) {
        let boundary_msg = repository::create_message(
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
            &session,
            Some(&run_id),
            AssistantUiEvent::MessageCreated {
                message: boundary_msg,
            },
        );
    }

    // No iteration cap: the agent runs as long as the LLM keeps emitting
    // tool calls. The cancel token is the only stop — surfaced as the
    // "Stop" button in the UI and any explicit cancel from upstream.
    // Provider-side context-length limits will surface as errors and
    // exit via fail_run; this loop itself imposes no ceiling.
    let mut iteration: usize = 0;
    let mut retried_after_context_compaction = false;
    loop {
        if input.cancel_token.is_cancelled() {
            cancel_run(deps, &session, &run_id, usage.as_ref(), None).await?;
            return Ok(());
        }

        // Load fresh message history each iteration (includes the persisted trigger).
        // Normalize before sending: drop empty assistant placeholders, drop tool
        // messages whose tool_call_id has no matching tool_use in the preceding
        // assistant turn, and merge consecutive same-role messages. The DB stays
        // the source of truth; this only shapes what the provider sees so a
        // mid-stream hangup or stacked user typing can't poison subsequent runs.
        let mut messages = repository::list_messages(&deps.pool, &session.id).await?;
        let current_provider_history =
            compaction::provider_history_messages(&deps.pool, &session.id, &messages).await?;
        if compaction::should_auto_compact(&current_provider_history, &tool_defs) {
            match compaction::compact_session_history(
                &deps.pool,
                &session,
                &connection,
                CompactionTrigger::Automatic,
                Some(&run_id),
                false,
            )
            .await
            {
                Ok(Some(outcome)) => {
                    let _ = emit_event(
                        &deps.app,
                        &session,
                        Some(&run_id),
                        AssistantUiEvent::SessionCompacted {
                            compaction: outcome.compaction,
                            summary_message: outcome.summary_message,
                        },
                    );
                    messages = repository::list_messages(&deps.pool, &session.id).await?;
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    session_id = %session.id,
                    run_id = %run_id,
                    error = %error,
                    "Automatic assistant history compaction failed"
                ),
            }
        }
        let message_ids_in_snapshot: HashSet<&str> =
            messages.iter().map(|message| message.id.as_str()).collect();
        let queued_message_ids_in_request: Vec<String> =
            repository::list_pending_queued_message_ids(&deps.pool, &session.id)
                .await?
                .into_iter()
                .filter(|id| message_ids_in_snapshot.contains(id.as_str()))
                .collect();
        let provider_history =
            compaction::provider_history_messages(&deps.pool, &session.id, &messages).await?;
        let normalized = normalize_history_for_provider(&provider_history);

        let mut provider_messages = vec![system_message.clone()];
        provider_messages.extend(normalized);

        let request = CompletionRequest {
            run_id: run_id.clone(),
            session_id: session.id.clone(),
            model_id: connection.model_id.clone(),
            messages: provider_messages,
            tools: tool_defs.clone(),
            temperature: None,
            max_output_tokens: None,
        };

        // Call the provider
        let stream_result = adapter.stream_completion(&connection, request).await;

        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                if !retried_after_context_compaction
                    && compaction::is_context_limit_error(&e.to_string())
                {
                    retried_after_context_compaction = true;
                    match compaction::compact_session_history(
                        &deps.pool,
                        &session,
                        &connection,
                        CompactionTrigger::ErrorRecovery,
                        Some(&run_id),
                        true,
                    )
                    .await
                    {
                        Ok(Some(outcome)) => {
                            let _ = emit_event(
                                &deps.app,
                                &session,
                                Some(&run_id),
                                AssistantUiEvent::SessionCompacted {
                                    compaction: outcome.compaction,
                                    summary_message: outcome.summary_message,
                                },
                            );
                            continue;
                        }
                        Ok(None) => tracing::warn!(
                            session_id = %session.id,
                            run_id = %run_id,
                            "Context-limit recovery found no compactable assistant history"
                        ),
                        Err(error) => tracing::warn!(
                            session_id = %session.id,
                            run_id = %run_id,
                            error = %error,
                            "Context-limit recovery compaction failed"
                        ),
                    }
                }
                fail_run(deps, &session, &run_id, usage.as_ref(), &e.to_string()).await?;
                // First iteration: the provider rejected the request outright
                // (connection/auth/limit), so the user's message never reached
                // the LLM — drop it. Later iterations already produced content.
                if iteration == 0 {
                    discard_unanswered_run_input(
                        deps,
                        &session,
                        &run_id,
                        input.trigger_message_id.as_deref(),
                        None,
                    )
                    .await;
                }
                return Err(e.into());
            }
        };

        if let Err(e) = repository::mark_queued_messages_delivered(
            &deps.pool,
            &session.id,
            &run_id,
            &queued_message_ids_in_request,
        )
        .await
        {
            fail_run(deps, &session, &run_id, usage.as_ref(), &e).await?;
            return Err(AssistantEngineError::Persistence(e));
        }
        if !queued_message_ids_in_request.is_empty() {
            // The queued messages just became part of this run's request —
            // tell the FE so their "Queued" chips clear.
            let _ = emit_event(
                &deps.app,
                &session,
                Some(&run_id),
                AssistantUiEvent::QueuedMessagesDelivered {
                    message_ids: queued_message_ids_in_request.clone(),
                },
            );
        }

        // Create assistant message placeholder
        let assistant_message = repository::create_message(
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
            &session,
            Some(&run_id),
            AssistantUiEvent::MessageCreated {
                message: assistant_message.clone(),
            },
        );

        // Consume the stream. `content_parts` grows in the order events
        // arrive so the assistant message preserves the model's real
        // text↔thinking↔tool interleaving (e.g. think → call tool → think →
        // answer) instead of hoisting all reasoning to the top. `tool_calls`
        // is tracked separately because the execution loop below needs it.
        let mut content_parts: Vec<ContentPart> = Vec::new();
        let mut tool_calls: Vec<ToolInvocationDraft> = Vec::new();

        loop {
            match tokio::select! {
                _ = input.cancel_token.cancelled() => None,
                next = stream.next() => next,
            } {
                None if input.cancel_token.is_cancelled() => {
                    cancel_run(
                        deps,
                        &session,
                        &run_id,
                        usage.as_ref(),
                        Some(&assistant_message.id),
                    )
                    .await?;
                    return Ok(());
                }
                Some(Ok(event)) => match event {
                    ProviderEvent::MessageStart => {}
                    ProviderEvent::TextDelta { text } => {
                        push_text_delta(&mut content_parts, &text);
                        let _ = emit_event(
                            &deps.app,
                            &session,
                            Some(&run_id),
                            AssistantUiEvent::AssistantDelta {
                                message_id: assistant_message.id.clone(),
                                text,
                            },
                        );
                    }
                    ProviderEvent::ThinkingDelta { text } => {
                        push_thinking_delta(&mut content_parts, &text);
                        let _ = emit_event(
                            &deps.app,
                            &session,
                            Some(&run_id),
                            AssistantUiEvent::AssistantThinkingDelta {
                                message_id: assistant_message.id.clone(),
                                text,
                            },
                        );
                    }
                    ProviderEvent::ThinkingSignature { signature } => {
                        // Anthropic sends the signature in its own event after a
                        // thinking block's text; bind it to that block so it can
                        // be replayed verbatim. A later thinking_delta opens a new
                        // block, so each block keeps its own signature.
                        set_thinking_signature(&mut content_parts, signature);
                    }
                    ProviderEvent::Usage { usage: u } => {
                        usage = Some(u);
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
                        fail_run(deps, &session, &run_id, usage.as_ref(), &message).await?;
                        // Mid-stream failure before anything came back (some
                        // providers report limits this way): the user got no
                        // answer at all, so drop their message and the empty
                        // assistant placeholder created above.
                        if iteration == 0 && run_produced_no_content(&content_parts) {
                            discard_unanswered_run_input(
                                deps,
                                &session,
                                &run_id,
                                input.trigger_message_id.as_deref(),
                                Some(&assistant_message.id),
                            )
                            .await;
                        }
                        return Ok(());
                    }
                },
                Some(Err(e)) => {
                    let error_msg = e.to_string();
                    fail_run(deps, &session, &run_id, usage.as_ref(), &error_msg).await?;
                    if iteration == 0 && run_produced_no_content(&content_parts) {
                        discard_unanswered_run_input(
                            deps,
                            &session,
                            &run_id,
                            input.trigger_message_id.as_deref(),
                            Some(&assistant_message.id),
                        )
                        .await;
                    }
                    return Err(AssistantEngineError::Provider(
                        ProviderError::RequestFailed(error_msg),
                    ));
                }
                None => break,
            }
        }

        // Finalize the assistant message from whatever we accumulated, even if
        // the provider never emitted MessageComplete. This prevents the orphan-
        // tool case: tool_calls captured via `finish_reason: tool_calls` but
        // [DONE] never arriving, leaving the assistant row with empty content
        // while tool result rows get persisted just below.
        //
        // `content_parts` is already in arrival order. Guarantee non-empty so
        // the assistant row never persists with zero content.
        let mut final_content = content_parts;
        if final_content.is_empty() {
            final_content.push(ContentPart::Text {
                text: String::new(),
            });
        }

        let updated_message =
            repository::update_message_content(&deps.pool, &assistant_message.id, &final_content)
                .await?;

        let _ = emit_event(
            &deps.app,
            &session,
            Some(&run_id),
            AssistantUiEvent::AssistantMessageCompleted {
                message: updated_message,
            },
        );

        // If no tool calls, we're done
        if tool_calls.is_empty() {
            break;
        }

        tracing::info!(
            "Assistant engine: executing {} tool call(s) (iteration {})",
            tool_calls.len(),
            iteration + 1
        );

        // Execute each tool call
        for tc in &tool_calls {
            if input.cancel_token.is_cancelled() {
                cancel_run(deps, &session, &run_id, usage.as_ref(), None).await?;
                return Ok(());
            }

            // Persist tool call
            let tool_invocation = repository::create_tool_call(
                &deps.pool,
                CreateToolCallParams {
                    id: tc.tool_call_id.clone(),
                    run_id: run_id.clone(),
                    session_id: session.id.clone(),
                    tool_name: tc.tool_name.clone(),
                    params: tc.params.clone(),
                    status: ToolCallStatus::Running,
                },
            )
            .await?;

            let _ = emit_event(
                &deps.app,
                &session,
                Some(&run_id),
                AssistantUiEvent::ToolCallStarted {
                    tool_call: tool_invocation.clone(),
                },
            );

            // Execute the tool
            let tool_context = ToolExecutionContext {
                session_id: session.id.clone(),
                run_id: run_id.clone(),
                tool_call_id: Some(tc.tool_call_id.clone()),
                cancel_token: input.cancel_token.clone(),
                workspace_id: session.context.workspace_id.clone(),
                space_id: session.context.space_id.clone(),
                room_id: session.context.room_id.clone(),
                mcp_server_ids: session.context.mcp_server_ids.clone(),
                agent_workspace_id: session.context.agent_workspace_id.clone(),
                workspace_root: workspace_root.clone(),
                automation_id: session.context.automation_id.clone(),
                workspace_agents: session.context.workspace_agents.clone(),
                inter_agent_call_depth: input.inter_agent_call_depth,
                execution: session.context.execution.clone(),
                notices: notices.clone(),
                session_grants: session_grants.clone(),
                session_allowed_command_prefixes: session_allowed_command_prefixes.clone(),
                session_blocked_command_prefixes: session_blocked_command_prefixes.clone(),
            };
            let tool_result = tokio::select! {
                _ = input.cancel_token.cancelled() => {
                    cancel_run(deps, &session, &run_id, usage.as_ref(), None).await?;
                    return Ok(());
                }
                result = tools::execute_tool(
                    deps,
                    &tool_context,
                    &tc.tool_name,
                    tc.params.clone()
                ) => result,
            };

            match tool_result {
                Ok(result) => {
                    let updated = repository::update_tool_call(
                        &deps.pool,
                        &tool_invocation.id,
                        ToolCallStatus::Completed,
                        Some(&result),
                        None,
                    )
                    .await?;
                    let tool_started_at = updated.started_at;
                    let tool_completed_at = updated.completed_at;

                    let _ = emit_event(
                        &deps.app,
                        &session,
                        Some(&run_id),
                        AssistantUiEvent::ToolCallCompleted { tool_call: updated },
                    );

                    // Persist tool result as a message
                    let result_message = repository::create_message(
                        &deps.pool,
                        CreateMessageParams {
                            session_id: session.id.clone(),
                            role: MessageRole::Tool,
                            content: vec![ContentPart::ToolResult {
                                tool_call_id: tc.tool_call_id.clone(),
                                payload: result,
                                started_at: Some(tool_started_at),
                                completed_at: tool_completed_at,
                            }],
                            provider_metadata: None,
                        },
                    )
                    .await?;

                    let _ = emit_event(
                        &deps.app,
                        &session,
                        Some(&run_id),
                        AssistantUiEvent::MessageCreated {
                            message: result_message,
                        },
                    );
                }
                Err(error) => {
                    let updated = repository::update_tool_call(
                        &deps.pool,
                        &tool_invocation.id,
                        ToolCallStatus::Failed,
                        None,
                        Some(&error),
                    )
                    .await?;
                    let tool_started_at = updated.started_at;
                    let tool_completed_at = updated.completed_at;

                    let _ = emit_event(
                        &deps.app,
                        &session,
                        Some(&run_id),
                        AssistantUiEvent::ToolCallFailed { tool_call: updated },
                    );

                    // Still persist the error as a tool result so the API can see it
                    let error_payload = serde_json::json!({"error": error});
                    repository::create_message(
                        &deps.pool,
                        CreateMessageParams {
                            session_id: session.id.clone(),
                            role: MessageRole::Tool,
                            content: vec![ContentPart::ToolResult {
                                tool_call_id: tc.tool_call_id.clone(),
                                payload: error_payload,
                                started_at: Some(tool_started_at),
                                completed_at: tool_completed_at,
                            }],
                            provider_metadata: None,
                        },
                    )
                    .await?;
                }
            }
        }

        // Continue loop — will call API again with tool results in message history.
        iteration += 1;
    }

    // Complete the run — check for policy notices
    let tool_context = ToolExecutionContext {
        session_id: session.id.clone(),
        run_id: run_id.clone(),
        tool_call_id: None,
        cancel_token: input.cancel_token.clone(),
        workspace_id: session.context.workspace_id.clone(),
        space_id: session.context.space_id.clone(),
        room_id: session.context.room_id.clone(),
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
    let notices = tool_context.take_notices();
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

fn usage_none() -> Option<&'static RunUsage> {
    None
}

/// Resolve the live agent description for the session's owning agent.
///
/// Returns the user-set description plus assembled skill content read fresh
/// from disk on each call. `None` when the session has no workspace/agent
/// binding, AppState is unavailable, or the workspace/agent has been
/// deleted — all "no agent instructions" scenarios.
pub(crate) fn live_agent_description(
    app: &AppHandle,
    context: &crate::assistant::types::SessionContext,
) -> Option<String> {
    let workspace_id = context.workspace_id.as_deref()?;
    let agent_id = context.automation_id.as_deref()?;
    let state = app.try_state::<AppState>()?;
    crate::commands::workspace::workspace_agent_runtime_description(
        state.inner(),
        workspace_id,
        agent_id,
    )
}

/// Build the system prompt for the assistant.
///
/// `agent_description` is the live-computed seed (user-set description plus
/// resolved skill content) for the agent owning this session. It is NOT
/// persisted on the session — callers re-derive it at turn start so toggling
/// a skill or editing a description is immediately visible to the model.
/// Pass `None` only for sessions that have no associated agent (e.g. tests,
/// or sessions whose underlying agent has been deleted).
pub(crate) fn build_system_prompt(
    context: &crate::assistant::types::SessionContext,
    agent_description: Option<&str>,
    tool_defs: &[crate::assistant::types::ToolDefinition],
    trigger: &RunTrigger,
) -> ProviderInputMessage {
    let tool_names: Vec<&str> = tool_defs.iter().map(|t| t.name.as_str()).collect();
    let current_datetime = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string();

    let mut prompt = String::from(
        "You are CLAI, a workspace assistant and multi-agent orchestration tool built into a desktop app. \
         You help users inspect available capabilities, choose the right tools for the job, update the workspace, \
         and explain outcomes clearly.\n\n",
    );

    prompt.push_str(&format!(
        "Current local date and time: `{}`.\n\n",
        current_datetime
    ));

    // Role-identity callout: if this session belongs to the workspace's
    // default manager AND there are non-manager members in the team,
    // put a short "you are the manager, here are your members" header
    // ABOVE the tool list. Without this, LLMs frequently hallucinate
    // their own toolset on the first turn ("I don't have reviewer
    // agents available") despite the team being listed lower down in
    // the prompt. Placing role identity first keeps the model from
    // framing itself as a solo assistant.
    let is_manager_session = context
        .workspace_agents
        .iter()
        .any(|a| a.is_default && Some(a.id.as_str()) == context.automation_id.as_deref());
    let member_agents: Vec<&crate::assistant::types::WorkspaceAgentSummary> = context
        .workspace_agents
        .iter()
        .filter(|a| !a.is_default)
        .collect();
    if is_manager_session {
        if !member_agents.is_empty() {
            prompt.push_str(
                "## Your Role\n\
                 You are the **manager** of this workspace. The user talks to you; you decide how the work gets done. \
                 You have member agents available for delegation via `workspace_assignTask` — prefer delegating specialized work to them over doing it yourself, then poll `workspace_getTaskResult` for the outcome. \
                 The roster below is your team; you do not need to call `workspace_listAgents` to confirm it.\n\n\
                 Member agents you can delegate to:\n",
            );
            for agent in &member_agents {
                let summary = agent
                    .description
                    .as_deref()
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or("(no description)");
                prompt.push_str(&format!(
                    "- **{}** ({}): {}\n",
                    agent.display_name, agent.role, summary
                ));
            }
            prompt.push('\n');
        } else {
            prompt.push_str(
                "## Your Role\n\
                 You are the **manager** (and only agent) of this workspace. The user talks to you; you decide how the work gets done. \
                 There are no member agents to delegate to, but the task tools still work: you can assign a task to *yourself* — your own workspace agent id, visible via `workspace_listAgents` — to run another instance of you in the background.\n\n",
            );
        }

        // Delegation mechanics — skipped when this manager session *is* a
        // task worker (a self-assigned task): the Task Worker Context block
        // below carries the don't-spawn-chains guidance instead, and
        // advertising fan-out there would invite recursive task spawning.
        if !matches!(trigger, RunTrigger::WorkspaceTask) {
            prompt.push_str(
                "### How tasks run\n\
                 - `workspace_assignTask` is asynchronous: it returns a task id immediately and the task runs in its own separate session, in parallel with you. Keep working or reply to the user while it runs; poll `workspace_getTaskResult` when you need the outcome.\n\
                 - Tasks run concurrently with no per-agent limit. Fan out independent subtasks freely — several tasks for the *same* agent at once is fine.\n\
                 - Assigning a task to yourself is the supported way to push long or background work out of this conversation while you stay responsive.\n\
                 - A task worker does NOT see this conversation. Write self-contained instructions: include the goal, the relevant file paths, and any context it needs.\n\
                 - All tasks share this workspace's directory. Partition parallel work so concurrent tasks don't write the same files.\n\
                 - If you expect to collect a result in a later run (your run can end before the task finishes), record the task id in memory (e.g. `.clai/memory/state.md`) so a future run can poll it.\n\n",
            );
        }
    }

    if matches!(trigger, RunTrigger::WorkspaceTask) {
        prompt.push_str(
            "## Task Worker Context\n\
             This session is a background task worker: you were spawned via `workspace_assignTask` to complete one bounded task, running in parallel with the agent that assigned it (possibly another instance of yourself). \
             Your final assistant message is captured as the task's result summary — make it a concise, self-contained outcome. \
             Do not assign further tasks from here unless your instructions explicitly require fan-out; never create task chains or loops.\n\n",
        );
    }

    if !tool_names.is_empty() {
        prompt.push_str("You have the following tools available:\n");
        for td in tool_defs {
            prompt.push_str(&format!("- `{}`: {}\n", td.name, td.description));
        }
        prompt.push('\n');
    }

    // Tool usage guidance
    prompt.push_str(
        "## Tool Usage Guidelines\n\
         - First inspect what is available in this session and choose the smallest set of tools needed.\n\
         - Use the configured MCP tools available in this session for domain-specific work.\n\
         - Use exposed CLAI tools such as `fs_list`, `fs_read`, `fs_write`, `fs_glob`, and `bash_exec` only when those local execution capabilities are available in this session.\n\
         - Prior tool outputs in the conversation may be stale. Treat them as historical context, not guaranteed current state.\n\
         - Evaluate whether prior tool outputs are still fresh enough for the current decision. When information can expire or change over time (for example issues, alerts, metrics, repo state, or external system status), re-run the relevant tools if freshness matters.\n\
         - Chat is the default output channel. Use normal assistant replies for status, findings, and conclusions.\n\
         - When looking for code, files, or prior work, ALWAYS search your workspace first — `fs_glob`/`fs_list` from the workspace root, or a `bash_exec` search scoped to it — before searching other granted paths. The workspace holds your own artifacts and earlier outputs; prefer a match found there over an equivalent one found elsewhere.\n\
         - Durable outputs belong in the workspace: write them there with `fs_write` so they persist after the run as user-visible artifacts.\n\
         - Before creating a new durable artifact, search the workspace for an existing relevant one and update it rather than creating a duplicate.\n\
         - Be concise and direct in your responses. Prefer concrete actions and evidence over vague summaries.\n",
    );

    // Transport-drop recovery for grant/response-blocking tools. The local MCP
    // transport can drop an in-flight call (surfaced to the model as
    // "transport dropped mid-call; response for tool <name> was lost"). For a
    // tool that blocks on a user grant or answer, the outcome is then unknown,
    // so the model must re-ask rather than assume an answer or proceed. The
    // backend treats the re-asked call as superseding the orphaned one (the
    // stale approval/question card is replaced in place), so no UI caveats are
    // needed here. Scoped to sessions that actually expose such a tool;
    // ordinary read/write tools, which can be retried without side effects,
    // need no special handling.
    let has_interactive_tool = tool_names
        .iter()
        .any(|n| matches!(*n, "ask_user" | "bash_exec" | "fs_request_grant"));
    if has_interactive_tool {
        prompt.push_str(
            "\n## Interactive Tool Reliability\n\
             A tool call can occasionally fail with a transport error such as `MCP server \"clai\" transport dropped mid-call; response for tool <name> was lost`. This means CLAI lost the in-flight call before its result reached you, so the call's outcome is UNKNOWN — it may or may not have run.\n\
             - This matters specifically for tools that block on a user grant or response — `ask_user`, and approval-gated `bash_exec` / `fs_request_grant`. When one of these drops mid-call, the user may never have answered, or they answered but the decision was lost.\n\
             - When it happens, re-issue the SAME interactive call once. CLAI replaces the lost prompt with the fresh one in the app, so the user simply answers the new prompt. Do NOT assume the lost call was approved, denied, or answered, and do NOT proceed past it.\n\
             - Apply this only to active CLAI human waits. If a user-input, command-approval, or filesystem-grant prompt expires or is denied, do not invent convoluted workarounds to bypass it. If the permission or answer is required, stop and explain what is blocked; retry only after a transport drop where the outcome is unknown.\n\
             - For non-interactive tools (reads, searches, writes), a transport drop needs no special handling — just retry normally if you still need the result.\n",
        );
    }

    if context.space_id.is_some() || !context.mcp_server_ids.is_empty() {
        prompt.push_str(
            "- This tab already carries session-specific context and capabilities. \
             Use the MCP tools attached to this session when they are relevant.\n",
        );
    }

    prompt.push_str("\n## Run Mode\n");
    match trigger {
        RunTrigger::Scheduled | RunTrigger::ManualAutomation => {
            prompt.push_str(
                "This is an autonomous automation pass. You should inspect the current state, \
                 decide what needs to be refreshed, and communicate the result clearly.\n",
            );
        }
        RunTrigger::InterAgentCall => {
            prompt.push_str(
                "This is a synchronous inter-agent call. The caller is waiting for your response.\n",
            );
        }
        RunTrigger::WorkspaceTask => {
            prompt.push_str(
                "This is a workspace-local task assigned by the manager agent. Complete the bounded task using the current workspace context, then report the result clearly. If blocked by missing capability, context, permission, or runtime failure, start with `BLOCKED:` and state the specific manager or user action needed. If you specifically need user feedback or approval, start with `NEEDS_USER_INPUT:` and state the decision needed.\n",
            );
        }
        RunTrigger::UserMessage | RunTrigger::Retry => {
            prompt.push_str(
                "This is a user-driven run. Prioritize the user's latest message and use prior context only as support.\n",
            );
        }
    }

    if !context.workspace_agents.is_empty() {
        prompt.push_str("\n## Workspace Team\n");
        prompt.push_str(
            "This workspace has assigned agents. The default manager agent receives user messages and is responsible for routing work inside this workspace.\n\
             Use this roster as workspace-local context. Do not assume agents outside this list are available for collaboration.\n\
             When task delegation tools are available, assign bounded tasks only to assigned workspace agents. Tasks run asynchronously and in parallel, each in its own session. Use `ask_user` when work is blocked on user feedback, approval, or missing information. If delegation tools are not available in this session, explain which assigned agent should handle the work and what is blocked.\n\n",
        );
        prompt.push_str("Assigned workspace agents:\n");
        for agent in &context.workspace_agents {
            let role = if agent.is_default {
                "manager"
            } else {
                agent.role.as_str()
            };
            if let Some(description) = agent
                .description
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                prompt.push_str(&format!(
                    "- {} ({}) — {}\n",
                    agent.display_name, role, description
                ));
            } else {
                prompt.push_str(&format!("- {} ({})\n", agent.display_name, role));
            }
        }
    }

    if let Some(automation_name) = context.automation_name.as_deref() {
        prompt.push_str("\n## Automation Context\n");
        prompt.push_str(&format!(
            "This session belongs to the automation `{}`.\n",
            automation_name
        ));
        prompt.push_str(
            "Your assistant text is visible to the user in chat. Treat chat as the primary way to communicate progress and outcomes.\n\
             Save durable outputs as files in the workspace (via `fs_write`) so they surface as artifacts.\n\
             For routine scheduled passes, a concise chat update is often sufficient.\n\
             Prefer updating existing visuals over recreating duplicate panels when the topic is unchanged.\n",
        );

        if let Some(description) = agent_description.filter(|s| !s.is_empty()) {
            prompt.push_str("\nAgent instructions:\n");
            prompt.push_str(description);
            prompt.push('\n');
        }
    }

    if matches!(trigger, RunTrigger::InterAgentCall) {
        prompt.push_str(
            "\n## Inter-Agent Call\n\
             You have been called by another agent. The latest user message includes the request parameters, the required JSON output schema, and a trace ID.\n\
             Return exactly one JSON object that matches the output schema.\n\
             Do not wrap the response in markdown fences.\n\
             Do not ask follow-up questions because you will not receive answers.\n",
        );
    }

    if let Some(workspace_id) = context.agent_workspace_id.as_deref() {
        prompt.push_str("\n## Local Execution Capabilities\n");
        prompt.push_str(&format!(
            "- Your workspace (id `{workspace_id}`) is your read_write home and your default shell working directory (run `pwd` for its path). Do your work here: write documents, scratch files, code, and durable outputs to the workspace unless the user points you elsewhere. Files in the workspace are shown to the user as **artifacts** in the CLAI app, so treat them as user-facing. The workspace is shared with other agents in the *same* workspace.\n",
        ));

        if context.execution.filesystem.extra_paths.is_empty() {
            prompt.push_str("- Additional path grants: none\n");
        } else {
            prompt.push_str("- Additional path grants:\n");
            for grant in &context.execution.filesystem.extra_paths {
                let access = match grant.access {
                    crate::config::FilesystemPathAccess::ReadOnly => "read_only",
                    crate::config::FilesystemPathAccess::ReadWrite => "read_write",
                };
                prompt.push_str(&format!("  - `{}` ({})\n", grant.path, access));
            }
        }

        let shell_mode = match context.execution.shell.mode {
            crate::config::ShellAccessMode::Off => "off",
            crate::config::ShellAccessMode::Restricted => "restricted",
            crate::config::ShellAccessMode::Full => "full",
        };
        prompt.push_str(&format!("- Shell mode: {}\n", shell_mode));
        let network_status = match context.execution.sandbox.network {
            crate::config::SandboxNetworkConfig::Enabled => "network allowed",
            crate::config::SandboxNetworkConfig::Disabled => "network disabled",
        };
        let sandbox_status = if cfg!(target_os = "linux") {
            let session_bus_status = match context.execution.sandbox.session_bus {
                crate::config::SandboxSessionBusConfig::Allow => "session bus available",
                crate::config::SandboxSessionBusConfig::Deny => "session bus blocked",
            };
            format!(
                "sandboxed shell on Linux through bubblewrap when `bash_exec` is available ({}, {})",
                network_status, session_bus_status
            )
        } else if cfg!(target_os = "macos") {
            format!(
                "sandboxed shell on macOS through Seatbelt/sandbox-exec when `bash_exec` is available ({})",
                network_status
            )
        } else {
            "host shell — sandbox not yet available on this platform".to_string()
        };
        prompt.push_str(&format!("- Shell sandbox: {}\n", sandbox_status));
        if cfg!(target_os = "linux")
            && matches!(
                context.execution.sandbox.session_bus,
                crate::config::SandboxSessionBusConfig::Allow
            )
        {
            prompt.push_str(
                "- Session bus is available: tools that authenticate through libsecret (e.g. `gh`, `git-credential-libsecret`, `secret-tool`) can reach the host keyring directly. Use the host's existing auth instead of asking the user for tokens.\n",
            );
        }

        if !context.execution.shell.blocked_command_prefixes.is_empty() {
            prompt.push_str(&format!(
                "- Blocked command prefixes: {}\n",
                context.execution.shell.blocked_command_prefixes.join(", ")
            ));
        }

        if context.execution.shell.allowed_command_prefixes.is_empty() {
            let hint = match context.execution.shell.mode {
                crate::config::ShellAccessMode::Restricted => "none (no commands allowed)",
                _ => "any command not blocked",
            };
            prompt.push_str(&format!("- Allowed command prefixes: {}\n", hint));
        } else {
            prompt.push_str(&format!(
                "- Allowed command prefixes: {}\n",
                context.execution.shell.allowed_command_prefixes.join(", ")
            ));
        }

        if context.execution.web.enabled {
            prompt.push_str("- Web access: enabled (`web_search` and `web_fetch` available)\n");
        }

        prompt.push_str(
            "\n## Filesystem boundary\n\
             The path grants listed above are the ONLY locations you are authorized to read, write, or operate against. The `fs_*` tools enforce this in-process. On Linux and macOS, `bash_exec` also runs inside an OS sandbox that allows only the workspace, configured path grants, and required platform system files; if the sandbox is unavailable, `bash_exec` fails closed. On platforms where the shell sandbox is not implemented yet, `bash_exec` is labeled as a host shell and this paragraph remains the authorization boundary.\n\
             - Do not `cd`, redirect to, or pass paths outside the listed grants — not even via subshells, heredocs, scripts, or absolute paths.\n\
             - Do not invoke commands that touch paths outside the grants (no editing the user's other repos, no installing to global locations, no reading personal files like `~/.ssh`, etc.).\n\
             - If a task genuinely needs a path outside your current grants (e.g. `~/.ssh` for `git push`, `~/.config/gh` for the `gh` CLI), call `fs_request_grant({path, access, reason})` BEFORE attempting the work. The user can approve once (lasts this run), approve always (persists to agent settings), narrow the path, or deny. Request the narrowest path that satisfies the task — prefer `~/.config/gh` over `~/.config`, prefer a specific file over its parent directory. Prefer `read_only` unless writes are genuinely needed.\n\
             - If `fs_request_grant` is denied, do not retry the same path. Either request a narrower path, ask the user via `ask_user`, or stop and explain what was blocked.\n\
             - Do not silently extend your reach by other means. The grant flow is the only sanctioned escape valve.\n\
             - Default your writes to the workspace. Other grants (often `$HOME`) are commonly read_only, so writing there fails — check the access listed above first, and if you genuinely need to write to a read_only or ungranted path, `fs_request_grant` it rather than attempting the write and failing.\n\
             - Other CLAI workspaces exist on this machine but are intentionally isolated: you cannot see, list, or read them, and they will never appear in your grants. If the user asks you to work with a different workspace, ask them for its workspace id (the value they can read most easily in the CLAI app; you cannot enumerate workspaces). That workspace lives next to yours — same parent directory as your workspace, named with that id — so `fs_request_grant` that path (e.g. read_only first) to gain access.\n",
        );

        // Git/SSH etiquette guard. The agent shouldn't rewrite commit authorship
        // to bypass GitHub's email-privacy block: that destroys provenance and
        // does an end-run around a user-configured policy. Also note the SSH
        // /etc/ssh overlay so the agent doesn't have to discover the
        // -F /dev/null workaround experimentally.
        prompt.push_str(
            "\n## Git and SSH conventions inside the sandbox\n\
             - Never rewrite commit authorship. Do not run `git commit --amend --reset-author`, do not change `user.email` / `user.name` away from what the commit already has, and do not use the `--author=` flag to overwrite an existing author. If a push is rejected because of GitHub's email privacy (error `GH007`) or because the author's email is not allowed, STOP and escalate via `ask_user` with the exact failing email and the rejection reason. The user owns the choice of which email to publish.\n",
        );
        if cfg!(target_os = "linux") {
            prompt.push_str(
                "             - The Linux sandbox overlays an empty tmpfs at `/etc/ssh`, so OpenSSH only consults `~/.ssh/config` and its built-in defaults. You do not need `-F /dev/null` workarounds; if you see `Bad owner or permissions` from ssh, the cause is something else (likely an explicit `-F` pointing at an unreadable path).\n",
            );
        }

        prompt.push_str(
            "\n## Agent Memory\n\
             The `.clai/memory/` directory inside your workspace is pre-created and ready to use as durable memory across runs. These memory files are surfaced to the user in the CLAI app's **Memory** view, so write them to be human-readable, not just machine notes.\n\
             Memory has three layers, each with a distinct purpose:\n\n\
             ### 1. State — short-horizon working memory (`state.md`)\n\
             Current focus, pending actions, open questions, and outcome of the last run.\n\
             Replaced (not appended) every run — this is what you are thinking about *right now*.\n\n\
             ### 2. Knowledge — curated durable heuristics (`knowledge.md`)\n\
             Patterns, baselines, and lessons that remain valid across multiple runs.\n\
             Each entry should have a confidence tag and supporting evidence:\n\
             - `hypothesis` — observed once, not yet confirmed.\n\
             - `provisional` — observed multiple times or partially corroborated.\n\
             - `confirmed` — verified through repeated observation or explicit validation.\n\
             Remove or downgrade entries when contradicted by fresh evidence.\n\n\
             ### 3. Journal — append-only audit trail (`journal/{date}.md`)\n\
             One file per calendar day. Append timestamped entries for significant decisions, actions, and observations.\n\
             Journals are write-once: never edit past entries, only append new ones.\n\n\
             ### Additional files\n\
             - `index.md` — catalog of all memory files with one-line summaries. Read this first to decide what else to read. Update it whenever you create, rename, or delete a memory file.\n\
             - `checkpoints/<task>.md` — for multi-step work that spans several runs.\n\n\
             ### File conventions\n\
             - Each memory file should start with YAML frontmatter:\n\
             ```\n\
             ---\n\
             updated_at: YYYY-MM-DDTHH:MM:SS\n\
             summary: one-line description of this file's purpose\n\
             tags: [subsystem, topic]   # optional, cross-cutting labels for retrieval\n\
             ---\n\
             ```\n\
             - Keep each file under ~200 lines. When a file grows past this, prune stale entries or split into focused files.\n\
             - Replace outdated sections rather than appending indefinitely (except in `journal/`).\n\
             - Cross-link related memory with relative markdown links inside `.clai/memory/`, e.g. from `knowledge.md`: `[transport-drop fix](journal/2026-06-12.md)` or `[migration plan](checkpoints/db-migration.md)`. Link a knowledge entry to the evidence behind it (a journal day, a checkpoint, a PR url) so memory forms a navigable graph instead of disconnected notes.\n\
             - Use `tags` to group cross-cutting entries (e.g. `[mcp, concurrency]`) so related heuristics are easy to retrieve as `knowledge.md` grows.\n\
             - Tolerate broken links: a link whose target was pruned or not yet written is not an error — leave it or tidy it on your next pass, never let it block you.\n\n",
        );

        match trigger {
            RunTrigger::Scheduled | RunTrigger::ManualAutomation => {
                prompt.push_str(
                    "### Startup protocol (autonomous runs)\n\
                     1. Read `index.md` (if it exists) to see what memory is available.\n\
                     2. Read `state.md` to resume context from the previous run.\n\
                     3. Read `knowledge.md` only if the current task needs historical patterns.\n\
                     4. Do your work.\n\
                     5. Update `state.md` with current focus and outcome.\n\
                     6. Append a journal entry to `journal/{today}.md`.\n\
                     7. If you discovered a durable pattern, add it to `knowledge.md` with the appropriate confidence level.\n\
                     8. If any analysis you produced is worth preserving, file it as a checkpoint or knowledge entry — don't let valuable findings vanish into chat history.\n\
                     9. Update `index.md` if you created or removed any files.\n\
                     10. Prune stale entries: if a knowledge entry or checkpoint is no longer relevant, remove it.\n",
                );
            }
            RunTrigger::InterAgentCall
            | RunTrigger::WorkspaceTask
            | RunTrigger::UserMessage
            | RunTrigger::Retry => {
                prompt.push_str(
                    "### Memory in user-driven runs\n\
                     - Do NOT read memory unless the user's request specifically needs historical context.\n\
                     - Focus on the user's latest message. Memory is supporting context, not the starting point.\n\
                     - If the message seems to assume earlier context you don't have — it references prior decisions, files, or an ongoing task, but you see no conversation history — your session may have been reset (e.g. switching the underlying provider starts a fresh session). Before asking the user to repeat anything, read `.clai/memory/` (start with `index.md`, then `state.md` and any relevant file) to recover the lost context, then continue.\n\
                     - If you discover something worth remembering for future runs, write it to the appropriate memory file.\n\
                     - If the user's request produces a durable finding, consider filing it into knowledge or a checkpoint.\n",
                );
            }
        }

        prompt.push_str(
            "\n### Hierarchy of truth\n\
             When sources conflict, trust the higher-ranked source and update the lower one:\n\
             1. User instruction or human directive (highest)\n\
             2. Live tool output (fresh data from the current run)\n\
             3. Agent knowledge (`knowledge.md`)\n\
             4. Agent state (`state.md`, lowest)\n\n\
             ### Guardrails\n\
             - Treat memory as fallible working notes, not ground truth. Re-check time-sensitive facts with tools before acting.\n\
             - Do not store secrets in memory unless the operator explicitly configured a path for that purpose.\n\
             - Knowledge is not a dashboard — don't duplicate transient metrics there. State is not knowledge — don't put durable heuristics in `state.md`.\n",
        );

        prompt.push_str(
            "\n## Conversation History Database (read-only)\n\
             The complete conversation record of this workspace lives in a SQLite database at `.clai/data.sqlite` (relative to your workspace root): every message, run, and tool call with its full output, across all agents in this workspace — including detail long since compacted out of your context window. Use it when memory files don't have what you need and you must recover verbatim past work: the exact command that was run, the full text of an old error, what the user or a sibling agent said weeks ago.\n\
             - PREFERRED: use the `history_query` tool. It runs a single read-only SQL query against THIS workspace's `.clai/data.sqlite` and returns rows as JSON. It needs no approval because it is structurally incapable of writing or escaping, so it is your always-available way to recover context: if you ever find yourself missing earlier context — for example right after a compaction — query the record to recover it instead of asking the user to repeat anything.\n\
             - Discover the schema first rather than assuming it — it changes between app versions: `SELECT name FROM sqlite_master WHERE type='table'`, then `PRAGMA table_info(<table>)`. The key tables are `assistant_messages` (conversation, `content_json`), `assistant_tool_calls` (every tool invocation and its result), `assistant_runs`, and `workspace_tasks`.\n\
             - Keep queries narrow. Single rows can hold megabytes of tool output, so always SELECT specific columns, filter (`WHERE ... LIKE`, `json_extract`, time ranges on `created_at`) and page with `LIMIT`/`OFFSET`; never dump whole tables or `SELECT *` unbounded.\n\
             - `history_query` reads only THIS workspace's database. To read a DIFFERENT workspace's DB that you have been granted access to, fall back to the shell — STRICTLY READ-ONLY: open it only as `sqlite3 'file:<path>/.clai/data.sqlite?mode=ro'` (or python3's sqlite3 module with the same `mode=ro` URI). Never INSERT/UPDATE/DELETE, never VACUUM or ALTER, never open it without `mode=ro` — a write can corrupt the app's state.\n\
             - Your own in-flight run is in there too. This is a tool for finding *past* work — check memory files first, and reach for the database when you need the verbatim record.\n",
        );
    }

    ProviderInputMessage {
        role: MessageRole::System,
        content: vec![ContentPart::Text { text: prompt }],
    }
}

// Production helper fns (normalize_history_for_provider, build_trigger_message,
// fail_run, cancel_run) live below the test module for legacy reasons; the
// allow silences clippy's items-after-test-module lint without a noisy reflow.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::ContentPart;
    use crate::assistant::types::SessionContext;
    use crate::assistant::types::SessionKind;
    use crate::assistant::types::WorkspaceAgentSummary;
    use crate::config::{ExecutionCapabilityConfig, ShellAccessMode};

    #[test]
    fn build_system_prompt_includes_agent_memory_guidance_for_automations() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Agent Memory"));
        assert!(text.contains("`.clai/memory/`"));
        // Three-layer memory model
        assert!(text.contains("`state.md`"));
        assert!(text.contains("`knowledge.md`"));
        assert!(text.contains("`journal/{date}.md`"));
        assert!(text.contains("`index.md`"));
        // Knowledge confidence levels
        assert!(text.contains("`hypothesis`"));
        assert!(text.contains("`provisional`"));
        assert!(text.contains("`confirmed`"));
        // Schema convention
        assert!(text.contains("updated_at:"));
        assert!(text.contains("summary:"));
        // Cross-cutting tags + cross-linking conventions (OKF-inspired). These
        // are additive: the three-layer model and confidence tags above stay
        // authoritative; links/tags just make memory navigable and retrievable.
        assert!(text.contains("tags: [subsystem, topic]"));
        assert!(text.contains("Cross-link related memory"));
        assert!(text.contains("Tolerate broken links"));
        // Size hint
        assert!(text.contains("~200 lines"));
        // Hierarchy of truth
        assert!(text.contains("### Hierarchy of truth"));
        assert!(text.contains("User instruction or human directive"));
        assert!(text.contains("Live tool output"));
        // Guardrails
        assert!(text.contains("Treat memory as fallible working notes"));
        assert!(text.contains("Knowledge is not a dashboard"));
        // Autonomous startup protocol
        assert!(text.contains("### Startup protocol (autonomous runs)"));
        assert!(text.contains("Read `index.md`"));
        assert!(text.contains("Read `state.md`"));
    }

    #[test]
    fn build_system_prompt_tells_user_runs_to_recover_lost_context_from_memory() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // A turn whose session was reset (e.g. provider switch) should recover
        // context from memory rather than asking the user to repeat themselves.
        assert!(text.contains("### Memory in user-driven runs"));
        assert!(text.contains("your session may have been reset"));
        assert!(text.contains("`.clai/memory/`"));
        assert!(text.contains("Before asking the user to repeat anything"));
    }

    #[test]
    fn build_system_prompt_omits_agent_memory_guidance_without_workspace() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Agent Memory"));
    }

    #[test]
    fn build_system_prompt_makes_agent_self_aware_of_clai_workspace_model() {
        let context = SessionContext {
            agent_workspace_id: Some("ws-abc".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // Workspace = the write-home, named by id, files visible as artifacts.
        assert!(text.contains("id `ws-abc`"));
        assert!(text.contains("read_write home"));
        assert!(text.contains("shown to the user as **artifacts**"));
        // Default writes to the workspace (read-only-grant awareness).
        assert!(text.contains("Default your writes to the workspace"));
        // Cross-workspace isolation + how to reach another via its id.
        assert!(text.contains("Other CLAI workspaces exist"));
        assert!(text.contains("ask them for its workspace id"));
        // Memory is surfaced to the user in the app.
        assert!(text.contains("**Memory** view"));
    }

    #[test]
    fn build_system_prompt_documents_readonly_conversation_history_db() {
        let context = SessionContext {
            agent_workspace_id: Some("ws-abc".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // Where the record lives, the preferred always-available tool, and
        // the read-only mandate for the cross-workspace shell fallback.
        assert!(text.contains("## Conversation History Database (read-only)"));
        assert!(text.contains(".clai/data.sqlite"));
        assert!(text.contains("`history_query` tool"));
        assert!(text.contains("STRICTLY READ-ONLY"));
        assert!(text.contains("?mode=ro'"));
        // Schema discovery over hardcoded assumptions; narrow queries.
        assert!(text.contains("sqlite_master"));
        assert!(text.contains("Keep queries narrow"));
        // Memory stays the first stop; the DB is the verbatim fallback.
        assert!(text.contains("check memory files first"));
    }

    #[test]
    fn build_system_prompt_omits_history_db_without_workspace() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Conversation History Database"));
    }

    #[test]
    fn build_system_prompt_describes_shell_mode_alongside_memory_guidance() {
        let mut execution = ExecutionCapabilityConfig::default();
        execution.shell.mode = ShellAccessMode::Restricted;
        execution.shell.allowed_command_prefixes = vec!["rg".to_string(), "git status".to_string()];

        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            execution,
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("- Shell mode: restricted"));
        assert!(text.contains("- Allowed command prefixes: rg, git status"));
        assert!(text.contains("## Agent Memory"));
    }

    #[test]
    fn build_system_prompt_makes_chat_the_primary_output_channel() {
        let context = SessionContext {
            automation_name: Some("Health Monitor".to_string()),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("Chat is the default output channel."));
        assert!(
            text.contains("Treat chat as the primary way to communicate progress and outcomes.")
        );
        assert!(text
            .contains("For routine scheduled passes, a concise chat update is often sufficient."));
    }

    #[test]
    fn build_system_prompt_includes_current_datetime() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("Current local date and time: `"));
    }

    #[test]
    fn build_system_prompt_warns_that_prior_tool_results_may_be_stale() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("Prior tool outputs in the conversation may be stale."));
        assert!(text.contains(
            "Evaluate whether prior tool outputs are still fresh enough for the current decision."
        ));
        assert!(text.contains("re-run the relevant tools if freshness matters."));
    }

    #[test]
    fn build_system_prompt_adds_interactive_reliability_guidance_when_blocking_tool_present() {
        let context = SessionContext::default();
        let tools = [crate::assistant::types::ToolDefinition {
            name: "ask_user".to_string(),
            description: "desc".to_string(),
            input_schema: serde_json::json!({}),
        }];

        let message = build_system_prompt(&context, None, &tools, &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Interactive Tool Reliability"));
        assert!(text.contains("transport dropped mid-call"));
        assert!(text.contains("re-issue the SAME interactive call once"));
        assert!(text.contains("Apply this only to active CLAI human waits"));
        assert!(text.contains("do not invent convoluted workarounds"));
        // The backend supersedes the orphaned request when the model
        // re-asks, so the prompt must NOT push stale-card caveats (e.g.
        // telling the user to dismiss duplicates) onto the model.
        assert!(text.contains("replaces the lost prompt with the fresh one"));
        assert!(!text.contains("dismiss"));
    }

    #[test]
    fn build_system_prompt_omits_interactive_reliability_guidance_without_blocking_tool() {
        let context = SessionContext::default();
        let tools = [crate::assistant::types::ToolDefinition {
            name: "fs_read".to_string(),
            description: "desc".to_string(),
            input_schema: serde_json::json!({}),
        }];

        let message = build_system_prompt(&context, None, &tools, &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Interactive Tool Reliability"));
    }

    #[test]
    fn build_system_prompt_describes_autonomous_run_mode() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Run Mode"));
        assert!(text.contains("This is an autonomous automation pass."));
        assert!(text.contains("### Startup protocol (autonomous runs)"));
        assert!(text.contains("Read `index.md`"));
        assert!(text.contains("Read `state.md`"));
        assert!(text.contains("Append a journal entry"));
        assert!(text.contains("Prune stale entries"));
        assert!(text.contains("don't let valuable findings vanish into chat history"));
    }

    #[test]
    fn build_system_prompt_describes_user_driven_run_mode() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Run Mode"));
        assert!(text.contains("This is a user-driven run."));
        assert!(text.contains("### Memory in user-driven runs"));
        assert!(text.contains("Do NOT read memory unless"));
    }

    #[test]
    fn build_system_prompt_includes_workspace_agent_roster() {
        let context = SessionContext {
            workspace_agents: vec![
                WorkspaceAgentSummary {
                    id: "workspace-agent-manager".to_string(),
                    agent_definition_id: "manager-definition".to_string(),
                    display_name: "Manager".to_string(),
                    role: "manager".to_string(),
                    is_default: true,
                    description: Some("Coordinates workspace tasks.".to_string()),
                },
                WorkspaceAgentSummary {
                    id: "workspace-agent-reviewer".to_string(),
                    agent_definition_id: "reviewer-definition".to_string(),
                    display_name: "Code Reviewer".to_string(),
                    role: "member".to_string(),
                    is_default: false,
                    description: Some("Reviews source changes.".to_string()),
                },
            ],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Workspace Team"));
        assert!(text.contains("The default manager agent receives user messages"));
        assert!(text.contains("- Manager (manager)"));
        assert!(text.contains("- Code Reviewer (member)"));
        assert!(text.contains("Reviews source changes."));
    }

    fn manager_summary() -> WorkspaceAgentSummary {
        WorkspaceAgentSummary {
            id: "workspace-agent-manager".to_string(),
            agent_definition_id: "workspace-agent-manager".to_string(),
            display_name: "Manager".to_string(),
            role: "manager".to_string(),
            is_default: true,
            description: Some("Coordinates workspace tasks.".to_string()),
        }
    }

    fn member_summary() -> WorkspaceAgentSummary {
        WorkspaceAgentSummary {
            id: "workspace-agent-reviewer".to_string(),
            agent_definition_id: "workspace-agent-reviewer".to_string(),
            display_name: "Code Reviewer".to_string(),
            role: "member".to_string(),
            is_default: false,
            description: Some("Reviews source changes.".to_string()),
        }
    }

    #[test]
    fn build_system_prompt_documents_parallel_task_mechanics_for_manager() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-manager".to_string()),
            workspace_agents: vec![manager_summary(), member_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // Async + parallel semantics, fan-out, self-tasking, and the caveats
        // (shared workspace dir, self-contained instructions, durable ids).
        assert!(text.contains("### How tasks run"));
        assert!(text.contains("no per-agent limit"));
        assert!(text.contains("Assigning a task to yourself"));
        assert!(text.contains("does NOT see this conversation"));
        assert!(text.contains("Partition parallel work"));
        assert!(text.contains("record the task id in memory"));
    }

    #[test]
    fn build_system_prompt_offers_self_tasking_to_solo_manager() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-manager".to_string()),
            workspace_agents: vec![manager_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // No members: the role callout still renders, framed around
        // self-tasking as the background-work mechanism.
        assert!(text.contains("(and only agent)"));
        assert!(text.contains("assign a task to *yourself*"));
        assert!(text.contains("### How tasks run"));
    }

    #[test]
    fn build_system_prompt_marks_workspace_task_runs_as_workers() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-manager".to_string()),
            workspace_agents: vec![manager_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::WorkspaceTask);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Task Worker Context"));
        assert!(text.contains("result summary"));
        assert!(text.contains("never create task chains"));
        // A worker (even a self-tasked manager instance) must not be invited
        // to fan out further tasks.
        assert!(!text.contains("### How tasks run"));
    }

    #[test]
    fn build_system_prompt_hides_task_mechanics_from_members() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-reviewer".to_string()),
            workspace_agents: vec![manager_summary(), member_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Your Role"));
        assert!(!text.contains("### How tasks run"));
        assert!(!text.contains("## Task Worker Context"));
    }

    #[test]
    fn build_system_prompt_memory_guardrails_present_in_both_modes() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            ..Default::default()
        };

        for trigger in &[RunTrigger::Scheduled, RunTrigger::UserMessage] {
            let message = build_system_prompt(&context, None, &[], trigger);
            let text = match &message.content[0] {
                ContentPart::Text { text } => text,
                other => panic!("expected text content, got {:?}", other),
            };

            assert!(
                text.contains("### Guardrails"),
                "Missing guardrails for {:?}",
                trigger
            );
            assert!(
                text.contains("### Hierarchy of truth"),
                "Missing hierarchy of truth for {:?}",
                trigger
            );
            assert!(text.contains("Treat memory as fallible working notes"));
            assert!(text.contains("Do not store secrets in memory"));
            assert!(text.contains("Knowledge is not a dashboard"));
        }
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
                    tool_name: "fs_write".into(),
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
                tool_name: "fs_write".into(),
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
                    tool_name: "fs_write".into(),
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
                    tool_name: "fs_write".into(),
                    arguments: serde_json::json!({}),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_b".into(),
                    tool_name: "fs_write".into(),
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
                    tool_name: "fs_write".into(),
                    arguments: serde_json::json!({}),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_b".into(),
                    tool_name: "fs_write".into(),
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
                tool_name: "fs_write".into(),
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
                tool_name: "fs_write".into(),
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
            tool_name: "fs_write".into(),
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
            tool_name: "fs_write".into(),
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
            tool_name: "fs_write".into(),
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

/// Helper to mark a run as failed and emit the event.
/// True when a run's accumulated content amounts to nothing the user could
/// see: no text, no thinking, no tool calls. (The empty-Text placeholder a
/// message row is seeded with doesn't count.)
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
/// Errors are logged, not propagated — cleanup must never mask the original
/// failure.
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
        Ok(queued) => message_ids.extend(queued.into_iter().map(|q| q.message.id)),
        Err(error) => tracing::warn!(
            run_id,
            error,
            "Failed to list queued messages while discarding unanswered run input"
        ),
    }
    if let Some(id) = trigger_message_id {
        if !message_ids.iter().any(|existing| existing == id) {
            message_ids.push(id.to_string());
        }
    }
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

async fn fail_run(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
    error_msg: &str,
) -> Result<(), AssistantEngineError> {
    for tool_call in
        repository::fail_running_tool_calls_for_run(&deps.pool, run_id, error_msg).await?
    {
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::ToolCallFailed { tool_call },
        );
    }
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
    session: &crate::assistant::types::AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
    _message_id: Option<&str>,
) -> Result<(), AssistantEngineError> {
    for tool_call in
        repository::fail_running_tool_calls_for_run(&deps.pool, run_id, "Run cancelled").await?
    {
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::ToolCallFailed { tool_call },
        );
    }
    let run = repository::complete_run(&deps.pool, run_id, RunStatus::Cancelled, usage, None, &[])
        .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::RunCancelled { run },
    );
    runtime::unregister_run(run_id);
    Ok(())
}

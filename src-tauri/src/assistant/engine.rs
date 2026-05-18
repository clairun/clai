use futures::StreamExt;
use tauri::AppHandle;
use tauri::Manager;
use thiserror::Error;

use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::providers;
use crate::assistant::providers::types::ProviderError;
use crate::assistant::repository;
use crate::assistant::repository::{CreateMessageParams, CreateRunParams, CreateToolCallParams};
use crate::assistant::runtime;
use crate::assistant::tools::local::agent_workspace_root_for_id;
use crate::assistant::tools::{self, ToolExecutionContext};
use crate::assistant::types::{
    AssistantMessage, CompletionRequest, ContentPart, MessageRole, ProviderEvent,
    ProviderInputMessage, RunId, RunStatus, RunTrigger, RunUsage, SessionId, SessionKind,
    ToolCallStatus, ToolInvocationDraft,
};
use crate::db::DbPool;
use tokio_util::sync::CancellationToken;

const MAX_TOOL_ITERATIONS: usize = 10;

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

    // Load provider connection
    let connection = repository::get_provider_connection(&deps.pool, &input.connection_id)
        .await?
        .ok_or_else(|| AssistantEngineError::ProviderNotConfigured(input.connection_id.clone()))?;

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

    let mut usage: Option<RunUsage> = None;

    // Pre-register the assistant's tab with the JS bridge (once per run)
    // so that frontend tool handlers can find the correct tab.
    if let Some(tab_id) = &session.tab_id {
        let managed_agent_tab = session.kind == SessionKind::BackgroundJob
            || session.context.automation_id.is_some()
            || session.context.automation_name.is_some();
        let bridge = crate::mcp::bridge::JsBridge::new(deps.app.clone());
        let setup_params = serde_json::json!({
            "agentName": session
                .context
                .automation_name
                .as_deref()
                .or(session.title.as_deref())
                .unwrap_or("Assistant"),
            "managedAgentTab": managed_agent_tab,
            "tabId": tab_id,
            "mcpServerIds": session.context.mcp_server_ids,
        });
        let _ = bridge
            .call_tool(
                &bridge_agent_id(&session.id),
                session.context.space_id.as_deref().unwrap_or(""),
                session.context.room_id.as_deref().unwrap_or(""),
                "agent.setup",
                setup_params,
            )
            .await;
    }

    // === Tool execution loop ===
    // Build system prompt (prepended to every API call, not persisted)
    let system_message = build_system_prompt(&session.context, &tool_defs, &input.trigger);

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

    for iteration in 0..MAX_TOOL_ITERATIONS {
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
        let messages = repository::list_messages(&deps.pool, &session.id).await?;
        let normalized = normalize_history_for_provider(&messages);

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
                fail_run(deps, &session, &run_id, usage.as_ref(), &e.to_string()).await?;
                return Err(e.into());
            }
        };

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

        // Consume the stream
        let mut accumulated_text = String::new();
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
                        accumulated_text.push_str(&text);
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
                    ProviderEvent::Usage { usage: u } => {
                        usage = Some(u);
                    }
                    ProviderEvent::ToolCallReady { tool_call } => {
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
                        return Ok(());
                    }
                },
                Some(Err(e)) => {
                    let error_msg = e.to_string();
                    fail_run(deps, &session, &run_id, usage.as_ref(), &error_msg).await?;
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
        let mut final_content = Vec::new();
        if !accumulated_text.is_empty() {
            final_content.push(ContentPart::Text {
                text: accumulated_text.clone(),
            });
        }
        for tc in &tool_calls {
            final_content.push(ContentPart::ToolUse {
                tool_call_id: tc.tool_call_id.clone(),
                tool_name: tc.tool_name.clone(),
                arguments: tc.params.clone(),
            });
        }
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
                tab_id: session.tab_id.clone(),
                workspace_id: session.context.workspace_id.clone(),
                space_id: session.context.space_id.clone(),
                room_id: session.context.room_id.clone(),
                mcp_server_ids: session.context.mcp_server_ids.clone(),
                agent_workspace_id: session.context.agent_workspace_id.clone(),
                automation_id: session.context.automation_id.clone(),
                workspace_agents: session.context.workspace_agents.clone(),
                inter_agent_call_depth: input.inter_agent_call_depth,
                execution: session.context.execution.clone(),
                notices: notices.clone(),
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

        // Continue loop — will call API again with tool results in message history
    }

    // Complete the run — check for policy notices
    let tool_context = ToolExecutionContext {
        session_id: session.id.clone(),
        run_id: run_id.clone(),
        tool_call_id: None,
        tab_id: session.tab_id.clone(),
        workspace_id: session.context.workspace_id.clone(),
        space_id: session.context.space_id.clone(),
        room_id: session.context.room_id.clone(),
        mcp_server_ids: session.context.mcp_server_ids.clone(),
        agent_workspace_id: session.context.agent_workspace_id.clone(),
        automation_id: session.context.automation_id.clone(),
        workspace_agents: session.context.workspace_agents.clone(),
        inter_agent_call_depth: input.inter_agent_call_depth,
        execution: session.context.execution.clone(),
        notices,
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

/// Build the system prompt for the assistant.
fn build_system_prompt(
    context: &crate::assistant::types::SessionContext,
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
         - Use `fs.*` and `bash.*` only when those local execution capabilities are exposed in this session.\n\
         - Prior tool outputs in the conversation may be stale. Treat them as historical context, not guaranteed current state.\n\
         - Evaluate whether prior tool outputs are still fresh enough for the current decision. When information can expire or change over time (for example issues, alerts, metrics, repo state, or external system status), re-run the relevant tools if freshness matters.\n\
         - Chat is the default output channel. Use normal assistant replies for status, findings, and conclusions.\n\
         - Prefer `workspace.*` tools for durable outputs. Use them to list, read, create, and update artifacts that should remain in the workspace after the run.\n\
         - Before creating a new durable artifact, call `workspace.listArtifacts` and reuse or update an existing relevant artifact when possible.\n\
         - Prefer updating existing workspace artifacts over duplicating them.\n\
         - Be concise and direct in your responses. Prefer concrete actions and evidence over vague summaries.\n",
    );

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
             When task delegation tools are available, assign bounded tasks only to assigned workspace agents. Use `workspace.requestUserInput` when work is blocked on user feedback, approval, or missing information. If delegation tools are not available yet, explain which assigned agent should handle the work and what is blocked.\n\n",
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
             Prefer `workspace.*` artifact tools when saving durable workspace outputs.\n\
             For routine scheduled passes, a concise chat update is often sufficient.\n\
             Prefer updating existing visuals over recreating duplicate panels when the topic is unchanged.\n",
        );

        if let Some(description) = context.automation_description.as_deref() {
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

    if let Some(agent_workspace_id) = context.agent_workspace_id.as_deref() {
        prompt.push_str("\n## Local Execution Capabilities\n");
        if let Some(workspace_root) = agent_workspace_root_for_id(agent_workspace_id) {
            prompt.push_str(&format!(
                "- Private agent workspace: `{}` (read_write, default shell cwd)\n",
                workspace_root.display()
            ));
        } else {
            prompt
                .push_str("- Private agent workspace: available (read_write, default shell cwd)\n");
        }

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
            prompt.push_str("- Web access: enabled (`web.search` and `web.fetch` available)\n");
        }

        prompt.push_str(
            "\n## Agent Memory\n\
             The `.clai/memory/` directory inside your workspace is pre-created and ready to use as durable memory across runs.\n\
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
             ---\n\
             ```\n\
             - Keep each file under ~200 lines. When a file grows past this, prune stale entries or split into focused files.\n\
             - Replace outdated sections rather than appending indefinitely (except in `journal/`).\n\n",
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
    }

    ProviderInputMessage {
        role: MessageRole::System,
        content: vec![ContentPart::Text { text: prompt }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::ContentPart;
    use crate::assistant::types::SessionContext;
    use crate::assistant::types::WorkspaceAgentSummary;
    use crate::config::{ExecutionCapabilityConfig, ShellAccessMode};

    #[test]
    fn build_system_prompt_includes_agent_memory_guidance_for_automations() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, &[], &RunTrigger::Scheduled);
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
    fn build_system_prompt_omits_agent_memory_guidance_without_workspace() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Agent Memory"));
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

        let message = build_system_prompt(&context, &[], &RunTrigger::Scheduled);
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

        let message = build_system_prompt(&context, &[], &RunTrigger::Scheduled);
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

        let message = build_system_prompt(&context, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("Current local date and time: `"));
    }

    #[test]
    fn build_system_prompt_warns_that_prior_tool_results_may_be_stale() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, &[], &RunTrigger::Scheduled);
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
    fn build_system_prompt_describes_autonomous_run_mode() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            ..Default::default()
        };

        let message = build_system_prompt(&context, &[], &RunTrigger::Scheduled);
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

        let message = build_system_prompt(&context, &[], &RunTrigger::UserMessage);
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

        let message = build_system_prompt(&context, &[], &RunTrigger::UserMessage);
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

    #[test]
    fn build_system_prompt_memory_guardrails_present_in_both_modes() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            ..Default::default()
        };

        for trigger in &[RunTrigger::Scheduled, RunTrigger::UserMessage] {
            let message = build_system_prompt(&context, &[], trigger);
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
                    tool_name: "fs.write".into(),
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
                tool_name: "fs.write".into(),
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
    fn normalize_preserves_happy_path_alternation() {
        let messages = vec![
            user_message("hi"),
            assistant_message_with_content(vec![
                ContentPart::Text {
                    text: "writing".into(),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_a".into(),
                    tool_name: "fs.write".into(),
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
                    tool_name: "fs.write".into(),
                    arguments: serde_json::json!({}),
                },
                ContentPart::ToolUse {
                    tool_call_id: "call_b".into(),
                    tool_name: "fs.write".into(),
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
                        last.content.extend(msg.content.iter().cloned());
                        continue;
                    }
                }
                out.push(ProviderInputMessage {
                    role: MessageRole::Assistant,
                    content: msg.content.clone(),
                });
            }
            MessageRole::Tool => {
                if !tool_message_has_matching_assistant(&msg.content, &out) {
                    tracing::warn!(
                        message_id = %msg.id,
                        "Dropping orphan tool message (no matching tool_use in preceding assistant)"
                    );
                    continue;
                }
                out.push(ProviderInputMessage {
                    role: MessageRole::Tool,
                    content: msg.content.clone(),
                });
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

    out
}

fn assistant_content_is_empty(content: &[ContentPart]) -> bool {
    content.iter().all(|part| match part {
        ContentPart::Text { text } => text.is_empty(),
        ContentPart::ToolUse { .. } | ContentPart::ToolResult { .. } => false,
    })
}

fn tool_message_has_matching_assistant(
    content: &[ContentPart],
    out: &[ProviderInputMessage],
) -> bool {
    let Some(tool_call_id) = content.iter().find_map(|part| match part {
        ContentPart::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
        _ => None,
    }) else {
        return false;
    };

    for msg in out.iter().rev() {
        match msg.role {
            MessageRole::Assistant => {
                return msg.content.iter().any(|part| {
                    matches!(part, ContentPart::ToolUse { tool_call_id: id, .. } if id == tool_call_id)
                });
            }
            MessageRole::Tool => continue,
            _ => return false,
        }
    }

    false
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

fn build_trigger_message(
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

pub fn bridge_agent_id(session_id: &str) -> String {
    format!("assistant-session:{}", session_id)
}

/// Helper to mark a run as failed and emit the event.
async fn fail_run(
    deps: &AssistantDeps,
    session: &crate::assistant::types::AssistantSession,
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
    session: &crate::assistant::types::AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
    _message_id: Option<&str>,
) -> Result<(), AssistantEngineError> {
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

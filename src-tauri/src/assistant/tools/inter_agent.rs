use std::time::Duration;

use jsonschema::validator_for;
use tauri::Manager;
use tokio::time::timeout;

use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::repository::{
    self, CreateMessageParams, CreateRunParams, CreateSessionParams,
};
use crate::assistant::tools::ToolExecutionContext;
use crate::assistant::types::{
    ContentPart, InterAgentCallContext, MessageRole, ProviderConnection, RunTrigger,
    SessionContext, SessionKind,
};
use crate::AppState;

const MAX_CALL_DEPTH: u32 = 5;
const DEFAULT_TIMEOUT_SECS: u64 = 120;

pub async fn execute(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (target_agent_id, target_tool_name) = match parse_tool_name(tool_name) {
        Ok(value) => value,
        Err(message) => return Ok(error_result("invalid_tool_name", &message, false, None)),
    };

    let depth = context.inter_agent_call_depth.unwrap_or(0);
    if depth >= MAX_CALL_DEPTH {
        return Ok(error_result(
            "depth_limit_exceeded",
            &format!(
                "Inter-agent call depth limit ({}) exceeded. This likely indicates a circular call chain.",
                MAX_CALL_DEPTH
            ),
            false,
            None,
        ));
    }

    let (target_config, exposed_tool) = {
        let state = deps.app.state::<AppState>();
        let config = state
            .config_manager
            .lock()
            .map_err(|e| format!("Config lock error: {}", e))?;

        let target = match config.get_agent(target_agent_id) {
            Some(agent) if agent.enabled => agent,
            Some(_) => {
                return Ok(error_result(
                    "agent_disabled",
                    &format!("Agent '{}' is disabled", target_agent_id),
                    false,
                    None,
                ));
            }
            None => {
                return Ok(error_result(
                    "agent_not_found",
                    &format!("Agent not found: {}", target_agent_id),
                    false,
                    None,
                ));
            }
        };

        let tool = match target
            .exposed_tools
            .iter()
            .find(|tool| tool.name == target_tool_name)
        {
            Some(tool) => tool.clone(),
            None => {
                return Ok(error_result(
                    "tool_not_exposed",
                    &format!(
                        "Agent '{}' does not expose tool '{}'",
                        target_agent_id, target_tool_name
                    ),
                    false,
                    None,
                ));
            }
        };

        (target, tool)
    };

    let connection = match resolve_first_connection(deps, &target_config).await {
        Ok(connection) => connection,
        Err(message) => return Ok(error_result("no_provider", &message, false, None)),
    };

    let call_id = uuid::Uuid::new_v4().to_string();
    let session = match repository::create_session(
        &deps.pool,
        CreateSessionParams {
            tab_id: None,
            kind: SessionKind::BackgroundJob,
            title: Some(format!("{} (called)", target_config.name)),
            context: SessionContext {
                space_id: context.space_id.clone(),
                room_id: context.room_id.clone(),
                workspace_id: Some(target_config.id.clone()),
                tab_id: None,
                tool_scopes: target_config
                    .required_tools()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                mcp_server_ids: target_config.selected_mcp_server_ids.clone(),
                execution: target_config.execution.clone(),
                netdata_conversation_id: None,
                automation_id: Some(target_config.id.clone()),
                agent_workspace_id: Some(target_config.id.clone()),
                automation_name: Some(target_config.name.clone()),
                automation_description: Some(target_config.description.clone()),
                inter_agent_call: Some(InterAgentCallContext {
                    call_id: call_id.clone(),
                    caller_agent_id: context.automation_id.clone(),
                    caller_session_id: context.session_id.clone(),
                    caller_run_id: context.run_id.clone(),
                    caller_tool_call_id: context.tool_call_id.clone(),
                    callee_agent_id: target_config.id.clone(),
                    exposed_tool_name: target_tool_name.to_string(),
                }),
                workspace_agents: Vec::new(),
            },
        },
    )
    .await
    {
        Ok(session) => session,
        Err(message) => {
            return Ok(error_result(
                "session_creation_failed",
                &format!("Failed to create session: {}", message),
                true,
                Some(basic_trace(&call_id, target_agent_id, None, None)),
            ));
        }
    };

    let caller_name = context.automation_id.as_deref().unwrap_or("interactive");
    if let Err(message) = repository::create_message(
        &deps.pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::User,
            content: vec![ContentPart::Text {
                text: format!(
                    "You have been called by agent '{}' via your tool '{}'.\n\n\
                     Call ID: {}\n\n\
                     Request parameters:\n{}\n\n\
                     Required output schema:\n{}\n\n\
                     Process this request using your tools. Return exactly one JSON object that matches the output schema. Do not use markdown fences. Do not ask follow-up questions.",
                    caller_name,
                    target_tool_name,
                    call_id,
                    serde_json::to_string_pretty(&params).unwrap_or_default(),
                    serde_json::to_string_pretty(&exposed_tool.output_schema).unwrap_or_default(),
                ),
            }],
            provider_metadata: None,
        },
    )
    .await
    {
        return Ok(error_result(
            "request_persist_failed",
            &format!("Failed to persist request: {}", message),
            true,
            Some(basic_trace(
                &call_id,
                target_agent_id,
                Some(&session.id),
                None,
            )),
        ));
    }

    let run = match repository::create_run(
        &deps.pool,
        CreateRunParams {
            session_id: session.id.clone(),
            status: crate::assistant::types::RunStatus::Queued,
            trigger: RunTrigger::InterAgentCall,
            connection_id: connection.id.clone(),
            provider_id: connection.provider_id.clone(),
            model_id: connection.model_id.clone(),
            usage: None,
            error: None,
        },
    )
    .await
    {
        Ok(run) => run,
        Err(message) => {
            return Ok(error_result(
                "run_creation_failed",
                &format!("Failed to create run: {}", message),
                true,
                Some(basic_trace(
                    &call_id,
                    target_agent_id,
                    Some(&session.id),
                    None,
                )),
            ));
        }
    };

    let cancel = crate::assistant::runtime::register_run(&run.id);
    let input = RunTurnInput {
        session_id: session.id.clone(),
        run_id: Some(run.id.clone()),
        trigger: RunTrigger::InterAgentCall,
        connection_id: connection.id.clone(),
        cancel_token: cancel.clone(),
        inter_agent_call_depth: Some(depth + 1),
    };

    let result = timeout(
        Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        Box::pin(engine::run_session_turn(deps, input)),
    )
    .await;

    crate::assistant::runtime::unregister_run(&run.id);

    match result {
        Ok(Ok(())) => {
            extract_response(
                &deps.pool,
                &session.id,
                &run.id,
                &call_id,
                target_agent_id,
                &exposed_tool.output_schema,
            )
            .await
        }
        Ok(Err(error)) => Ok(error_result(
            "agent_execution_failed",
            &format!("Agent '{}' failed: {}", target_agent_id, error),
            true,
            Some(basic_trace(
                &call_id,
                target_agent_id,
                Some(&session.id),
                Some(&run.id),
            )),
        )),
        Err(_) => {
            cancel.cancel();
            Ok(error_result(
                "timeout",
                &format!(
                    "Call to agent '{}' timed out after {}s",
                    target_agent_id, DEFAULT_TIMEOUT_SECS
                ),
                true,
                Some(basic_trace(
                    &call_id,
                    target_agent_id,
                    Some(&session.id),
                    Some(&run.id),
                )),
            ))
        }
    }
}

fn parse_tool_name(tool_name: &str) -> Result<(&str, &str), String> {
    let rest = tool_name
        .strip_prefix("agent.")
        .ok_or_else(|| format!("Not an agent tool: {}", tool_name))?;
    let dot = rest
        .find('.')
        .ok_or_else(|| format!("Invalid agent tool name: {}", tool_name))?;
    Ok((&rest[..dot], &rest[dot + 1..]))
}

async fn extract_response(
    pool: &crate::db::DbPool,
    session_id: &str,
    run_id: &str,
    call_id: &str,
    target_agent_id: &str,
    output_schema: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let messages = repository::list_messages(pool, session_id)
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    for msg in messages.iter().rev() {
        if msg.role != MessageRole::Assistant {
            continue;
        }

        for part in &msg.content {
            let ContentPart::Text { text } = part else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }

            let parsed: serde_json::Value = match serde_json::from_str(text) {
                Ok(value) => value,
                Err(_) => {
                    return Ok(error_result(
                        "invalid_response_json",
                        "Agent response was not valid JSON",
                        true,
                        Some(basic_trace(
                            call_id,
                            target_agent_id,
                            Some(session_id),
                            Some(run_id),
                        )),
                    ));
                }
            };

            if let Err(message) = validate_json_schema(output_schema, &parsed) {
                return Ok(error_result(
                    "output_validation_failed",
                    &format!("Agent response did not match output schema: {}", message),
                    true,
                    Some(basic_trace(
                        call_id,
                        target_agent_id,
                        Some(session_id),
                        Some(run_id),
                    )),
                ));
            }

            return Ok(serde_json::json!({
                "ok": true,
                "data": parsed,
                "trace": basic_trace(
                    call_id,
                    target_agent_id,
                    Some(session_id),
                    Some(run_id),
                ),
            }));
        }
    }

    Ok(error_result(
        "no_response",
        "Agent produced no response",
        true,
        Some(basic_trace(
            call_id,
            target_agent_id,
            Some(session_id),
            Some(run_id),
        )),
    ))
}

async fn resolve_first_connection(
    deps: &AssistantDeps,
    config: &crate::config::AgentConfig,
) -> Result<ProviderConnection, String> {
    let all = repository::list_provider_connections(&deps.pool)
        .await
        .map_err(|e| format!("Failed to list providers: {}", e))?;

    for id in &config.provider_connection_ids {
        if let Some(conn) = all.iter().find(|c| &c.id == id && c.enabled) {
            return Ok(conn.clone());
        }
    }

    Err(format!("Agent '{}' has no active provider", config.id))
}

fn validate_json_schema(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
) -> Result<(), String> {
    let validator = validator_for(schema).map_err(|e| e.to_string())?;
    validator.validate(instance).map_err(|e| e.to_string())
}

fn error_result(
    code: &str,
    message: &str,
    retryable: bool,
    trace: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "error": {
            "code": code,
            "message": message,
            "retryable": retryable,
        },
        "trace": trace,
    })
}

fn basic_trace(
    call_id: &str,
    callee_agent_id: &str,
    callee_session_id: Option<&str>,
    callee_run_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "callId": call_id,
        "calleeAgentId": callee_agent_id,
        "calleeSessionId": callee_session_id,
        "calleeRunId": callee_run_id,
    })
}

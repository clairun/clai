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
use crate::config::{AgentConfig, ExecutionCapabilityConfig, ExposedAgentTool};
use crate::db::DbPool;
use crate::AppState;

const MAX_CALL_DEPTH: u32 = 5;
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Loads a workspace_agents row by id and reconstructs it as an `AgentConfig`.
///
/// Phase 1.5 of the workspace-local-agents refactor: inter-agent callees are
/// resolved from the DB, not from the (transitional) global catalog.
async fn load_workspace_agent_as_config(
    pool: &DbPool,
    id: &str,
) -> Result<Option<AgentConfig>, String> {
    let row = sqlx::query(
        r#"
        SELECT id, workspace_id, name, description, selected_skill_ids, selected_mcp_server_ids,
               provider_connection_ids, execution, exposed_tools,
               schedule_enabled, interval_minutes, enabled,
               created_at, updated_at
        FROM workspace_agents
        WHERE id = ?
        LIMIT 1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load workspace agent: {}", e))?;

    let Some(row) = row else {
        return Ok(None);
    };

    use sqlx::Row;
    let selected_skill_ids: String = row.try_get("selected_skill_ids").unwrap_or_default();
    let selected_mcp_server_ids: String =
        row.try_get("selected_mcp_server_ids").unwrap_or_default();
    let provider_connection_ids: String =
        row.try_get("provider_connection_ids").unwrap_or_default();
    let execution: String = row.try_get("execution").unwrap_or_default();
    let exposed_tools: String = row.try_get("exposed_tools").unwrap_or_default();
    let schedule_enabled: i64 = row.try_get("schedule_enabled").unwrap_or(0);
    let enabled: i64 = row.try_get("enabled").unwrap_or(1);
    let created_ms: i64 = row.try_get("created_at").unwrap_or(0);
    let updated_ms: i64 = row.try_get("updated_at").unwrap_or(0);
    let created_at = chrono::DateTime::from_timestamp_millis(created_ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    let updated_at = chrono::DateTime::from_timestamp_millis(updated_ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    Ok(Some(AgentConfig {
        id: row.try_get::<String, _>("id").unwrap_or_default(),
        workspace_id: row.try_get::<String, _>("workspace_id").unwrap_or_default(),
        name: row.try_get::<String, _>("name").unwrap_or_default(),
        description: row.try_get::<String, _>("description").unwrap_or_default(),
        schedule_enabled: schedule_enabled != 0,
        interval_minutes: row
            .try_get::<i64, _>("interval_minutes")
            .unwrap_or(0)
            .max(0) as u32,
        enabled: enabled != 0,
        selected_mcp_server_ids: serde_json::from_str(&selected_mcp_server_ids)
            .unwrap_or_default(),
        provider_connection_ids: serde_json::from_str(&provider_connection_ids)
            .unwrap_or_default(),
        selected_skill_ids: serde_json::from_str(&selected_skill_ids).unwrap_or_default(),
        execution: serde_json::from_str::<ExecutionCapabilityConfig>(&execution)
            .unwrap_or_default(),
        exposed_tools: serde_json::from_str::<Vec<ExposedAgentTool>>(&exposed_tools)
            .unwrap_or_default(),
        created_at,
        updated_at,
    }))
}

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

    // Phase 1.5: resolve the callee from the workspace_agents DB table.
    // The target_agent_id is the workspace-local row id (populated into
    // WorkspaceAgentSummary.agent_definition_id by workspace_agent_summaries).
    let target_config = match load_workspace_agent_as_config(&deps.pool, target_agent_id).await {
        Ok(Some(agent)) if agent.enabled => agent,
        Ok(Some(_)) => {
            return Ok(error_result(
                "agent_disabled",
                &format!("Agent '{}' is disabled", target_agent_id),
                false,
                None,
            ));
        }
        Ok(None) => {
            return Ok(error_result(
                "agent_not_found",
                &format!("Agent not found: {}", target_agent_id),
                false,
                None,
            ));
        }
        Err(message) => {
            return Ok(error_result("agent_load_failed", &message, false, None));
        }
    };

    let exposed_tool = match target_config
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
                workspace_id: Some(target_config.workspace_id.clone()),
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
                // Same workspace, same on-disk root as the caller. Tools
                // dispatched from the assignee write into the workspace's
                // shared dir, not the assignee's per-agent scratch.
                agent_workspace_id: Some(target_config.workspace_id.clone()),
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

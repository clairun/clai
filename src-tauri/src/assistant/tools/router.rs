use tauri::Manager;

use crate::assistant::engine::{bridge_agent_id, AssistantDeps};
use crate::assistant::tools::inter_agent;
use crate::assistant::tools::local;
use crate::assistant::tools::workspace_tasks;
use crate::mcp::bridge::JsBridge;
use crate::AppState;

use super::ToolExecutionContext;

/// Execute a tool by name with the given parameters.
/// Returns the tool result as JSON, or an error string.
pub async fn execute_tool(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match tool_name {
        name if name.starts_with("fs.")
            || name.starts_with("bash.")
            || name.starts_with("web.") =>
        {
            local::execute_local_tool(context, name, params).await
        }
        name if name.starts_with("dashboard.")
            || name.starts_with("anomalies.")
            || name.starts_with("tabs.")
            || name.starts_with("canvas.") =>
        {
            execute_bridge_tool(deps, context, name, params).await
        }
        name if name.starts_with("agent.") => {
            inter_agent::execute(deps, context, name, params).await
        }
        "workspace.listAgents"
        | "workspace.assignTask"
        | "workspace.getTaskResult"
        | "workspace.requestUserInput" => {
            workspace_tasks::execute(deps, context, tool_name, params).await
        }
        _ => execute_external_mcp_tool(deps, context, tool_name, params).await,
    }
}

/// Execute a JS-bridge tool (dashboard.*, tabs.*, canvas.*).
/// The engine pre-registers the tab via `agent.setup` once per run,
/// so tool handlers can find the correct tab via `ensureAgentTab`.
async fn execute_bridge_tool(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let bridge = JsBridge::new(deps.app.clone());

    bridge
        .call_tool_with_context(
            &bridge_agent_id(&context.session_id),
            context.space_id.as_deref().unwrap_or(""),
            context.room_id.as_deref().unwrap_or(""),
            context.tab_id.as_deref(),
            &context.mcp_server_ids,
            tool_name,
            params,
        )
        .await
        .map_err(|e| format!("{} failed: {}", tool_name, e))
}

async fn execute_external_mcp_tool(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let state = deps.app.state::<AppState>();
    let mut manager = state.mcp_client_manager.lock().await;
    manager
        .execute_tool(&context.mcp_server_ids, tool_name, params)
        .await
        .map_err(|e| format!("{} failed: {}", tool_name, e))
}

use tauri::Manager;

use crate::assistant::engine::AssistantDeps;
use crate::assistant::tools::local;
use crate::assistant::tools::workspace_tasks;
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
        name if name.starts_with("agent.") => Err(
            "Global agent tools are no longer available. Use workspace-local task delegation instead."
                .to_string(),
        ),
        "workspace.listAgents"
        | "workspace.assignTask"
        | "workspace.getTaskResult"
        | "workspace.requestUserInput" => {
            workspace_tasks::execute(deps, context, tool_name, params).await
        }
        _ => execute_external_mcp_tool(deps, context, tool_name, params).await,
    }
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

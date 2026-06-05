use tauri::Manager;

use crate::assistant::engine::AssistantDeps;
use crate::assistant::tools::{ask_user, local, workspace_tasks};
use crate::AppState;

use super::ToolExecutionContext;

/// Execute a tool by name with the given parameters.
/// Returns the tool result as JSON, or an error string.
///
/// Tool names use `_` as the separator (`fs_list`, `bash_exec`,
/// `workspace_listAgents`) to satisfy OpenAI's stricter function-name
/// regex (`^[a-zA-Z][a-zA-Z0-9_-]*$`). Legacy conversation history may
/// still carry the old dotted form (`fs.list`); we canonicalize on
/// dispatch so those continue to work, and a one-shot DB migration
/// rewrites them at-rest on the next launch.
pub async fn execute_tool(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // The external arm gets the qualifier-stripped (but NOT legacy-dot
    // rewritten) name: `mcp__clai__mcp__1c47ed2d__x` must reach the MCP
    // client as `mcp__1c47ed2d__x`, while external tool names themselves
    // are always passed verbatim.
    let unqualified = super::strip_local_mcp_qualifier(tool_name);
    let canonical = canonicalize_tool_name(tool_name);
    let name_for_dispatch = canonical.as_str();
    match name_for_dispatch {
        name if name.starts_with("fs_")
            || name.starts_with("bash_")
            || name.starts_with("web_") =>
        {
            local::execute_local_tool(deps, context, name, params).await
        }
        name if name.starts_with("agent_") => Err(
            "Global agent tools are no longer available. Use workspace-local task delegation instead."
                .to_string(),
        ),
        "workspace_listAgents" | "workspace_assignTask" | "workspace_getTaskResult" => {
            workspace_tasks::execute(deps, context, name_for_dispatch, params).await
        }
        "ask_user" => ask_user::execute(deps, context, params).await,
        _ => execute_external_mcp_tool(deps, context, unqualified, params).await,
    }
}

/// Canonicalizes a possibly-legacy tool name. Built-in tools historically
/// used `.` as the namespace separator (`bash.exec`); we now use `_` to
/// be compatible with OpenAI's function-name regex. Names that match a
/// known legacy prefix are rewritten on the fly so old conversation
/// history dispatches to the right handler.
///
/// Names qualified with the local MCP server (`mcp__clai__web_fetch`) are
/// unwrapped first: history recorded under a Claude Code run stores tools
/// under that qualifier, and a model running on the local agent mimics
/// those names from replayed history instead of the advertised plain ones.
/// Without the unwrap they fall through to the external-MCP path and every
/// tool fails with "not allowed for this session" after a provider switch.
fn canonicalize_tool_name(name: &str) -> String {
    let name = super::strip_local_mcp_qualifier(name);
    const LEGACY_PREFIXES: &[&str] = &["fs.", "bash.", "web.", "workspace.", "agent."];
    if LEGACY_PREFIXES.iter().any(|p| name.starts_with(p)) {
        name.replacen('.', "_", 1)
    } else {
        name.to_string()
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

#[cfg(test)]
mod tests {
    use super::canonicalize_tool_name;

    #[test]
    fn canonicalizes_legacy_dotted_names() {
        assert_eq!(canonicalize_tool_name("fs.list"), "fs_list");
        assert_eq!(canonicalize_tool_name("bash.exec"), "bash_exec");
        assert_eq!(canonicalize_tool_name("web_fetch"), "web_fetch");
    }

    #[test]
    fn unwraps_local_mcp_qualified_builtins() {
        assert_eq!(canonicalize_tool_name("mcp__clai__web_fetch"), "web_fetch");
        assert_eq!(canonicalize_tool_name("mcp__clai__bash_exec"), "bash_exec");
        assert_eq!(
            canonicalize_tool_name("mcp__clai__workspace_assignTask"),
            "workspace_assignTask"
        );
    }

    #[test]
    fn leaves_external_mcp_names_for_the_external_path() {
        assert_eq!(
            canonicalize_tool_name("mcp__1c47ed2d__search"),
            "mcp__1c47ed2d__search"
        );
        assert_eq!(
            canonicalize_tool_name("mcp__clai__mcp__1c47ed2d__search"),
            "mcp__1c47ed2d__search"
        );
    }
}

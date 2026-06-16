use std::collections::HashMap;
use std::sync::OnceLock;

use crate::assistant::tools::ask_user::AskUserParams;
use crate::assistant::tools::workspace_tasks::{
    AssignWorkspaceTaskParams, GetWorkspaceTaskResultParams, ListWorkspaceAgentsParams,
};
use crate::assistant::types::{SessionContext, ToolDefinition};
use crate::config::ShellAccessMode;

/// Returns all tool definitions available for the given session context.
pub fn available_tools(
    context: &SessionContext,
    external_tools: &[ToolDefinition],
) -> Vec<ToolDefinition> {
    let mut tools = vec![];

    if is_workspace_manager_context(context) {
        tools.push(workspace_list_agents_def());
        tools.push(workspace_assign_task_def());
        tools.push(workspace_get_task_result_def());
        tools.push(ask_user_def());
    }

    if context.agent_workspace_id.is_some() {
        tools.push(fs_list_def());
        tools.push(fs_glob_def());
        tools.push(fs_read_def());
        tools.push(fs_write_def());
        tools.push(fs_request_grant_def());
        tools.push(history_query_def());
    }

    if context.agent_workspace_id.is_some()
        && !matches!(context.execution.shell.mode, ShellAccessMode::Off)
    {
        tools.push(bash_exec_def());
    }

    // Web tools: always available in regular sessions, opt-in for agent sessions
    let web_enabled = match context.agent_workspace_id.as_ref() {
        Some(_) => context.execution.web.enabled,
        None => true,
    };
    if web_enabled {
        tools.push(web_search_def());
        tools.push(web_fetch_def());
    }

    tools.extend(external_tools.iter().cloned());

    tools
}

/// Compiled JSON-Schema validator for a built-in tool's declared
/// `input_schema`, plus the schema itself (embedded in rejection
/// messages so the model can self-correct in the same turn).
pub struct BuiltinParamValidator {
    pub schema: serde_json::Value,
    pub validator: jsonschema::Validator,
}

/// Look up the compiled validator for a built-in tool name. Returns
/// `None` for external MCP tools — their schemas live on (and are
/// enforced by) the remote server; double-validating against a possibly
/// loose advertised copy risks false rejections.
pub fn builtin_param_validator(name: &str) -> Option<&'static BuiltinParamValidator> {
    static VALIDATORS: OnceLock<HashMap<String, BuiltinParamValidator>> = OnceLock::new();
    VALIDATORS
        .get_or_init(|| {
            all_builtin_defs()
                .into_iter()
                .filter_map(|def| {
                    let validator = match jsonschema::validator_for(&def.input_schema) {
                        Ok(validator) => validator,
                        Err(e) => {
                            // A schema we wrote ourselves failed to compile —
                            // programming error. Skip validation for that tool
                            // rather than blocking every call to it.
                            tracing::error!(tool = %def.name, error = %e,
                                "Built-in tool input_schema failed to compile; \
                                 dispatch-time validation disabled for it");
                            return None;
                        }
                    };
                    Some((
                        def.name,
                        BuiltinParamValidator {
                            schema: def.input_schema,
                            validator,
                        },
                    ))
                })
                .collect()
        })
        .get(name)
}

/// Every built-in tool definition, unconditionally. `available_tools`
/// gates what a given session *sees*; this list is the single source
/// for name→schema lookups at dispatch time (validation must cover a
/// tool even when the model calls it from a session that wasn't
/// offered it — dispatch decides how to fail, not the registry).
fn all_builtin_defs() -> Vec<ToolDefinition> {
    vec![
        workspace_list_agents_def(),
        workspace_assign_task_def(),
        workspace_get_task_result_def(),
        ask_user_def(),
        fs_list_def(),
        fs_glob_def(),
        fs_read_def(),
        fs_write_def(),
        fs_request_grant_def(),
        bash_exec_def(),
        web_search_def(),
        web_fetch_def(),
        history_query_def(),
    ]
}

fn workspace_list_agents_def() -> ToolDefinition {
    tool::<ListWorkspaceAgentsParams>(
        "workspace_listAgents",
        "List agents assigned to this workspace. Use this before delegating work so tasks are assigned only to workspace-local agents.",
    )
}

fn workspace_assign_task_def() -> ToolDefinition {
    tool::<AssignWorkspaceTaskParams>(
        "workspace_assignTask",
        "Assign a bounded task to an agent assigned to this workspace — including yourself, to run work in the background. The task runs asynchronously in its own session, in parallel with this one; multiple tasks may run at once, even for the same agent. Returns immediately with a task ID to poll via workspace_getTaskResult. The worker does not see this conversation, so instructions must be self-contained.",
    )
}

fn workspace_get_task_result_def() -> ToolDefinition {
    tool::<GetWorkspaceTaskResultParams>(
        "workspace_getTaskResult",
        "Read the current status and result of a workspace-local task by task ID.",
    )
}

fn ask_user_def() -> ToolDefinition {
    tool::<AskUserParams>(
        "ask_user",
        "Ask the human a question and BLOCK until they answer. The question is rendered inline in the chat with an interactive answer control (radio buttons when `options` are provided, with an automatic \"Other\" free-text fallback; a plain textarea otherwise). Do NOT add an \"Other\" entry to `options` yourself — the FE always appends one. Use this whenever you need a human decision before proceeding — do NOT phrase the question as plain assistant text expecting them to reply in the next turn; use this tool instead so the run pauses, the question is unmistakable, and the answer is returned to you as the tool_result. Returns `{answer: string, selectedOptionIndex?: number}`.",
    )
}

fn fs_list_def() -> ToolDefinition {
    ToolDefinition {
        name: "fs_list".to_string(),
        description: "List files and directories under the agent workspace or another allowed filesystem path. Supports optional recursive traversal with a hard result limit.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "recursive": { "type": "boolean" },
                "limit": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        }),
    }
}

fn fs_glob_def() -> ToolDefinition {
    ToolDefinition {
        name: "fs_glob".to_string(),
        description: "Find files or directories matching a glob pattern within the agent workspace or another allowed filesystem path.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1 }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    }
}

fn fs_read_def() -> ToolDefinition {
    ToolDefinition {
        name: "fs_read".to_string(),
        description: "Read a text file from the agent workspace or from an additional allowed filesystem path.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    }
}

fn fs_write_def() -> ToolDefinition {
    ToolDefinition {
        name: "fs_write".to_string(),
        description: "Write a text file to the agent workspace or to an additional writable filesystem path. Creates parent directories when requested.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" },
                "createParents": { "type": "boolean" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
    }
}

fn fs_request_grant_def() -> ToolDefinition {
    ToolDefinition {
        name: "fs_request_grant".to_string(),
        description: "Request the user's approval to extend this agent's filesystem grants. Use BEFORE attempting work that needs paths outside your current grants (e.g. `~/.ssh` for `git push`, `~/.config/gh` for the `gh` CLI). Request the narrowest path that satisfies the task. The user can approve (once or always), narrow the path, downgrade the access, or deny. If granted `once`, the access lasts the rest of this run. If granted `always`, the grant persists to agent settings. If the path is already covered by existing grants, the tool returns immediately without prompting.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path or ~-prefixed path" },
                "access": { "type": "string", "enum": ["read_only", "read_write"], "description": "Requested access level. Prefer read_only unless writes are genuinely needed." },
                "reason": { "type": "string", "description": "Brief explanation shown to the user in the approval modal. Be specific about why you need this path for the current task." }
            },
            "required": ["path", "access", "reason"],
            "additionalProperties": false
        }),
    }
}

fn history_query_def() -> ToolDefinition {
    ToolDefinition {
        name: "history_query".to_string(),
        description: "Run a single READ-ONLY SQL query against THIS workspace's conversation history database (`.clai/data.sqlite`) and nothing else. This is your always-available recovery path: use it to recall verbatim past work when memory files don't have it — the exact command that was run, the full text of an old error, what the user or a sibling agent said earlier, or detail compacted out of your context. It is read-only and cannot reach any other file, so it never needs approval. Discover the schema first (e.g. `SELECT name FROM sqlite_master WHERE type='table'`, `PRAGMA table_info(<table>)`); key tables are `assistant_messages` (content in `content_json`), `assistant_tool_calls`, `assistant_runs`, `workspace_tasks`. Keep queries narrow: SELECT specific columns, filter, and page with LIMIT/OFFSET — single rows can be very large. Only SELECT/WITH/EXPLAIN/PRAGMA/VALUES are accepted; writes, multiple statements, and ATTACH are rejected.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "sql": { "type": "string", "description": "A single read-only SQL statement (SELECT/WITH/EXPLAIN/PRAGMA/VALUES) against this workspace's data.sqlite." },
                "maxRows": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Maximum rows to return (default 100, max 1000). Page larger result sets with LIMIT/OFFSET." }
            },
            "required": ["sql"],
            "additionalProperties": false
        }),
    }
}

fn bash_exec_def() -> ToolDefinition {
    ToolDefinition {
        name: "bash_exec".to_string(),
        description: "Run a shell command through CLAI's guarded executor inside this automation's allowed working directory. On Linux this runs inside the local execution sandbox; if the sandbox is unavailable, the command fails closed. For long-running work (CI tails, builds, large test suites), pass an explicit timeoutMs up to 1800000 (30 min); the default is 300000 (5 min).".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "cwd": { "type": "string" },
                "timeoutMs": { "type": "integer", "minimum": 1, "maximum": 1800000, "description": "Command timeout in milliseconds (default 300000, max 1800000)" },
                "maxOutputChars": { "type": "integer", "minimum": 1 }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    }
}

fn web_search_def() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".to_string(),
        description: "Search the web using DuckDuckGo and return a list of results with titles, URLs, and snippets.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query" },
                "maxResults": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Maximum number of results to return (default 10)" }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

fn web_fetch_def() -> ToolDefinition {
    ToolDefinition {
        name: "web_fetch".to_string(),
        description: "Fetch a web page and return its content as markdown. Useful for reading documentation, articles, or any web content.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The URL to fetch" },
                "maxContentChars": { "type": "integer", "minimum": 1, "description": "Maximum characters of content to return (default 20000)" },
                "timeoutMs": { "type": "integer", "minimum": 1, "description": "Request timeout in milliseconds (default 15000)" }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
    }
}

fn is_workspace_manager_context(context: &SessionContext) -> bool {
    let Some(current_agent_definition_id) = context.automation_id.as_deref() else {
        return false;
    };

    context
        .workspace_agents
        .iter()
        .any(|agent| agent.is_default && agent.agent_definition_id == current_agent_definition_id)
}

/// Build a ToolDefinition from a schemars-annotated param type.
///
/// Subschemas are inlined (no `$defs`/`$ref` indirection) — weaker
/// models have been observed mis-reading referenced schemas and
/// inventing wrapper objects around nested arrays (MiniMax sent
/// `options: {"item": [...]}` to ask_user, June 2026). The `$schema`
/// and `title` meta keys are stripped: providers treat `input_schema`
/// as a plain object schema and the extra keys are noise in every
/// request payload.
fn tool<T: schemars::JsonSchema>(name: &str, description: &str) -> ToolDefinition {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.meta_schema = None;
        })
        .into_generator();
    let schema = generator.into_root_schema_for::<T>();
    let mut input_schema = serde_json::to_value(schema).unwrap_or(serde_json::json!({}));
    if let Some(object) = input_schema.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
    }

    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_validator_exists_for_every_builtin_tool() {
        for def in all_builtin_defs() {
            assert!(
                builtin_param_validator(&def.name).is_some(),
                "no compiled validator for `{}` — its input_schema failed to compile",
                def.name
            );
        }
    }

    #[test]
    fn external_tool_names_have_no_builtin_validator() {
        assert!(builtin_param_validator("mcp__1c47ed2d__search").is_none());
        assert!(builtin_param_validator("not_a_tool").is_none());
    }

    #[test]
    fn schemars_tools_are_inlined_and_strict() {
        let def = ask_user_def();
        let rendered = def.input_schema.to_string();
        // No $ref/$defs indirection — the nested option shape is literal.
        assert!(!rendered.contains("$ref"), "schema not inlined: {rendered}");
        assert!(
            !rendered.contains("$defs"),
            "schema not inlined: {rendered}"
        );
        assert!(!rendered.contains("$schema"));
        // Strict: unknown keys are rejected at the top level.
        assert_eq!(
            def.input_schema["additionalProperties"],
            serde_json::json!(false),
            "ask_user schema must set additionalProperties:false (deny_unknown_fields)"
        );
        // The options items schema is visible inline.
        assert_eq!(
            def.input_schema["properties"]["options"]["items"]["type"],
            "object"
        );
    }

    #[test]
    fn handwritten_schemas_are_strict() {
        for def in all_builtin_defs() {
            assert_eq!(
                def.input_schema["additionalProperties"],
                serde_json::json!(false),
                "`{}` input_schema must set additionalProperties:false",
                def.name
            );
        }
    }
}

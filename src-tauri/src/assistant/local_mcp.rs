use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::Router;
use rmcp::model::{
    CallToolRequestParam, CallToolResult, ErrorData as McpError, JsonObject, ListToolsResult,
    PaginatedRequestParam, ServerCapabilities, ServerInfo, Tool as RmcpTool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, tower::StreamableHttpService,
};
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::ServerHandler;
use tauri::Manager;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::assistant::engine::AssistantDeps;
use crate::assistant::repository;
use crate::assistant::tools::{self, ToolExecutionContext};
use crate::assistant::types::{RunNotice, ToolDefinition};
use crate::AppState;

static LOCAL_MCP_RUNTIME: OnceLock<tokio::sync::Mutex<Option<Arc<LocalMcpRuntime>>>> =
    OnceLock::new();

fn runtime_slot() -> &'static tokio::sync::Mutex<Option<Arc<LocalMcpRuntime>>> {
    LOCAL_MCP_RUNTIME.get_or_init(|| tokio::sync::Mutex::new(None))
}

#[derive(Clone)]
pub struct LocalMcpRuntime {
    url: String,
    deps: AssistantDeps,
    cancellation_token: CancellationToken,
    bindings: Arc<RwLock<HashMap<String, ToolBinding>>>,
}

#[derive(Clone)]
pub struct ToolBinding {
    pub session_id: String,
    pub run_id: String,
    pub cancel_token: CancellationToken,
    pub inter_agent_call_depth: Option<u32>,
    pub notices: Arc<Mutex<Vec<RunNotice>>>,
    /// Run-scoped grants accepted via `fs_request_grant`. Shared with the
    /// host engine so a session grant accepted during a CLI-provider run is
    /// visible to the same run's subsequent tool calls.
    pub session_grants: Arc<Mutex<Vec<crate::config::FilesystemPathGrant>>>,
}

impl LocalMcpRuntime {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn bind_run(&self, binding: ToolBinding) -> String {
        let token = Uuid::new_v4().to_string();
        self.bindings.write().await.insert(token.clone(), binding);
        token
    }

    pub async fn unbind_token(&self, token: &str) {
        self.bindings.write().await.remove(token);
    }

    async fn binding_from_request(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<ToolBinding, McpError> {
        let token = bearer_token(context).ok_or_else(|| {
            McpError::invalid_request("missing bearer token for CLAI MCP request", None)
        })?;
        self.bindings
            .read()
            .await
            .get(&token)
            .cloned()
            .ok_or_else(|| {
                McpError::invalid_request("invalid bearer token for CLAI MCP request", None)
            })
    }
}

pub async fn ensure_started(deps: &AssistantDeps) -> Result<Arc<LocalMcpRuntime>, String> {
    let mut guard = runtime_slot().lock().await;
    if let Some(runtime) = guard.as_ref() {
        return Ok(runtime.clone());
    }

    let cancellation_token = CancellationToken::new();
    let bindings = Arc::new(RwLock::new(HashMap::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind local MCP server: {}", e))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to read local MCP server address: {}", e))?;
    let url = format!("http://{}/mcp", addr);

    let runtime_with_url = Arc::new(LocalMcpRuntime {
        url,
        deps: deps.clone(),
        cancellation_token: cancellation_token.clone(),
        bindings,
    });
    let service_runtime = runtime_with_url.clone();
    let service: StreamableHttpService<ClaiMcpService, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(ClaiMcpService {
                    runtime: service_runtime.clone(),
                })
            },
            Default::default(),
            StreamableHttpServerConfig {
                stateful_mode: true,
                sse_keep_alive: None,
                cancellation_token: cancellation_token.child_token(),
            },
        );
    let router = Router::new().nest_service("/mcp", service);
    let shutdown = runtime_with_url.cancellation_token.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
            .await
        {
            tracing::error!(error = %error, "Local MCP server exited with error");
        }
    });

    tracing::info!(url = %runtime_with_url.url(), "Started local CLAI MCP server");
    *guard = Some(runtime_with_url.clone());
    Ok(runtime_with_url)
}

#[derive(Clone)]
struct ClaiMcpService {
    runtime: Arc<LocalMcpRuntime>,
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for ClaiMcpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Use only these CLAI MCP tools. Do not use CLI-native filesystem or shell tools."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let binding = self.runtime.binding_from_request(&context).await?;
            let session = repository::get_session(&self.runtime.deps.pool, &binding.session_id)
                .await
                .map_err(|e| McpError::internal_error(e, None))?
                .ok_or_else(|| McpError::invalid_request("assistant session not found", None))?;

            let external_tools = {
                let state = self.runtime.deps.app.state::<AppState>();
                let mut manager = state.mcp_client_manager.lock().await;
                manager
                    .list_tools_for_servers(&session.context.mcp_server_ids)
                    .await
            };
            let tool_defs = tools::available_tools(&session.context, &external_tools);
            Ok(ListToolsResult {
                meta: None,
                tools: tool_defs.into_iter().map(tool_definition_to_mcp).collect(),
                next_cursor: None,
            })
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            let binding = self.runtime.binding_from_request(&context).await?;
            let tool_name = request.name.to_string();
            let params = request
                .arguments
                .map(serde_json::Value::Object)
                .unwrap_or(serde_json::Value::Object(Default::default()));

            match execute_bound_tool(&self.runtime.deps, &binding, &tool_name, params).await {
                Ok(value) => Ok(CallToolResult::structured(value)),
                Err(error) => Ok(CallToolResult::structured_error(serde_json::json!({
                    "error": error,
                }))),
            }
        }
    }
}

/// Execute the requested tool for a Claude Code MCP call.
///
/// This is a silent executor — it does **not** persist tool_call records,
/// emit ToolCall* UI events, or create Tool-role messages. The Claude
/// stream parser in `local_agent::handle_claude_event` owns all that
/// bookkeeping (it sees the matching `tool_use` and `tool_result` blocks
/// in the stream and uses Claude's `tool_use_id` as the canonical id, so
/// the chat UI can wire results back to the originating assistant
/// message). If this function did its own writes we'd end up with two
/// disconnected tool_call records per invocation — one with a random
/// UUID from here and one with Claude's id from the stream parser — and
/// the chat couldn't enrich the assistant's `ContentPart::ToolUse` with
/// a result.
async fn execute_bound_tool(
    deps: &AssistantDeps,
    binding: &ToolBinding,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if binding.cancel_token.is_cancelled() {
        return Err("run cancelled".to_string());
    }

    let session = repository::get_session(&deps.pool, &binding.session_id)
        .await?
        .ok_or_else(|| format!("Assistant session not found: {}", binding.session_id))?;
    let workspace_root = session
        .context
        .agent_workspace_id
        .as_deref()
        .and_then(|workspace_id| {
            deps.app
                .try_state::<AppState>()
                .and_then(|state| state.workspace_root(workspace_id))
        });

    let tool_context = ToolExecutionContext {
        session_id: binding.session_id.clone(),
        run_id: binding.run_id.clone(),
        // No tool_call_id here — the stream parser doesn't surface it
        // through the MCP transport. Inter-agent caller-id telemetry
        // therefore loses that one link for Claude-CLI sessions; the
        // tool still executes correctly.
        tool_call_id: None,
        workspace_id: session.context.workspace_id.clone(),
        space_id: session.context.space_id.clone(),
        room_id: session.context.room_id.clone(),
        mcp_server_ids: session.context.mcp_server_ids.clone(),
        agent_workspace_id: session.context.agent_workspace_id.clone(),
        workspace_root,
        automation_id: session.context.automation_id.clone(),
        workspace_agents: session.context.workspace_agents.clone(),
        inter_agent_call_depth: binding.inter_agent_call_depth,
        execution: session.context.execution.clone(),
        notices: binding.notices.clone(),
        session_grants: binding.session_grants.clone(),
    };

    tokio::select! {
        _ = binding.cancel_token.cancelled() => Err("run cancelled".to_string()),
        result = tools::execute_tool(deps, &tool_context, tool_name, params) => result,
    }
}

fn tool_definition_to_mcp(definition: ToolDefinition) -> RmcpTool {
    let input_schema = match definition.input_schema {
        serde_json::Value::Object(map) => map,
        _ => JsonObject::default(),
    };
    RmcpTool {
        name: definition.name.clone().into(),
        title: Some(definition.name),
        description: Some(definition.description.into()),
        input_schema: Arc::new(input_schema),
        output_schema: None,
        annotations: tool_annotations(),
        icons: None,
        meta: None,
    }
}

fn tool_annotations() -> Option<ToolAnnotations> {
    Some(ToolAnnotations {
        open_world_hint: Some(true),
        ..Default::default()
    })
}

fn bearer_token(context: &RequestContext<RoleServer>) -> Option<String> {
    let parts = context.extensions.get::<Parts>()?;
    let header = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    header
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

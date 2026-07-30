use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::Router;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ErrorData as McpError, JsonObject, ListToolsResult,
    PaginatedRequestParams, ProgressNotificationParam, ProgressToken, ServerCapabilities,
    ServerInfo, Tool as RmcpTool, ToolAnnotations,
};
use rmcp::service::{Peer, RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, tower::StreamableHttpService,
};
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::ServerHandler;
use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::assistant::engine::AssistantDeps;
use crate::assistant::repository;
use crate::assistant::tools::{self, ToolExecutionContext};
use crate::assistant::types::{RunNotice, ToolDefinition};
use crate::db::DbPool;
use crate::AppState;

static LOCAL_MCP_RUNTIME: OnceLock<tokio::sync::Mutex<Option<Arc<LocalMcpRuntime>>>> =
    OnceLock::new();

fn runtime_slot() -> &'static tokio::sync::Mutex<Option<Arc<LocalMcpRuntime>>> {
    LOCAL_MCP_RUNTIME.get_or_init(|| tokio::sync::Mutex::new(None))
}

#[derive(Clone)]
pub struct LocalMcpRuntime {
    url: String,
    // Only process-wide state lives on the runtime. The DB pool is
    // per-workspace and rides on each ToolBinding instead — pinning a
    // pool here would silently route every workspace's MCP calls to
    // whichever workspace happened to bind first.
    app: AppHandle,
    cancellation_token: CancellationToken,
    bindings: Arc<RwLock<HashMap<String, BoundRun>>>,
}

/// A registered run binding plus the run-scope token that reaps its
/// in-flight tool calls. [`BindingGuard::drop`] cancels `run_scope`, so
/// any tool future still racing in [`execute_bound_tool`]'s `select!`
/// when the run ends (e.g. an interactive wait orphaned by a CLI
/// transport drop) is dropped instead of lingering on the rmcp session
/// worker until its own timeout. Dropping those futures fires their
/// cleanup guards (pending-registry removal + `resolved` UI events).
#[derive(Clone)]
struct BoundRun {
    binding: ToolBinding,
    run_scope: CancellationToken,
}

#[derive(Clone)]
pub struct ToolBinding {
    /// The workspace-scoped DB pool this run's session lives in. Carried
    /// per-binding because the local MCP runtime is a process singleton
    /// while pools are per-workspace.
    pub pool: DbPool,
    pub session_id: String,
    pub run_id: String,
    pub cancel_token: CancellationToken,
    pub inter_agent_call_depth: Option<u32>,
    pub notices: Arc<Mutex<Vec<RunNotice>>>,
    /// Run-scoped grants accepted via `fs_request_grant`. Shared with the
    /// host engine so a session grant accepted during a CLI-provider run is
    /// visible to the same run's subsequent tool calls.
    pub session_grants: Arc<Mutex<Vec<crate::config::FilesystemPathGrant>>>,
    /// Run-scoped allowed command prefixes accepted via the bash approval
    /// modal. Populated from both `AllowOnce` and `AllowAlways` decisions
    /// so a single `Allow once` during a run prevents the user from being
    /// re-prompted for the same (or descendant) command for the rest of
    /// the run. `AllowAlways` is additionally persisted by the submit
    /// command into the agent's durable `allowed_command_prefixes`.
    pub session_allowed_command_prefixes: Arc<Mutex<Vec<String>>>,
    /// Run-scoped blocked command prefixes — mirror of the allow cache
    /// for `DenyAlways` decisions. Without this, a `DenyAlways` would be
    /// honored on the next session (durable persistence) but the current
    /// run would still re-prompt the user for the same command if the
    /// LLM retried, because the running `context.execution` snapshot
    /// doesn't pick up persistence. `DenyOnce` is intentionally NOT
    /// cached: that decision is one-shot by definition, and re-prompting
    /// on retry lets the user reconsider.
    pub session_blocked_command_prefixes: Arc<Mutex<Vec<String>>>,
}

impl LocalMcpRuntime {
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Register a tool binding and return an RAII guard. The binding is
    /// removed automatically when the guard is dropped — including on
    /// panic or early return — so callers cannot leak entries into the
    /// bindings map.
    pub fn bind_run(&self, binding: ToolBinding) -> BindingGuard {
        let token = Uuid::new_v4().to_string();
        let run_scope = CancellationToken::new();
        // Bindings map only ever holds short, await-free critical sections,
        // so `std::sync::RwLock` is fine and lets `Drop` clean up sync.
        // A poisoned lock means the binding map is unusable; we'd rather
        // panic here than continue with a corrupted server state.
        self.bindings
            .write()
            .expect("local MCP binding map poisoned")
            .insert(
                token.clone(),
                BoundRun {
                    binding,
                    run_scope: run_scope.clone(),
                },
            );
        BindingGuard {
            bindings: self.bindings.clone(),
            token,
            run_scope,
        }
    }

    fn binding_from_request(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<BoundRun, McpError> {
        let token = bearer_token(context).ok_or_else(|| {
            McpError::invalid_request("missing bearer token for CLAI MCP request", None)
        })?;
        self.bindings
            .read()
            .expect("local MCP binding map poisoned")
            .get(&token)
            .cloned()
            .ok_or_else(|| {
                McpError::invalid_request("invalid bearer token for CLAI MCP request", None)
            })
    }
}

/// RAII guard returned by [`LocalMcpRuntime::bind_run`]. Holds the bearer
/// token while alive and removes it from the runtime on drop, so a panic
/// or early return between bind and the end of a run cannot leak a stale
/// binding into the process-singleton MCP server.
///
/// Dropping the guard also cancels the binding's run-scope token, which
/// reaps any tool call still in flight on the rmcp session worker. This
/// matters for interactive waits (`ask_user`, bash approvals, path
/// grants) orphaned by a CLI transport drop: the worker keeps their
/// futures alive past the dropped connection, so without this reap they
/// would pin pending approval entries (and their UI cards) until their
/// own multi-minute timeout.
pub struct BindingGuard {
    bindings: Arc<RwLock<HashMap<String, BoundRun>>>,
    token: String,
    run_scope: CancellationToken,
}

impl BindingGuard {
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for BindingGuard {
    fn drop(&mut self) {
        // Reap in-flight tool calls for this run BEFORE unbinding, so a
        // racing request can't observe the binding gone while its
        // already-running future survives the run.
        self.run_scope.cancel();
        if let Ok(mut bindings) = self.bindings.write() {
            bindings.remove(&self.token);
        }
        // If the lock is poisoned we leave the (now-unreachable) entry
        // in place; the binding map is already in an unrecoverable state
        // and the process is on its way down.
    }
}

pub async fn ensure_started(app: &AppHandle) -> Result<Arc<LocalMcpRuntime>, String> {
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
        app: app.clone(),
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
            {
                // rmcp 1.7's StreamableHttpServerConfig is #[non_exhaustive]
                // (added DNS-rebinding host/origin allowlists, session store,
                // etc.), so build from Default and override only what we need.
                // The default allowed_hosts (loopback) already covers our
                // 127.0.0.1 bind, port-agnostically.
                let mut config = StreamableHttpServerConfig::default();
                config.stateful_mode = true;
                // Keep rmcp's default sse_keep_alive (15s pings). SSE
                // comments keep intermediaries from closing an idle
                // response stream, but they are not visible to the MCP
                // layer of the client. MCP clients with per-call
                // watchdogs are fed by real MCP progress notifications,
                // which `call_tool` sends periodically while a tool
                // executes (see PROGRESS_KEEPALIVE_INTERVAL).
                config.cancellation_token = cancellation_token.child_token();
                config
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
        // ServerInfo (InitializeResult) is #[non_exhaustive] in rmcp 1.7, so we
        // can't use struct-literal syntax. Default sets protocol_version to
        // rmcp's LATEST (2025-11-25), which is what Claude Code >=2.1.153
        // negotiates — the version mismatch that previously left runs tool-less.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Use only these CLAI MCP tools. Do not use CLI-native filesystem or shell tools."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let binding = self.runtime.binding_from_request(&context)?.binding;
            let session = repository::get_session(&binding.pool, &binding.session_id)
                .await
                .map_err(|e| McpError::internal_error(e, None))?
                .ok_or_else(|| McpError::invalid_request("assistant session not found", None))?;

            let external_tools = {
                let state = self.runtime.app.state::<AppState>();
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
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            let bound = self.runtime.binding_from_request(&context)?;
            let tool_name = request.name.to_string();
            let params = request
                .arguments
                .map(serde_json::Value::Object)
                .unwrap_or(serde_json::Value::Object(Default::default()));

            // Keep the CLI's per-call watchdogs fed while the tool runs.
            // Long tool calls (a 30-minute `bash_exec` build, an
            // unanswered `ask_user`) are silent at the MCP layer, so
            // without this ticker they can die as
            // "MCP server \"clai\" transport dropped mid-call" even
            // though the tool is still running. Gated on the client
            // having requested progress (MCP spec: only send progress
            // for a token the client supplied).
            let _progress_keepalive = context
                .meta
                .get_progress_token()
                .map(|token| spawn_progress_keepalive(context.peer.clone(), token));

            let result = run_until_mcp_request_cancelled(
                &context.ct,
                execute_bound_tool(
                    &self.runtime.app,
                    &bound.binding,
                    &bound.run_scope,
                    &tool_name,
                    params,
                ),
            )
            .await;

            match result {
                Ok(value) => Ok(CallToolResult::structured(value)),
                Err(error) => Ok(CallToolResult::structured_error(serde_json::json!({
                    "error": error,
                }))),
            }
        }
    }
}

/// Race an MCP request handler's work against the request-level
/// cancellation token rmcp gives us. When the client sends
/// `notifications/cancelled`, the work future is dropped and any RAII
/// guards it owns clean up immediately.
async fn run_until_mcp_request_cancelled<F, T>(
    request_scope: &CancellationToken,
    work: F,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    tokio::select! {
        _ = request_scope.cancelled() => Err("MCP request cancelled".to_string()),
        result = work => result,
    }
}

/// Execute the requested tool for a CLI-provider MCP call.
///
/// This is a silent executor — it does **not** persist tool_call records,
/// emit ToolCall* UI events, or create Tool-role messages. The Claude
/// stream parser in `local_agent::handle_claude_event` owns all that
/// bookkeeping (it sees the matching provider-side tool events and uses
/// the provider's tool id as the canonical id, so the chat UI can wire
/// results back to the originating assistant message). If this function
/// did its own writes we'd end up with two disconnected tool_call records
/// per invocation.
async fn execute_bound_tool(
    app: &AppHandle,
    binding: &ToolBinding,
    run_scope: &CancellationToken,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if binding.cancel_token.is_cancelled() {
        return Err("run cancelled".to_string());
    }

    // Rebuild AssistantDeps from the per-binding pool plus the singleton's
    // app handle, so downstream `tools::execute_tool` keeps its existing
    // `&AssistantDeps` API but operates on the correct workspace DB.
    let deps = AssistantDeps {
        pool: binding.pool.clone(),
        app: app.clone(),
    };

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
        cancel_token: binding.cancel_token.clone(),
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
        session_allowed_command_prefixes: binding.session_allowed_command_prefixes.clone(),
        session_blocked_command_prefixes: binding.session_blocked_command_prefixes.clone(),
    };

    tokio::select! {
        _ = binding.cancel_token.cancelled() => Err("run cancelled".to_string()),
        // Run-scope reap: fires when `BindingGuard` drops at the end of the
        // run. A live CLI always waits for its tool results before ending
        // its turn, so the only futures still here at that point are
        // orphans whose response stream was lost to a transport drop.
        // Dropping them fires their cleanup guards (pending-approval
        // removal, `resolved` UI events).
        _ = run_scope.cancelled() => Err("run ended before this tool call completed".to_string()),
        result = tools::execute_tool(&deps, &tool_context, tool_name, params) => result,
    }
}

/// Interval between MCP progress notifications sent while a tool call is
/// executing. Keep this comfortably below client MCP idle/watchdog
/// thresholds while adding negligible traffic on the loopback transport.
const PROGRESS_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// RAII guard for the progress keep-alive task: aborts the ticker when the
/// tool call resolves (or is dropped/cancelled), so no notification can be
/// sent after the call's result.
struct ProgressKeepaliveGuard {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for ProgressKeepaliveGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Spawn a background task that sends `notifications/progress` for `token`
/// every [`PROGRESS_KEEPALIVE_INTERVAL`] until the returned guard is
/// dropped. The session layer routes these onto the originating request's
/// own response stream, so they reach the client even without a standalone
/// GET stream.
fn spawn_progress_keepalive(
    peer: Peer<RoleServer>,
    token: ProgressToken,
) -> ProgressKeepaliveGuard {
    let handle = tokio::spawn(progress_keepalive_loop(
        PROGRESS_KEEPALIVE_INTERVAL,
        move |tick| {
            let peer = peer.clone();
            let token = token.clone();
            async move {
                peer.notify_progress(ProgressNotificationParam {
                    progress_token: token,
                    // Monotonically increasing, as the MCP spec requires
                    // even when the total is unknown.
                    progress: tick as f64,
                    total: None,
                    message: None,
                })
                .await
                .is_ok()
            }
        },
    ));
    ProgressKeepaliveGuard { handle }
}

/// Send a keep-alive every `interval` until `send` reports failure (the
/// peer is gone) or the future is dropped. The first tick fires after one
/// full interval, so short tool calls send no progress at all.
async fn progress_keepalive_loop<F, Fut>(interval: Duration, mut send: F)
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = bool>,
{
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    // If the runtime stalls past a tick, one immediate catch-up
    // notification is enough — don't burst-send the backlog.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tick: u64 = 0;
    loop {
        ticker.tick().await;
        tick += 1;
        if !send(tick).await {
            break;
        }
    }
}

fn tool_definition_to_mcp(definition: ToolDefinition) -> RmcpTool {
    let input_schema = match definition.input_schema {
        serde_json::Value::Object(map) => map,
        _ => JsonObject::default(),
    };
    // RmcpTool is #[non_exhaustive] in rmcp 1.7 (new `execution`, `icons`,
    // `meta` fields), so construct via the builder rather than a literal.
    let title = definition.name.clone();
    let mut tool = RmcpTool::new(definition.name, definition.description, input_schema);
    tool.title = Some(title);
    tool.annotations = tool_annotations();
    tool
}

fn tool_annotations() -> Option<ToolAnnotations> {
    let mut annotations = ToolAnnotations::default();
    annotations.open_world_hint = Some(true);
    Some(annotations)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_binding() -> ToolBinding {
        ToolBinding {
            pool: sqlx::Pool::connect_lazy("sqlite::memory:").expect("lazy pool"),
            session_id: "session-1".to_string(),
            run_id: "run-1".to_string(),
            cancel_token: CancellationToken::new(),
            inter_agent_call_depth: None,
            notices: Arc::new(Mutex::new(Vec::new())),
            session_grants: Arc::new(Mutex::new(Vec::new())),
            session_allowed_command_prefixes: Arc::new(Mutex::new(Vec::new())),
            session_blocked_command_prefixes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Dropping the guard must cancel the run scope (reaping in-flight
    /// tool futures racing on it in `execute_bound_tool`) and unbind the
    /// bearer token — while leaving the run's own cancel token alone, so
    /// reaping orphans at normal run end is not a run cancellation.
    #[tokio::test] // tokio: the lazy sqlx pool requires a runtime context on drop
    async fn binding_guard_drop_cancels_run_scope_and_unbinds() {
        let bindings: Arc<RwLock<HashMap<String, BoundRun>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let binding = test_binding();
        let run_cancel = binding.cancel_token.clone();
        let run_scope = CancellationToken::new();
        bindings.write().unwrap().insert(
            "token-1".to_string(),
            BoundRun {
                binding,
                run_scope: run_scope.clone(),
            },
        );
        let guard = BindingGuard {
            bindings: bindings.clone(),
            token: "token-1".to_string(),
            run_scope: run_scope.clone(),
        };

        assert!(!run_scope.is_cancelled());
        drop(guard);

        assert!(
            run_scope.is_cancelled(),
            "guard drop must reap in-flight tool calls via the run scope"
        );
        assert!(
            bindings.read().unwrap().is_empty(),
            "guard drop must unbind the bearer token"
        );
        assert!(
            !run_cancel.is_cancelled(),
            "reaping at run end must not look like a run cancellation"
        );
    }

    /// The keep-alive loop must stay silent for short calls (first tick
    /// only after one full interval), then send monotonically increasing
    /// progress values every interval.
    #[tokio::test(start_paused = true)]
    async fn progress_keepalive_loop_ticks_at_interval() {
        let sent: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = sent.clone();
        let task = tokio::spawn(progress_keepalive_loop(
            Duration::from_secs(20),
            move |tick| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push(tick);
                    true
                }
            },
        ));

        // Just under one interval: a fast tool call sends nothing.
        tokio::time::sleep(Duration::from_secs(19)).await;
        assert!(
            sent.lock().unwrap().is_empty(),
            "no progress before the first interval"
        );

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            *sent.lock().unwrap(),
            vec![1],
            "first tick after one interval"
        );

        tokio::time::sleep(Duration::from_secs(40)).await;
        assert_eq!(
            *sent.lock().unwrap(),
            vec![1, 2, 3],
            "one monotonically increasing tick per interval"
        );
        task.abort();
    }

    /// When the peer send reports failure, the loop must stop instead of
    /// spinning against a dead channel.
    #[tokio::test(start_paused = true)]
    async fn progress_keepalive_loop_stops_when_send_fails() {
        let sent: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = sent.clone();
        let task = tokio::spawn(progress_keepalive_loop(
            Duration::from_secs(20),
            move |tick| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push(tick);
                    tick < 2 // second send reports failure
                }
            },
        ));

        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(
            *sent.lock().unwrap(),
            vec![1, 2],
            "loop must terminate after the failed send"
        );
        assert!(task.is_finished(), "loop future must have returned");
    }

    /// Dropping the guard must abort the ticker so no progress can be sent
    /// after the tool call has resolved.
    #[tokio::test(start_paused = true)]
    async fn progress_keepalive_guard_drop_aborts_ticker() {
        let sent: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = sent.clone();
        let handle = tokio::spawn(progress_keepalive_loop(
            Duration::from_secs(20),
            move |tick| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push(tick);
                    true
                }
            },
        ));
        let guard = ProgressKeepaliveGuard { handle };

        tokio::time::sleep(Duration::from_secs(21)).await;
        assert_eq!(*sent.lock().unwrap(), vec![1]);

        drop(guard);
        // Give the abort a chance to land, then advance well past more ticks.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(
            *sent.lock().unwrap(),
            vec![1],
            "no progress may be sent after the guard is dropped"
        );
    }

    /// MCP request cancellation must drop the racing tool future, which
    /// also drops the progress keepalive guard owned by `call_tool`.
    #[tokio::test]
    async fn mcp_request_cancel_drops_in_flight_future() {
        struct DropFlag(Arc<Mutex<bool>>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                *self.0.lock().unwrap() = true;
            }
        }

        let dropped = Arc::new(Mutex::new(false));
        let flag = DropFlag(dropped.clone());
        let request_scope = CancellationToken::new();
        let scope = request_scope.clone();

        let task = tokio::spawn(async move {
            run_until_mcp_request_cancelled(&scope, async move {
                let _flag = flag; // owned by the racing future, dropped with it
                std::future::pending::<Result<(), String>>().await
            })
            .await
        });

        request_scope.cancel();
        let result = task.await.expect("select task must not panic");
        assert_eq!(result.unwrap_err(), "MCP request cancelled");
        assert!(
            *dropped.lock().unwrap(),
            "the in-flight future must be dropped so its RAII cleanup guards fire"
        );
    }

    /// The reap arm in `execute_bound_tool` must drop the racing tool
    /// future (firing its cleanup guards), not merely resolve alongside it.
    #[tokio::test]
    async fn run_scope_cancel_drops_in_flight_future() {
        struct DropFlag(Arc<Mutex<bool>>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                *self.0.lock().unwrap() = true;
            }
        }

        let dropped = Arc::new(Mutex::new(false));
        let flag = DropFlag(dropped.clone());
        let run_scope = CancellationToken::new();
        let scope = run_scope.clone();

        let task = tokio::spawn(async move {
            let _flag = flag; // owned by the racing future, dropped with it
            tokio::select! {
                _ = scope.cancelled() => Err::<(), String>("run ended before this tool call completed".to_string()),
                _ = std::future::pending::<()>() => Ok(()), // never resolves, like an unanswered approval
            }
        });

        run_scope.cancel();
        let result = task.await.expect("select task must not panic");
        assert_eq!(
            result.unwrap_err(),
            "run ended before this tool call completed"
        );
        assert!(
            *dropped.lock().unwrap(),
            "the in-flight future must be dropped so its RAII cleanup guards fire"
        );
    }
}

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use rmcp::{
    model::{
        CallToolRequestParam, CallToolResult, Content as McpContent, ResourceContents,
        Tool as RmcpTool,
    },
    service::{RoleClient, RunningService, ServiceExt},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStderr, Command},
};

use crate::assistant::auth::McpSecretStorage;
use crate::assistant::types::ToolDefinition;
use crate::config::{ClaiConfig, McpServerAuth, McpServerConfig};

/// MCP tool discovered from an external server.
///
/// Discovery and execution will be implemented in a follow-up slice; this
/// foundation keeps the runtime registry shape stable now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalMcpToolDefinition {
    pub server_id: String,
    pub tool_name: String,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub input_schema: serde_json::Value,
}

impl ExternalMcpToolDefinition {
    /// Stable assistant-visible tool name.
    pub fn qualified_name(&self) -> String {
        format!("mcp.{}.{}", self.server_id, self.tool_name)
    }

    pub fn to_tool_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.qualified_name(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }

    fn from_rmcp_tool(server_id: &str, tool: RmcpTool) -> Self {
        let tool_name = tool.name.to_string();
        let display_name = tool.title.unwrap_or_else(|| tool_name.clone());
        let description = tool
            .description
            .map(|value| value.into_owned())
            .unwrap_or_else(|| format!("MCP tool `{}`", tool_name));

        Self {
            server_id: server_id.to_string(),
            tool_name,
            display_name,
            description,
            input_schema: serde_json::Value::Object(tool.input_schema.as_ref().clone()),
        }
    }
}

struct StdioMcpServerConnection {
    service: RunningService<RoleClient, ()>,
    #[allow(dead_code)]
    child: Child,
}

enum ConnectedMcpServer {
    Http(RunningService<RoleClient, ()>),
    Stdio(StdioMcpServerConnection),
}

impl ConnectedMcpServer {
    fn service(&self) -> &RunningService<RoleClient, ()> {
        match self {
            ConnectedMcpServer::Http(service) => service,
            ConnectedMcpServer::Stdio(connection) => &connection.service,
        }
    }

    fn is_transport_closed(&self) -> bool {
        self.service().peer().is_transport_closed()
    }

    async fn list_all_tools(&self) -> Result<Vec<RmcpTool>, String> {
        self.service()
            .list_all_tools()
            .await
            .map_err(|error| format!("Failed to list MCP tools: {}", error))
    }

    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, String> {
        self.service()
            .call_tool(CallToolRequestParam {
                name: tool_name.to_string().into(),
                arguments,
            })
            .await
            .map_err(|error| format!("Failed to call MCP tool `{}`: {}", tool_name, error))
    }
}

struct ManagedMcpServer {
    config: McpServerConfig,
    discovered_tools: Vec<ExternalMcpToolDefinition>,
    connection: Option<ConnectedMcpServer>,
}

/// Central registry for user-configured external MCP servers.
///
/// The manager owns configured servers and, in future slices, will own active
/// client transports plus cached `tools/list` results.
#[derive(Default)]
pub struct McpClientManager {
    servers: HashMap<String, ManagedMcpServer>,
}

impl McpClientManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Synchronize configured servers from persisted app config.
    pub fn sync_from_config(&mut self, config: &ClaiConfig) {
        let mut next_servers = HashMap::new();

        for server in &config.mcp_servers {
            match self.servers.remove(&server.id) {
                Some(existing) if existing.config == *server => {
                    next_servers.insert(server.id.clone(), existing);
                }
                _ => {
                    next_servers.insert(
                        server.id.clone(),
                        ManagedMcpServer {
                            config: server.clone(),
                            discovered_tools: Vec::new(),
                            connection: None,
                        },
                    );
                }
            }
        }

        self.servers = next_servers;
    }

    /// Returns configured external tools for the selected server IDs.
    pub async fn list_tools_for_servers(&mut self, server_ids: &[String]) -> Vec<ToolDefinition> {
        let mut tools = Vec::new();

        for server_id in server_ids {
            match self.ensure_server_tools_discovered(server_id).await {
                Ok(discovered_tools) => {
                    tools.extend(
                        discovered_tools
                            .into_iter()
                            .map(|tool| tool.to_tool_definition()),
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        server_id = %server_id,
                        error = %error,
                        "Failed to discover MCP server tools"
                    );
                }
            }
        }

        tools
    }

    /// Resolve a stored bearer token for a configured server, if any.
    pub fn bearer_token_for_server(&self, server_id: &str) -> Result<Option<String>, String> {
        let Some(server) = self.servers.get(server_id) else {
            return Ok(None);
        };

        match &server.config.auth {
            McpServerAuth::None => Ok(None),
            McpServerAuth::BearerToken { secret_ref } => McpSecretStorage::get_secret(secret_ref)
                .map_err(|e| format!("Failed to read MCP server credential: {}", e)),
        }
    }

    pub async fn execute_tool(
        &mut self,
        server_ids: &[String],
        tool_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let (server_id, remote_tool_name, assistant_visible_name) =
            self.resolve_tool_target(server_ids, tool_name).await?;

        let arguments = match params {
            serde_json::Value::Null => None,
            serde_json::Value::Object(map) => Some(map),
            other => {
                return Err(format!(
                    "MCP tool arguments must be a JSON object, got {}",
                    other
                ));
            }
        };

        let result = {
            let server = self
                .servers
                .get(&server_id)
                .ok_or_else(|| format!("MCP server not found: {}", server_id))?;
            let connection = server
                .connection
                .as_ref()
                .ok_or_else(|| format!("MCP server not connected: {}", server.config.name))?;

            connection.call_tool(&remote_tool_name, arguments).await?
        };

        Ok(normalize_call_tool_result(
            &server_id,
            &remote_tool_name,
            &assistant_visible_name,
            result,
        ))
    }

    async fn ensure_server_tools_discovered(
        &mut self,
        server_id: &str,
    ) -> Result<Vec<ExternalMcpToolDefinition>, String> {
        let reconnected = self.ensure_connected(server_id).await?;
        let should_refresh = {
            let server = self
                .servers
                .get(server_id)
                .ok_or_else(|| format!("MCP server not found: {}", server_id))?;
            reconnected || server.discovered_tools.is_empty()
        };

        if should_refresh {
            self.refresh_server_tools(server_id).await
        } else {
            Ok(self
                .servers
                .get(server_id)
                .map(|server| server.discovered_tools.clone())
                .unwrap_or_default())
        }
    }

    async fn ensure_connected(&mut self, server_id: &str) -> Result<bool, String> {
        let (needs_connect, config, bearer_token) = {
            let server = self
                .servers
                .get(server_id)
                .ok_or_else(|| format!("MCP server not found: {}", server_id))?;

            if !server.config.enabled {
                return Err(format!("MCP server is disabled: {}", server.config.name));
            }

            let needs_connect = server
                .connection
                .as_ref()
                .map(ConnectedMcpServer::is_transport_closed)
                .unwrap_or(true);

            let bearer_token = match &server.config.auth {
                McpServerAuth::None => None,
                McpServerAuth::BearerToken { secret_ref } => {
                    McpSecretStorage::get_secret(secret_ref)
                        .map_err(|e| format!("Failed to read MCP server credential: {}", e))?
                }
            };

            (needs_connect, server.config.clone(), bearer_token)
        };

        if !needs_connect {
            return Ok(false);
        }

        let connection = Self::connect_server(&config, bearer_token).await?;
        let server = self
            .servers
            .get_mut(server_id)
            .ok_or_else(|| format!("MCP server not found: {}", server_id))?;
        server.connection = Some(connection);
        Ok(true)
    }

    async fn refresh_server_tools(
        &mut self,
        server_id: &str,
    ) -> Result<Vec<ExternalMcpToolDefinition>, String> {
        let tools = {
            let server = self
                .servers
                .get(server_id)
                .ok_or_else(|| format!("MCP server not found: {}", server_id))?;
            let connection = server
                .connection
                .as_ref()
                .ok_or_else(|| format!("MCP server not connected: {}", server.config.name))?;

            connection.list_all_tools().await?
        };

        let discovered_tools = tools
            .into_iter()
            .map(|tool| ExternalMcpToolDefinition::from_rmcp_tool(server_id, tool))
            .collect::<Vec<_>>();

        if let Some(server) = self.servers.get_mut(server_id) {
            server.discovered_tools = discovered_tools.clone();
        }

        Ok(discovered_tools)
    }

    async fn resolve_tool_target(
        &mut self,
        server_ids: &[String],
        tool_name: &str,
    ) -> Result<(String, String, String), String> {
        if let Some((server_id, remote_tool_name)) = parse_qualified_tool_name(tool_name) {
            if !server_ids.iter().any(|candidate| candidate == &server_id) {
                return Err(format!(
                    "MCP tool `{}` is not allowed for this session",
                    tool_name
                ));
            }

            let discovered_tools = self.ensure_server_tools_discovered(&server_id).await?;
            if !discovered_tools
                .iter()
                .any(|tool| tool.tool_name == remote_tool_name)
            {
                return Err(format!(
                    "MCP server `{}` does not expose tool `{}`",
                    server_id, remote_tool_name
                ));
            }

            return Ok((server_id, remote_tool_name, tool_name.to_string()));
        }

        let mut matches = Vec::new();
        for server_id in server_ids {
            let discovered_tools = self.ensure_server_tools_discovered(server_id).await?;
            if discovered_tools
                .iter()
                .any(|tool| tool.tool_name == tool_name)
            {
                matches.push(server_id.clone());
            }
        }

        match matches.len() {
            0 => Err(format!("Unknown external MCP tool: {}", tool_name)),
            1 => {
                let server_id = matches.remove(0);
                Ok((
                    server_id.clone(),
                    tool_name.to_string(),
                    format!("mcp.{}.{}", server_id, tool_name),
                ))
            }
            _ => Err(format!(
                "Ambiguous MCP tool `{}`: available on multiple selected servers ({})",
                tool_name,
                matches.join(", ")
            )),
        }
    }

    async fn connect_server(
        config: &McpServerConfig,
        bearer_token: Option<String>,
    ) -> Result<ConnectedMcpServer, String> {
        match &config.transport {
            crate::config::McpServerTransport::Http { url } => {
                let client = reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(60))
                    .build()
                    .map_err(|error| format!("Failed to build HTTP client: {}", error))?;

                let mut transport_config =
                    StreamableHttpClientTransportConfig::with_uri(url.clone());
                if let Some(token) = bearer_token {
                    transport_config = transport_config.auth_header(token);
                }

                let service = ()
                    .serve(StreamableHttpClientTransport::with_client(
                        client,
                        transport_config,
                    ))
                    .await
                    .map_err(|error| {
                        format!(
                            "Failed to connect to HTTP MCP server `{}`: {}",
                            config.name, error
                        )
                    })?;

                Ok(ConnectedMcpServer::Http(service))
            }
            crate::config::McpServerTransport::Stdio { command, args } => {
                let mut cmd = Command::new(command);
                cmd.args(args);
                cmd.stdin(Stdio::piped());
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::piped());
                cmd.kill_on_drop(true);

                let mut child = cmd.spawn().map_err(|error| {
                    format!(
                        "Failed to spawn stdio MCP server `{}` ({}): {}",
                        config.name, command, error
                    )
                })?;

                if let Some(stderr) = child.stderr.take() {
                    spawn_stderr_logger(config.id.clone(), stderr);
                }

                let stdout = child.stdout.take().ok_or_else(|| {
                    format!("Failed to capture stdout for MCP server `{}`", config.name)
                })?;
                let stdin = child.stdin.take().ok_or_else(|| {
                    format!("Failed to capture stdin for MCP server `{}`", config.name)
                })?;

                let service = ().serve((stdout, stdin)).await.map_err(|error| {
                    format!(
                        "Failed to initialize stdio MCP server `{}`: {}",
                        config.name, error
                    )
                })?;

                Ok(ConnectedMcpServer::Stdio(StdioMcpServerConnection {
                    service,
                    child,
                }))
            }
        }
    }
}

fn parse_qualified_tool_name(tool_name: &str) -> Option<(String, String)> {
    let raw = tool_name.strip_prefix("mcp.")?;
    let (server_id, remote_tool_name) = raw.split_once('.')?;
    Some((server_id.to_string(), remote_tool_name.to_string()))
}

fn normalize_call_tool_result(
    server_id: &str,
    tool_name: &str,
    qualified_tool_name: &str,
    result: CallToolResult,
) -> serde_json::Value {
    let text = extract_text_from_contents(&result.content);
    let content = serde_json::to_value(&result.content).unwrap_or_else(|_| serde_json::json!([]));

    serde_json::json!({
        "serverId": server_id,
        "toolName": tool_name,
        "qualifiedToolName": qualified_tool_name,
        "isError": result.is_error.unwrap_or(false),
        "structuredContent": result.structured_content,
        "content": content,
        "text": text,
    })
}

fn extract_text_from_contents(contents: &[McpContent]) -> String {
    contents
        .iter()
        .filter_map(|content| {
            if let Some(text) = content.as_text() {
                return Some(text.text.clone());
            }

            content
                .as_resource()
                .and_then(|resource| match &resource.resource {
                    ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
                    _ => None,
                })
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn spawn_stderr_logger(server_id: String, stderr: ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => tracing::debug!(
                    server_id = %server_id,
                    line = %line,
                    "MCP stdio server stderr"
                ),
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(
                        server_id = %server_id,
                        error = %error,
                        "Failed reading MCP stdio server stderr"
                    );
                    break;
                }
            }
        }
    });
}

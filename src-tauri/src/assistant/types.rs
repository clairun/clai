use serde::{Deserialize, Serialize};

use crate::config::ExecutionCapabilityConfig;

pub type SessionId = String;
pub type MessageId = String;
pub type RunId = String;
pub type ToolCallId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolFamily {
    OpenAiCompatible,
    Anthropic,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    SubscriptionLogin,
    SubscriptionApiKey,
    DeveloperApiKey,
    WorkspaceToken,
}

impl std::fmt::Display for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthMode::SubscriptionLogin => write!(f, "subscription_login"),
            AuthMode::SubscriptionApiKey => write!(f, "subscription_api_key"),
            AuthMode::DeveloperApiKey => write!(f, "developer_api_key"),
            AuthMode::WorkspaceToken => write!(f, "workspace_token"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Interactive,
    BackgroundJob,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    WaitingForTool,
    Completed,
    CompletedWithWarnings,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunNoticeKind {
    CommandDenied,
    PathDenied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunNotice {
    pub kind: RunNoticeKind,
    pub message: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    UserMessage,
    Retry,
    Scheduled,
    ManualAutomation,
    InterAgentCall,
    WorkspaceTask,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InterAgentCallContext {
    pub call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_agent_id: Option<String>,
    pub caller_session_id: String,
    pub caller_run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_tool_call_id: Option<String>,
    pub callee_agent_id: String,
    pub exposed_tool_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgentSummary {
    pub id: String,
    pub agent_definition_id: String,
    pub display_name: String,
    pub role: String,
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub tool_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_server_ids: Vec<String>,
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub netdata_conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inter_agent_call: Option<InterAgentCallContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_agents: Vec<WorkspaceAgentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSession {
    pub id: SessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub kind: SessionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub context: SessionContext,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    ToolUse {
        tool_call_id: ToolCallId,
        tool_name: String,
        #[serde(default)]
        arguments: serde_json::Value,
    },
    ToolResult {
        tool_call_id: ToolCallId,
        payload: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completed_at: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub id: MessageId,
    pub session_id: SessionId,
    pub role: MessageRole,
    pub content: Vec<ContentPart>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRun {
    pub id: RunId,
    pub session_id: SessionId,
    pub status: RunStatus,
    pub trigger: RunTrigger,
    pub connection_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<RunUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<RunNotice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvocation {
    pub id: ToolCallId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub tool_name: String,
    pub params: serde_json::Value,
    pub status: ToolCallStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    pub id: String,
    pub name: String,
    pub provider_id: String,
    pub auth_mode: AuthMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub secret_ref: String,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    pub protocol_family: ProtocolFamily,
    #[serde(default)]
    pub supported_auth_modes: Vec<AuthMode>,
    pub configurable_base_url: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub supports_tools: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInputMessage {
    pub role: MessageRole,
    pub content: Vec<ContentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct CompletionRequest {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub model_id: String,
    pub messages: Vec<ProviderInputMessage>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvocationDraft {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderEvent {
    MessageStart,
    TextDelta {
        text: String,
    },
    ToolCallDelta {
        tool_call_id: ToolCallId,
        partial: serde_json::Value,
    },
    ToolCallReady {
        tool_call: ToolInvocationDraft,
    },
    MessageComplete,
    Usage {
        usage: RunUsage,
    },
    ProviderError {
        message: String,
    },
}

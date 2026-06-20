use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::config::ExecutionCapabilityConfig;

pub type SessionId = String;
pub type MessageId = String;
pub type RunId = String;
pub type ToolCallId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ProtocolFamily {
    OpenAiCompatible,
    Anthropic,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum SessionKind {
    Interactive,
    BackgroundJob,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum RunStatus {
    Queued,
    Running,
    WaitingForTool,
    Completed,
    CompletedWithWarnings,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum RunNoticeKind {
    CommandDenied,
    PathDenied,
    SandboxUnavailable,
    /// A `fs_request_grant` was approved by the user. Carries the granted
    /// path and access level in the message so the run record shows what
    /// was extended.
    PathGranted,
    /// A `fs_request_grant` was denied (explicit deny, timeout, or channel
    /// close). Same shape as the existing `PathDenied` but distinguishes
    /// "path was outside grants" from "grant request was refused."
    PathGrantDenied,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct RunNotice {
    pub kind: RunNoticeKind,
    pub message: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum RunTrigger {
    UserMessage,
    Retry,
    Scheduled,
    ManualAutomation,
    InterAgentCall,
    WorkspaceTask,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceAgentSummary {
    pub id: String,
    pub agent_definition_id: String,
    pub display_name: String,
    pub role: String,
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct SessionContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub tool_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_server_ids: Vec<String>,
    // ExecutionCapabilityConfig has a deep tree (sandbox/fs/shell/web
    // configs each with sub-types). The FE doesn't deeply consume it in
    // event payloads, so we erase it to `unknown` here to avoid pulling
    // the entire config module into the generated bindings.
    #[serde(default)]
    #[ts(type = "unknown")]
    pub execution: ExecutionCapabilityConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_session_id: Option<String>,
    /// The CLI provider that owns `cli_session_id`. A session id is
    /// provider-specific (Claude generates its own UUID; Codex returns a
    /// server-side thread id), so switching providers must NOT reuse it.
    /// `None` for sessions created before this was tracked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_session_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inter_agent_call: Option<InterAgentCallContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_agents: Vec<WorkspaceAgentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AssistantSession {
    pub id: SessionId,
    pub kind: SessionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub context: SessionContext,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// Model "reasoning" / "thinking" content. OpenAI's o1/kimi/etc.
    /// stream this via `choices[0].delta.reasoning_content`; Anthropic
    /// streams it via `thinking` content blocks. We store it as a
    /// first-class content part so we can echo it back to providers
    /// that require it (LiteLLM-fronted OpenAI rejects assistant
    /// tool_call messages with `thinking enabled but reasoning_content
    /// missing` when this is absent).
    ///
    /// `signature` carries the Anthropic extended-thinking signature
    /// (from `signature_delta`). It must be replayed verbatim alongside
    /// the text when this block is sent back in history — required for
    /// tool-use turns, and improves output quality for MiniMax over the
    /// Anthropic protocol. `None` for providers that don't sign thinking
    /// (OpenAI `reasoning_content`, CLI-backed agents). Thinking blocks are
    /// kept in arrival order and each retains its own signature, so a turn
    /// with multiple (or interleaved) thinking blocks round-trips correctly.
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
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
    /// A user-attached image. Bytes live on disk (see `assistant::images`);
    /// this part only carries a lightweight reference so `content_json` and
    /// the DB stay small and history replay does not re-embed megabytes of
    /// base64. CLI transports pass `path` directly (codex `--image`, claude
    /// image block); HTTP API transports read the file and base64-encode at
    /// send time. Gated by `connection_supports_images` — never constructed
    /// for a connection whose active model lacks vision.
    Image {
        /// Stable id for this image part (also the on-disk file stem).
        id: String,
        /// Path to the stored file, relative to the image-store root.
        path: String,
        /// MIME type, e.g. `image/png`.
        media_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AssistantMessage {
    pub id: MessageId,
    pub session_id: SessionId,
    pub role: MessageRole,
    pub content: Vec<ContentPart>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AssistantMessageCursor {
    pub session_id: SessionId,
    pub created_at: i64,
    pub message_id: MessageId,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AssistantMessagePage {
    pub messages: Vec<AssistantMessage>,
    #[serde(default)]
    pub tool_calls: Vec<ToolInvocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<AssistantMessageCursor>,
    pub has_more: bool,
    /// Total number of messages in the conversation (the requested session
    /// plus, when ancestors were included, its whole rotation chain) — not
    /// just this page. Lets the UI show a true conversation count while only
    /// a window of messages is loaded.
    #[serde(default)]
    pub total_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum CompactionTrigger {
    Manual,
    Automatic,
    ErrorRecovery,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum CompactionStrategy {
    LocalSummary,
    SessionRotationSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum CompactionStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AssistantCompaction {
    pub id: String,
    pub session_id: SessionId,
    pub trigger: CompactionTrigger,
    pub strategy: CompactionStrategy,
    pub status: CompactionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_from_message_id: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_to_message_id: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_message_id: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_run_id: Option<RunId>,
    pub provider_id: String,
    pub model_id: String,
    pub input_message_count: i64,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ToolCallStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    pub protocol_family: ProtocolFamily,
    #[serde(default)]
    pub supported_auth_modes: Vec<AuthMode>,
    pub configurable_base_url: bool,
    #[serde(default)]
    pub is_cli_backed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub supports_tools: bool,
    /// Whether this model accepts image input (vision). Drives capability
    /// gating of the composer's image-attach affordance: the UI only offers
    /// pasting/attaching images when the active connection's selected model
    /// reports `true`. Mirrors `supports_tools`.
    #[serde(default)]
    pub supports_images: bool,
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
    /// Inline image bytes for this request, keyed by `ContentPart::Image.id`.
    /// Resolved from disk by the engine only for connections that accept images
    /// (otherwise empty). Never persisted or sent to the frontend — it carries
    /// megabytes of base64 that belong only on the outbound provider request.
    #[serde(skip, default)]
    pub images: std::collections::HashMap<String, ResolvedImage>,
}

/// An image resolved to inline base64 for a single outbound provider request.
/// Built by the engine from a `ContentPart::Image`'s on-disk file; the content
/// model itself only stores a lightweight file reference (see `image_store`).
#[derive(Debug, Clone)]
pub struct ResolvedImage {
    /// Canonical MIME type (e.g. `image/png`).
    pub media_type: String,
    /// Standard-base64-encoded file bytes.
    pub data_base64: String,
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
    /// Streamed "reasoning"/"thinking" text. Engines append to a
    /// `ContentPart::Thinking` block on the assistant message under
    /// construction. The same content gets serialized back on outbound
    /// so providers that enforce its presence (LiteLLM/OpenAI o1/kimi)
    /// accept the conversation history.
    ThinkingDelta {
        text: String,
    },
    /// Cryptographic signature for the preceding thinking block
    /// (Anthropic `signature_delta`). Arrives once, after the thinking
    /// text. Engines attach it to the finalized `ContentPart::Thinking`
    /// so it can be replayed verbatim in history.
    ThinkingSignature {
        signature: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_content_part_round_trips_with_type_tag() {
        let part = ContentPart::Image {
            id: "img-1".to_string(),
            path: "imgs/img-1.png".to_string(),
            media_type: "image/png".to_string(),
            filename: Some("paste.png".to_string()),
            width: Some(640),
            height: None,
        };
        let json = serde_json::to_value(&part).unwrap();
        // Internally tagged + snake_case: variant serializes as {"type":"image",...}.
        assert_eq!(json["type"], "image");
        assert_eq!(json["media_type"], "image/png");
        // None fields are skipped, present fields survive.
        assert_eq!(json["filename"], "paste.png");
        assert_eq!(json["width"], 640);
        assert!(json.get("height").is_none());

        let back: ContentPart = serde_json::from_value(json).unwrap();
        match back {
            ContentPart::Image {
                id,
                media_type,
                height,
                ..
            } => {
                assert_eq!(id, "img-1");
                assert_eq!(media_type, "image/png");
                assert_eq!(height, None);
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }
}

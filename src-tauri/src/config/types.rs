//! Configuration types for CLAI.
//!
//! This module defines the configuration structures that are persisted
//! to disk and shared across the application.

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_restricted_shell_blocklist() -> Vec<String> {
    vec![
        "rm".to_string(),
        "sudo".to_string(),
        "chmod".to_string(),
        "chown".to_string(),
        "dd".to_string(),
        "mkfs".to_string(),
        "mount".to_string(),
        "umount".to_string(),
        "shutdown".to_string(),
        "reboot".to_string(),
    ]
}

// =============================================================================
// AI Provider
// =============================================================================

/// Supported AI providers for agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AiProvider {
    OpenCode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Claude {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Gemini {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Codex {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Custom {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

impl AiProvider {
    /// Returns the CLI command for this provider.
    pub fn command(&self) -> &str {
        match self {
            AiProvider::OpenCode { .. } => "opencode",
            AiProvider::Claude { .. } => "claude",
            AiProvider::Gemini { .. } => "gemini",
            AiProvider::Codex { .. } => "codex",
            AiProvider::Custom { command, .. } => command,
        }
    }

    /// Returns a human-readable name for this provider.
    pub fn display_name(&self) -> &str {
        match self {
            AiProvider::OpenCode { .. } => "OpenCode",
            AiProvider::Claude { .. } => "Claude Code",
            AiProvider::Gemini { .. } => "Gemini CLI",
            AiProvider::Codex { .. } => "OpenAI Codex",
            AiProvider::Custom { command, .. } => command,
        }
    }

    /// Returns the selected model (if any).
    pub fn model(&self) -> Option<&str> {
        match self {
            AiProvider::OpenCode { model } => model.as_deref(),
            AiProvider::Claude { model } => model.as_deref(),
            AiProvider::Gemini { model } => model.as_deref(),
            AiProvider::Codex { model } => model.as_deref(),
            AiProvider::Custom { model, .. } => model.as_deref(),
        }
    }

    /// Returns a new provider with the specified model.
    pub fn with_model(self, model: Option<String>) -> Self {
        match self {
            AiProvider::OpenCode { .. } => AiProvider::OpenCode { model },
            AiProvider::Claude { .. } => AiProvider::Claude { model },
            AiProvider::Gemini { .. } => AiProvider::Gemini { model },
            AiProvider::Codex { .. } => AiProvider::Codex { model },
            AiProvider::Custom { command, args, .. } => AiProvider::Custom {
                command,
                args,
                model,
            },
        }
    }

    /// Returns the provider type as a string.
    pub fn provider_type(&self) -> &str {
        match self {
            AiProvider::OpenCode { .. } => "opencode",
            AiProvider::Claude { .. } => "claude",
            AiProvider::Gemini { .. } => "gemini",
            AiProvider::Codex { .. } => "codex",
            AiProvider::Custom { .. } => "custom",
        }
    }
}

// =============================================================================
// MCP Server Config
// =============================================================================

/// User-configured MCP server transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Http {
        url: String,
    },
}

/// Auth mode for a configured MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerAuth {
    #[default]
    None,
    BearerToken {
        secret_ref: String,
    },
}

/// Optional integration classification for a configured MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpServerIntegrationType {
    #[default]
    Generic,
    NetdataCloud,
}

/// User-configured MCP server definition persisted in app config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub transport: McpServerTransport,
    #[serde(default)]
    pub auth: McpServerAuth,
    #[serde(default)]
    pub integration_type: McpServerIntegrationType,
    pub created_at: String,
    pub updated_at: String,
}

// =============================================================================
// Local Execution Capability Config
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemPathAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemPathGrant {
    pub path: String,
    pub access: FilesystemPathAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShellAccessMode {
    #[default]
    Off,
    Restricted,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemCapabilityConfig {
    #[serde(default)]
    pub extra_paths: Vec<FilesystemPathGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellCapabilityConfig {
    #[serde(default)]
    pub mode: ShellAccessMode,
    #[serde(default)]
    pub allowed_command_prefixes: Vec<String>,
    #[serde(default = "default_restricted_shell_blocklist")]
    pub blocked_command_prefixes: Vec<String>,
}

impl Default for ShellCapabilityConfig {
    fn default() -> Self {
        Self {
            mode: ShellAccessMode::Off,
            allowed_command_prefixes: Vec::new(),
            blocked_command_prefixes: default_restricted_shell_blocklist(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WebCapabilityConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionCapabilityConfig {
    #[serde(default)]
    pub filesystem: FilesystemCapabilityConfig,
    #[serde(default)]
    pub shell: ShellCapabilityConfig,
    #[serde(default)]
    pub web: WebCapabilityConfig,
}

impl McpServerConfig {
    /// Creates a new MCP server with a generated UUID.
    pub fn new(name: String, transport: McpServerTransport) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            enabled: true,
            transport,
            auth: McpServerAuth::None,
            integration_type: McpServerIntegrationType::Generic,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

// =============================================================================
// Skill Config
// =============================================================================

/// Configured source of reusable skills.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillSourceKind {
    Local {
        path: String,
    },
    Git {
        uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_path: Option<String>,
    },
}

/// User-configured skill source persisted in app config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillSourceConfig {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub source: SkillSourceKind,
    pub created_at: String,
    pub updated_at: String,
}

impl SkillSourceConfig {
    pub fn new_local(name: String, path: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            enabled: true,
            source: SkillSourceKind::Local { path },
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn new_git(
        name: String,
        uri: String,
        reference: Option<String>,
        local_path: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            enabled: true,
            source: SkillSourceKind::Git {
                uri,
                reference,
                local_path,
            },
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

// =============================================================================
// Automation Config
// =============================================================================

/// Exposed tool that an agent makes callable to siblings via inter-agent calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExposedAgentTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
}

/// User-defined scheduled automation stored in configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique identifier (UUID).
    pub id: String,

    /// FK to the workspace this agent belongs to (`workspaces.id`).
    /// Populated by the DB load path; new in-memory constructions
    /// default to empty — callers that need cross-agent scoping (e.g.,
    /// the inline approval card binding) must source this from the DB
    /// load, not from `AgentConfig::new()`.
    #[serde(default)]
    pub workspace_id: String,

    /// Display name.
    pub name: String,

    /// Description/instructions for the automation.
    pub description: String,

    /// Whether this automation participates in scheduled execution.
    #[serde(default = "default_true")]
    pub schedule_enabled: bool,

    /// How often the automation runs (in minutes).
    pub interval_minutes: u32,

    /// Whether this automation is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// User-selected MCP servers available to this automation.
    #[serde(default)]
    pub selected_mcp_server_ids: Vec<String>,

    /// Ordered provider connections for this automation.
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,

    /// Selected reusable skills.
    #[serde(default)]
    pub selected_skill_ids: Vec<String>,

    /// Local execution capability policy for this automation.
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,

    /// Tools intentionally exposed to other agents.
    #[serde(default)]
    pub exposed_tools: Vec<ExposedAgentTool>,

    /// When the automation was created (ISO 8601).
    pub created_at: String,

    /// When the automation was last modified (ISO 8601).
    pub updated_at: String,
}

// AgentConfig is the in-memory row shape for `workspace_agents`. The helpers
// below are used by tests and by commands::workspace_agents validation logic
// (via separate copies); marked `allow(dead_code)` because rustc can't see
// across that boundary.
#[allow(dead_code)]
impl AgentConfig {
    /// Creates a new workspace-local agent with a generated UUID.
    pub fn new(name: String, description: String, interval_minutes: u32) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            workspace_id: String::new(),
            name,
            description,
            schedule_enabled: true,
            interval_minutes,
            enabled: false,
            selected_mcp_server_ids: vec![],
            provider_connection_ids: vec![],
            selected_skill_ids: vec![],
            execution: ExecutionCapabilityConfig::default(),
            exposed_tools: vec![],
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Returns the static list of required built-in tool namespaces.
    pub fn required_tools(&self) -> Vec<&'static str> {
        let mut tools = vec!["netdata", "dashboard", "tabs"];
        tools.push("fs");
        if !matches!(self.execution.shell.mode, ShellAccessMode::Off) {
            tools.push("bash");
        }
        if self.execution.web.enabled {
            tools.push("web");
        }
        tools
    }

    /// Updates the global enabled state.
    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.updated_at = chrono::Utc::now().to_rfc3339();
            true
        } else {
            false
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schedule_enabled && self.interval_minutes == 0 {
            return Err("Scheduled agents must have an interval of at least 1 minute.".to_string());
        }

        for tool in &self.exposed_tools {
            if tool.name.trim().is_empty() {
                return Err("Exposed tool names cannot be empty.".to_string());
            }
            if tool.description.trim().is_empty() {
                return Err(format!(
                    "Exposed tool '{}' must have a description.",
                    tool.name
                ));
            }
            if !tool.input_schema.is_object() {
                return Err(format!(
                    "Exposed tool '{}' must define an object JSON Schema for input_schema.",
                    tool.name
                ));
            }
            if !tool.output_schema.is_object() {
                return Err(format!(
                    "Exposed tool '{}' must define an object JSON Schema for output_schema.",
                    tool.name
                ));
            }
        }

        Ok(())
    }
}

// =============================================================================
// Root Config
// =============================================================================

/// Root configuration structure for CLAI.
///
/// Agents live in the `workspace_agents` DB table (workspace-local), not here.
/// Legacy `agents: [...]` and `assistantDefaultModel` entries in existing
/// `config.json` files are silently dropped by `#[serde(default)]` when
/// the file is deserialized.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaiConfig {
    /// Global AI provider for all automations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_provider: Option<AiProvider>,

    /// User-configured MCP servers.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// Configured skill sources.
    #[serde(default)]
    pub skill_sources: Vec<SkillSourceConfig>,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clai_config_serialization() {
        let config = ClaiConfig {
            ai_provider: Some(AiProvider::Claude { model: None }),
            ..ClaiConfig::default()
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(json.contains("claude"));

        let parsed: ClaiConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed.ai_provider,
            Some(AiProvider::Claude { .. })
        ));
    }

    #[test]
    fn test_clai_config_drops_legacy_agents_field_on_deserialize() {
        // A `config.json` written by a pre-refactor build may still have
        // `agents: [...]`. The struct no longer carries that field; serde
        // should silently drop it.
        let legacy = r#"{
            "ai_provider": {"type": "claude"},
            "agents": [{"id": "x", "name": "Old", "description": "", "intervalMinutes": 5,
                        "selectedMcpServerIds": [], "providerConnectionIds": [],
                        "selectedSkillIds": [], "execution": {},
                        "exposedTools": [], "createdAt": "", "updatedAt": ""}],
            "mcp_servers": [],
            "skill_sources": []
        }"#;
        let parsed: ClaiConfig = serde_json::from_str(legacy).unwrap();
        assert!(parsed.mcp_servers.is_empty());
    }

    #[test]
    fn test_new_agent_has_unique_id() {
        let agent1 = AgentConfig::new("Agent 1".to_string(), "Description 1".to_string(), 10);
        let agent2 = AgentConfig::new("Agent 2".to_string(), "Description 2".to_string(), 15);

        assert_ne!(agent1.id, agent2.id);
        assert_eq!(agent1.name, "Agent 1");
        assert_eq!(agent2.interval_minutes, 15);
    }

    #[test]
    fn test_agent_required_tools() {
        let agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), 5);
        let tools = agent.required_tools();

        assert!(tools.contains(&"netdata"));
        assert!(tools.contains(&"dashboard"));
        assert!(tools.contains(&"tabs"));
        assert!(tools.contains(&"fs"));
        assert_eq!(tools.len(), 4);
    }

    #[test]
    fn test_agent_required_tools_include_local_execution_when_enabled() {
        let mut agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), 5);
        agent.execution.shell.mode = ShellAccessMode::Restricted;

        let tools = agent.required_tools();

        assert!(tools.contains(&"fs"));
        assert!(tools.contains(&"bash"));
    }

    #[test]
    fn test_agent_required_tools_include_web_when_enabled() {
        let mut agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), 5);
        agent.execution.web.enabled = true;

        let tools = agent.required_tools();
        assert!(tools.contains(&"web"));

        agent.execution.web.enabled = false;
        let tools = agent.required_tools();
        assert!(!tools.contains(&"web"));
    }

    #[test]
    fn test_agent_enabled_toggle() {
        let mut agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), 5);
        agent.enabled = false;

        assert!(agent.set_enabled(true));
        assert!(agent.enabled);
        assert!(!agent.set_enabled(true));
        assert!(agent.set_enabled(false));
        assert!(!agent.enabled);
    }

    #[test]
    fn test_agent_validate_rejects_zero_interval_when_scheduled() {
        let mut agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), 0);
        agent.schedule_enabled = true;

        let err = agent.validate().unwrap_err();
        assert!(err.contains("interval"));
    }

    #[test]
    fn test_agent_validate_allows_on_demand_agent() {
        let mut agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), 0);
        agent.schedule_enabled = false;
        assert!(agent.validate().is_ok());
    }
}

//! Configuration types for CLAI.
//!
//! This module defines the configuration structures that are persisted
//! to disk and shared across the application.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::assistant::network::default_lists::{DEFAULT_ALLOW_DOMAINS, DEFAULT_DENY_DOMAINS};
use crate::assistant::network::matcher::parse_entry;
use crate::assistant::types::ProviderConnection;
use crate::paths;

fn default_true() -> bool {
    true
}

/// The default blocklist from before the ask-first list replaced it. A config
/// saved with neither list reads as if it had this one.
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

/// Ask-first rules that are not on the former default blocklist: commands
/// that publish or delete something outside this machine.
const NEW_ASK_FIRST_RULES: &[&str] = &[
    "git push",
    "gh pr merge",
    "gh release",
    "gh repo delete",
    "gh api -X DELETE",
    "npm publish",
    "pnpm publish",
    "yarn publish",
    "yarn npm publish",
    "cargo publish",
    "twine upload",
    "docker push",
    "docker image push",
    "kubectl delete",
    "terraform destroy",
];

/// The ask-first list a new agent starts with.
pub fn default_ask_first_command_prefixes() -> Vec<String> {
    ask_first_from_blocklist(&default_restricted_shell_blocklist())
}

/// The ask-first list of an agent saved before the list existed: the new
/// rules, then the agent's own blocklist. A default entry the user had
/// removed from the blocklist stays out.
fn ask_first_from_blocklist(blocked: &[String]) -> Vec<String> {
    let mut rules = Vec::new();
    for rule in NEW_ASK_FIRST_RULES
        .iter()
        .copied()
        .chain(blocked.iter().map(String::as_str))
    {
        let rule = rule.trim();
        if !rule.is_empty() {
            push_unique_prefix(&mut rules, rule.to_string());
        }
    }
    rules
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
}

// =============================================================================
// MCP Server Config
// =============================================================================

/// A single environment variable (stdio) or HTTP header (http) entry for an
/// MCP server. Secret entries store their value in the OS vault under
/// `secret_ref` and leave `value` empty; non-secret entries store the value
/// inline. See `design/mcp-env-vars-design.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct McpEnvVar {
    /// Env variable name, or HTTP header name.
    pub key: String,
    /// Plain value — present only for non-secret entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Vault reference — present only for secret entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    /// When true the value lives in the vault under `secret_ref`; otherwise
    /// the plain `value` is used.
    #[serde(default)]
    pub secret: bool,
}

/// User-configured MCP server transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum McpServerTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// Environment variables layered on top of CLAI's inherited env when
        /// the stdio subprocess is spawned.
        #[serde(default)]
        env: Vec<McpEnvVar>,
    },
    Http {
        url: String,
        /// Extra HTTP headers sent with each request (alongside any auth).
        #[serde(default)]
        headers: Vec<McpEnvVar>,
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
    #[serde(rename = "oauth")]
    OAuth {
        credential_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization_server_issuer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_metadata_url: Option<String>,
        #[serde(default)]
        scopes: Vec<String>,
    },
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
    pub created_at: String,
    pub updated_at: String,
}

// =============================================================================
// Local Execution Capability Config
// =============================================================================

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum FilesystemPathAccess {
    ReadOnly,
    ReadWrite,
}

/// Provenance for a `FilesystemPathGrant`. Lets the agent-settings UI tell
/// the user *why* a path is in the grant list: was it added by hand, derived
/// from the credentials preset, or accepted via an in-run approval modal?
/// `None` (the historical shape) is treated as `Manual` by the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum GrantOrigin {
    /// Added by the user via the agent settings form.
    Manual,
    /// Derived from `credentials_preset = true`. Not stored individually in
    /// `extra_paths` — computed at sandbox-profile build time.
    CredentialsPreset,
    /// Accepted via the in-run `fs_request_grant` approval modal.
    #[serde(rename_all = "camelCase")]
    Approval {
        reason: String,
        granted_at_unix_ms: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct FilesystemPathGrant {
    pub path: String,
    pub access: FilesystemPathAccess,
    /// Set by paths that landed in `extra_paths` via the approval flow.
    /// Pre-existing grants on disk have no provenance and deserialize as
    /// `None`; the UI shows those as `Manual`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<GrantOrigin>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ShellAccessMode {
    #[default]
    Off,
    Restricted,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct FilesystemCapabilityConfig {
    /// Additional path grants. Used for two things now that the sandbox
    /// auto-binds the user's `$HOME` read-only:
    /// 1. Read-write upgrades to specific subpaths inside `$HOME`
    ///    (e.g., a tool cache directory the agent needs to write).
    /// 2. Any access (read or write) to paths outside `$HOME` (e.g.,
    ///    `/mnt/data`, `/opt/external`).
    ///
    /// Entries land here either from explicit user edits in agent
    /// settings or from `fs_request_grant` approvals (the latter
    /// carries provenance via `GrantOrigin::Approval`).
    #[serde(default)]
    pub extra_paths: Vec<FilesystemPathGrant>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct ShellCapabilityConfig {
    pub mode: ShellAccessMode,
    /// Commands a Restricted agent asks the user about before running.
    pub ask_first_command_prefixes: Vec<String>,
}

impl Default for ShellCapabilityConfig {
    fn default() -> Self {
        Self {
            mode: ShellAccessMode::Off,
            ask_first_command_prefixes: default_ask_first_command_prefixes(),
        }
    }
}

/// [`ShellCapabilityConfig`] as stored. A config saved before the ask-first
/// list existed has none, and gets one built from its blocklist; its
/// `allowedCommandPrefixes` is ignored. Neither old list is written back.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredShellCapabilityConfig {
    #[serde(default)]
    mode: ShellAccessMode,
    #[serde(default = "default_restricted_shell_blocklist")]
    blocked_command_prefixes: Vec<String>,
    #[serde(default)]
    ask_first_command_prefixes: Option<Vec<String>>,
}

impl<'de> Deserialize<'de> for ShellCapabilityConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = StoredShellCapabilityConfig::deserialize(deserializer)?;
        let ask_first_command_prefixes = stored
            .ask_first_command_prefixes
            .unwrap_or_else(|| ask_first_from_blocklist(&stored.blocked_command_prefixes));
        Ok(Self {
            mode: stored.mode,
            ask_first_command_prefixes,
        })
    }
}

fn push_unique_prefix(prefixes: &mut Vec<String>, prefix: String) {
    if !prefixes.iter().any(|existing| existing == &prefix) {
        prefixes.push(prefix);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WebCapabilityConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// The network switch stored before [`NetworkPolicy`] existed. Still written
/// on save, derived from the policy, so an older build loads the agent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxNetworkConfig {
    Enabled,
    Disabled,
}

/// How an agent's shell commands and web tools reach the network.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum NetworkMode {
    Off,
    /// Only hosts on the allow list and not on the deny list.
    Allowlist,
    /// The host's network, unfiltered.
    HostNetwork,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct NetworkPolicy {
    pub mode: NetworkMode,
    /// Kept in every mode, so leaving Allowlist and coming back keeps the
    /// user's edits. Read only in Allowlist.
    #[serde(default = "default_allow_domains")]
    pub allow_domains: Vec<String>,
    /// Checked before the allow list.
    #[serde(default = "default_deny_domains")]
    pub deny_domains: Vec<String>,
}

impl NetworkPolicy {
    /// Refuses, naming it, an entry the matcher could never match. Every save
    /// of an agent calls this; an invalid entry already stored (a hand edit)
    /// is skipped when the lists are matched (see [`HostLists::new`]).
    ///
    /// [`HostLists::new`]: crate::assistant::network::matcher::HostLists::new
    pub fn validate(&self) -> Result<(), String> {
        for (list, entries) in [
            ("Allowed hosts", &self.allow_domains),
            ("Denied hosts", &self.deny_domains),
        ] {
            for entry in entries {
                parse_entry(entry).map_err(|reason| {
                    format!("{list}: `{entry}` is not a valid host entry: {reason}.")
                })?;
            }
        }
        Ok(())
    }
}

fn default_allow_domains() -> Vec<String> {
    DEFAULT_ALLOW_DOMAINS
        .iter()
        .map(|entry| entry.to_string())
        .collect()
}

fn default_deny_domains() -> Vec<String> {
    DEFAULT_DENY_DOMAINS
        .iter()
        .map(|entry| entry.to_string())
        .collect()
}

/// The policy of an agent stored before `networkAccess` existed, or created
/// now, with the default lists. Legacy `disabled` stays Off on every PC, so
/// it blocks the web tools even where no sandbox restricts the shell.
/// `enabled` or nothing becomes Allowlist where the sandbox can enforce it,
/// and Host network elsewhere, which is what the shell does there.
pub fn network_access_from_legacy(
    legacy: Option<SandboxNetworkConfig>,
    allowlist_available: bool,
) -> NetworkPolicy {
    let mode = match legacy {
        Some(SandboxNetworkConfig::Disabled) => NetworkMode::Off,
        Some(SandboxNetworkConfig::Enabled) | None if allowlist_available => NetworkMode::Allowlist,
        Some(SandboxNetworkConfig::Enabled) | None => NetworkMode::HostNetwork,
    };
    NetworkPolicy {
        mode,
        allow_domains: default_allow_domains(),
        deny_domains: default_deny_domains(),
    }
}

pub(crate) fn allowlist_available_here() -> bool {
    crate::assistant::sandbox::status().allowlist_available
}

/// Rewrites every sandbox config in `value` to the shape saved before
/// `networkAccess` existed, with `network` set to `legacy`.
#[cfg(test)]
pub(crate) fn to_pre_network_access_json(value: &mut serde_json::Value, legacy: &str) {
    match value {
        serde_json::Value::Object(map) => {
            if map.remove("networkAccess").is_some() {
                map.insert("network".to_string(), serde_json::json!(legacy));
            }
            map.values_mut()
                .for_each(|value| to_pre_network_access_json(value, legacy));
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .for_each(|value| to_pre_network_access_json(value, legacy)),
        _ => {}
    }
}

/// Whether the agent's sandbox can reach the user's session D-Bus.
///
/// `Allow` (the default) exposes the user's D-Bus session bus socket
/// (`$XDG_RUNTIME_DIR/bus`) inside the sandbox and lets
/// `DBUS_SESSION_BUS_ADDRESS` + `XDG_RUNTIME_DIR` through the env filter.
/// This enables every CLI that talks to libsecret / `gnome-keyring-daemon`
/// over D-Bus — most importantly `gh`, but also `git-credential-libsecret`,
/// `secret-tool`, password managers, and so on. It matches Codex's
/// "agent acts as me on my desktop" reach while CLAI's path-grant flow
/// keeps file access narrow.
///
/// `Deny` strips the bus socket bind and the corresponding env vars.
/// Use for high-isolation agents — running untrusted code, agents that
/// don't need any host integration, or workspaces where you specifically
/// want keyring contents kept out of reach.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum SandboxSessionBusConfig {
    Deny,
    #[default]
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "bindings.ts", rename_all = "camelCase")]
pub struct ExecutionSandboxConfig {
    pub network_access: NetworkPolicy,
    pub session_bus: SandboxSessionBusConfig,
}

impl Default for ExecutionSandboxConfig {
    fn default() -> Self {
        Self {
            network_access: network_access_from_legacy(None, allowlist_available_here()),
            session_bus: SandboxSessionBusConfig::default(),
        }
    }
}

impl ExecutionSandboxConfig {
    fn legacy_network(&self) -> SandboxNetworkConfig {
        match self.network_access.mode {
            NetworkMode::Off => SandboxNetworkConfig::Disabled,
            NetworkMode::Allowlist | NetworkMode::HostNetwork => SandboxNetworkConfig::Enabled,
        }
    }
}

/// [`ExecutionSandboxConfig`] as stored: the policy in `networkAccess`, and
/// the legacy `network` string an older build reads. A config without
/// `networkAccess` gets one from `network` (see [`network_access_from_legacy`]).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredExecutionSandboxConfig {
    #[serde(default)]
    network: Option<SandboxNetworkConfig>,
    #[serde(default)]
    network_access: Option<NetworkPolicy>,
    #[serde(default)]
    session_bus: SandboxSessionBusConfig,
}

impl Serialize for ExecutionSandboxConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        StoredExecutionSandboxConfig {
            network: Some(self.legacy_network()),
            network_access: Some(self.network_access.clone()),
            session_bus: self.session_bus,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExecutionSandboxConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = StoredExecutionSandboxConfig::deserialize(deserializer)?;
        Ok(Self {
            network_access: stored.network_access.unwrap_or_else(|| {
                network_access_from_legacy(stored.network, allowlist_available_here())
            }),
            session_bus: stored.session_bus,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct ExecutionCapabilityConfig {
    #[serde(default)]
    pub sandbox: ExecutionSandboxConfig,
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
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

// =============================================================================
// Skill Config
// =============================================================================

/// Configured source of reusable skills.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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

    /// How the automation's next run is computed — interval after
    /// completion, or a cron expression in a chosen timezone. Mirrors
    /// the workspace-level `WorkspaceSchedule.kind` for the manager
    /// agent; non-manager agents leave this at the default (`Interval{0}`)
    /// since only the workspace's manager runs on a schedule.
    #[serde(default)]
    pub schedule_kind: crate::config::workspace_config::ScheduleKind,

    /// Whether this automation is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Ordered provider connections for this automation.
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,

    /// Selected reusable skills.
    #[serde(default)]
    pub selected_skill_ids: Vec<String>,

    /// When the automation was created (ISO 8601).
    pub created_at: String,

    /// When the automation was last modified (ISO 8601).
    pub updated_at: String,
}

/// First entry of `provider_connection_ids` that resolves to an enabled
/// connection. Scheduled and task runs use exactly this one; there is no
/// fallback to the rest of the list.
pub fn first_enabled_connection<'c>(
    provider_connection_ids: &[String],
    connections: &'c [ProviderConnection],
) -> Option<&'c ProviderConnection> {
    provider_connection_ids.iter().find_map(|id| {
        connections
            .iter()
            .find(|connection| &connection.id == id && connection.enabled)
    })
}

#[cfg(test)]
impl AgentConfig {
    /// Creates a new workspace-local agent with a generated UUID.
    pub fn new(
        name: String,
        description: String,
        schedule_kind: crate::config::workspace_config::ScheduleKind,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            workspace_id: String::new(),
            name,
            description,
            schedule_enabled: true,
            schedule_kind,
            enabled: false,
            provider_connection_ids: vec![],
            selected_skill_ids: vec![],
            created_at: now.clone(),
            updated_at: now,
        }
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
        use crate::config::workspace_config::ScheduleKind;
        if !self.schedule_enabled {
            return Ok(());
        }
        match &self.schedule_kind {
            ScheduleKind::Interval { interval_minutes } => {
                if *interval_minutes == 0 {
                    return Err(
                        "Scheduled agents must have an interval of at least 1 minute.".to_string(),
                    );
                }
            }
            ScheduleKind::Cron { expression, .. } => {
                if expression.trim().is_empty() {
                    return Err("Cron-scheduled agents must have a cron expression.".to_string());
                }
            }
        }
        Ok(())
    }
}

// =============================================================================
// Root Config
// =============================================================================

fn default_app_config_version() -> u32 {
    1
}

fn default_workspace_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from("~/.clai/workspaces")]
}

/// Root app configuration persisted at `~/.clai/config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub agent_definitions: Vec<super::global_agents::AgentDefinition>,
    #[serde(default = "default_app_config_version")]
    pub version: u32,

    #[serde(default = "default_workspace_dirs")]
    pub workspace_dirs: Vec<PathBuf>,

    /// User-configured MCP servers.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// Configured skill sources.
    #[serde(default)]
    pub skill_sources: Vec<SkillSourceConfig>,

    /// User-configured provider connections for the app-owned assistant runtime.
    #[serde(default)]
    pub provider_connections: Vec<ProviderConnection>,

    /// Editor/terminal selections for "open in app" actions
    /// (Settings → Applications).
    #[serde(default)]
    pub system_apps: crate::system_apps::SystemAppsConfig,

    /// Global "pause all" overlay for the agent scheduler. When true, NO
    /// workspace's scheduled tick runs, regardless of its individual
    /// `schedule.paused` state — which is preserved underneath and restored
    /// when this is cleared. Persisted so a deliberate maintenance pause
    /// survives app restarts. In-flight runs are not interrupted.
    #[serde(default)]
    pub scheduler_paused: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            agent_definitions: Vec::new(),
            version: default_app_config_version(),
            workspace_dirs: default_workspace_dirs(),
            mcp_servers: Vec::new(),
            skill_sources: Vec::new(),
            provider_connections: Vec::new(),
            system_apps: crate::system_apps::SystemAppsConfig::default(),
            scheduler_paused: false,
        }
    }
}

impl AppConfig {
    pub fn expanded_workspace_dirs(&self) -> Vec<PathBuf> {
        self.workspace_dirs
            .iter()
            .map(|path| paths::expand_tilde(path))
            .collect()
    }
}

pub type ClaiConfig = AppConfig;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::AuthMode;

    fn connection(id: &str, enabled: bool) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            name: format!("conn-{id}"),
            protocol_id: "claude-code".to_string(),
            provider_id: "claude-code".to_string(),
            auth_mode: AuthMode::SubscriptionLogin,
            base_url: None,
            secret_ref: format!("provider-connection::{id}"),
            model_id: String::new(),
            account_label: None,
            enabled,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn first_of<'c>(ids: &[&str], all: &'c [ProviderConnection]) -> Option<&'c ProviderConnection> {
        let ids: Vec<String> = ids.iter().map(|id| (*id).to_string()).collect();
        first_enabled_connection(&ids, all)
    }

    #[test]
    fn scheduled_run_uses_first_enabled_connection_only() {
        let all = vec![connection("a", true), connection("b", true)];
        let chosen = first_of(&["a", "b"], &all);
        assert_eq!(chosen.map(|c| c.id.as_str()), Some("a"));

        let all = vec![connection("a", false), connection("b", true)];
        let chosen = first_of(&["a", "b"], &all);
        assert_eq!(chosen.map(|c| c.id.as_str()), Some("b"));

        let all = vec![connection("b", true), connection("c", true)];
        let chosen = first_of(&["dangling", "c", "b"], &all);
        assert_eq!(chosen.map(|c| c.id.as_str()), Some("c"));
    }

    #[test]
    fn scheduled_run_with_no_enabled_connection_fails_instance() {
        let all = vec![connection("a", false), connection("b", false)];
        assert!(first_of(&["a", "b"], &all).is_none());
        assert!(first_of(&[], &all).is_none());
        assert!(first_of(&["unknown"], &all).is_none());
        assert!(first_of(&["a"], &[]).is_none());
    }

    fn stored_shell(json: serde_json::Value) -> ShellCapabilityConfig {
        serde_json::from_value(json).expect("shell config")
    }

    #[test]
    fn default_ask_first_list_is_the_new_rules_then_the_old_blocklist() {
        let rules = default_ask_first_command_prefixes();
        assert_eq!(rules.len(), 25, "{rules:?}");
        assert_eq!(rules[0], "git push");
        assert_eq!(rules[15..], default_restricted_shell_blocklist()[..]);
        assert_eq!(
            ShellCapabilityConfig::default().ask_first_command_prefixes,
            rules
        );
    }

    #[test]
    fn the_settings_form_drafts_the_same_default_list() {
        let form = include_str!("../../../src/components/Settings/AgentBehaviorForm.tsx");
        let start = form
            .find("const DEFAULT_ASK_FIRST = [")
            .expect("the form's default list");
        let list = &form[start..];
        let list = &list[..list.find("];").expect("the end of the list")];
        let rules: Vec<&str> = list.split('\'').skip(1).step_by(2).collect();
        assert_eq!(rules, default_ask_first_command_prefixes());
    }

    #[test]
    fn a_config_without_an_ask_first_list_builds_it_from_its_blocklist() {
        let shell = stored_shell(serde_json::json!({
            "mode": "restricted",
            "allowedCommandPrefixes": ["git status"],
            "blockedCommandPrefixes": ["sudo", " curl ", "git push", ""],
        }));
        assert_eq!(
            shell.mode,
            ShellAccessMode::Restricted,
            "Restricted stays Restricted"
        );
        let mut expected: Vec<String> = NEW_ASK_FIRST_RULES.iter().map(|r| r.to_string()).collect();
        expected.extend(["sudo".to_string(), "curl".to_string()]);
        assert_eq!(
            shell.ask_first_command_prefixes, expected,
            "the new rules, then the blocklist, trimmed and without duplicates; \
             the rm the user removed from the blocklist stays out"
        );

        assert_eq!(
            serde_json::to_value(&shell).unwrap(),
            serde_json::json!({
                "mode": "restricted",
                "askFirstCommandPrefixes": expected,
            }),
            "neither old list is written back"
        );
    }

    #[test]
    fn a_config_with_neither_list_gets_the_default_ask_first_list() {
        for mode in ["off", "restricted", "full"] {
            let shell = stored_shell(serde_json::json!({ "mode": mode }));
            assert_eq!(
                shell.ask_first_command_prefixes,
                default_ask_first_command_prefixes()
            );
        }
        assert_eq!(
            stored_shell(serde_json::json!({ "mode": "full" })).mode,
            ShellAccessMode::Full
        );
    }

    #[test]
    fn a_stored_ask_first_list_is_kept_as_saved() {
        let shell = stored_shell(serde_json::json!({
            "mode": "restricted",
            "blockedCommandPrefixes": ["sudo"],
            "askFirstCommandPrefixes": [],
        }));
        assert!(
            shell.ask_first_command_prefixes.is_empty(),
            "an empty list stays empty"
        );
        let shell = stored_shell(serde_json::json!({
            "askFirstCommandPrefixes": ["git push"],
        }));
        assert_eq!(
            shell.ask_first_command_prefixes,
            vec!["git push".to_string()]
        );
        let round_trip: ShellCapabilityConfig =
            serde_json::from_str(&serde_json::to_string(&shell).unwrap()).unwrap();
        assert_eq!(round_trip, shell);
    }

    #[test]
    fn test_clai_config_serialization() {
        let config = ClaiConfig {
            scheduler_paused: true,
            ..ClaiConfig::default()
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: ClaiConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.scheduler_paused);
    }

    #[test]
    fn config_with_the_removed_ai_provider_key_still_loads() {
        let legacy = r#"{
            "aiProvider": {"type": "claude"},
            "schedulerPaused": true
        }"#;
        let parsed: ClaiConfig = serde_json::from_str(legacy).unwrap();
        assert!(parsed.scheduler_paused);
        assert!(!serde_json::to_string(&parsed)
            .unwrap()
            .contains("aiProvider"));
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
    fn config_written_before_updates_became_mandatory_keeps_its_settings() {
        // `autoUpdate.autoDownload` was a user setting until background
        // downloads became mandatory on self-updating builds. Configs on disk
        // still carry the key, so the removed field must be ignored, NOT
        // rejected: a parse error here would silently reset every real
        // setting in the file to its default. Both asserted values are
        // deliberately non-default so the test can fail.
        let legacy = r#"{
            "version": 1,
            "workspaceDirs": ["/tmp/clai-test-workspaces"],
            "mcpServers": [],
            "skillSources": [],
            "providerConnections": [],
            "schedulerPaused": true,
            "autoUpdate": { "autoDownload": false }
        }"#;
        let parsed: ClaiConfig = serde_json::from_str(legacy)
            .expect("a config carrying the removed autoUpdate key must still load");

        assert_eq!(
            parsed.workspace_dirs,
            vec![PathBuf::from("/tmp/clai-test-workspaces")],
        );
        assert_ne!(parsed.workspace_dirs, default_workspace_dirs());
        assert!(parsed.scheduler_paused);
    }

    fn interval_kind(minutes: u32) -> crate::config::workspace_config::ScheduleKind {
        crate::config::workspace_config::ScheduleKind::Interval {
            interval_minutes: minutes,
        }
    }

    #[test]
    fn test_new_agent_has_unique_id() {
        let agent1 = AgentConfig::new(
            "Agent 1".to_string(),
            "Description 1".to_string(),
            interval_kind(10),
        );
        let agent2 = AgentConfig::new(
            "Agent 2".to_string(),
            "Description 2".to_string(),
            interval_kind(15),
        );

        assert_ne!(agent1.id, agent2.id);
        assert_eq!(agent1.name, "Agent 1");
        assert!(matches!(
            agent2.schedule_kind,
            crate::config::workspace_config::ScheduleKind::Interval {
                interval_minutes: 15
            }
        ));
    }

    #[test]
    fn test_agent_enabled_toggle() {
        let mut agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), interval_kind(5));
        agent.enabled = false;

        assert!(agent.set_enabled(true));
        assert!(agent.enabled);
        assert!(!agent.set_enabled(true));
        assert!(agent.set_enabled(false));
        assert!(!agent.enabled);
    }

    #[test]
    fn test_agent_validate_rejects_zero_interval_when_scheduled() {
        let mut agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), interval_kind(0));
        agent.schedule_enabled = true;

        let err = agent.validate().unwrap_err();
        assert!(err.contains("interval"));
    }

    #[test]
    fn test_agent_validate_allows_on_demand_agent() {
        let mut agent = AgentConfig::new("Agent".to_string(), "Desc".to_string(), interval_kind(0));
        agent.schedule_enabled = false;
        assert!(agent.validate().is_ok());
    }

    #[test]
    fn test_agent_validate_rejects_empty_cron_expression() {
        let mut agent = AgentConfig::new(
            "Agent".to_string(),
            "Desc".to_string(),
            crate::config::workspace_config::ScheduleKind::Cron {
                expression: String::new(),
                timezone: "UTC".to_string(),
            },
        );
        agent.schedule_enabled = true;
        let err = agent.validate().unwrap_err();
        assert!(err.to_lowercase().contains("cron"));
    }

    #[test]
    fn mcp_transport_loads_legacy_config_without_env_or_headers() {
        // Old configs predate env/headers — they must still deserialize
        // (regression against the fail-closed loader lesson).
        let stdio: McpServerTransport =
            serde_json::from_str(r#"{"type":"stdio","command":"uvx","args":["x"]}"#).unwrap();
        match stdio {
            McpServerTransport::Stdio { command, args, env } => {
                assert_eq!(command, "uvx");
                assert_eq!(args, vec!["x".to_string()]);
                assert!(env.is_empty());
            }
            _ => panic!("expected stdio"),
        }
        let http: McpServerTransport =
            serde_json::from_str(r#"{"type":"http","url":"https://x/mcp"}"#).unwrap();
        match http {
            McpServerTransport::Http { url, headers } => {
                assert_eq!(url, "https://x/mcp");
                assert!(headers.is_empty());
            }
            _ => panic!("expected http"),
        }
    }

    #[test]
    fn mcp_env_var_round_trips_secret_and_plain_shapes() {
        // Plain: value inline, no secret_ref, not skipped.
        let plain = McpEnvVar {
            key: "LOG_LEVEL".into(),
            value: Some("debug".into()),
            secret_ref: None,
            secret: false,
        };
        let j = serde_json::to_string(&plain).unwrap();
        assert!(j.contains("\"value\":\"debug\""));
        assert!(!j.contains("secretRef"));
        assert_eq!(serde_json::from_str::<McpEnvVar>(&j).unwrap(), plain);

        // Secret: secret_ref present, value omitted.
        let secret = McpEnvVar {
            key: "API_KEY".into(),
            value: None,
            secret_ref: Some("mcp-server::abc::env::API_KEY".into()),
            secret: true,
        };
        let j = serde_json::to_string(&secret).unwrap();
        assert!(j.contains("secretRef"));
        assert!(!j.contains("\"value\""));
        assert_eq!(serde_json::from_str::<McpEnvVar>(&j).unwrap(), secret);
    }

    // ------------------------------------------------------------------
    // Network policy: migration from the legacy switch, downgrade
    // ------------------------------------------------------------------

    use crate::assistant::sandbox::{status_for, SandboxBackend};

    fn allowlist_on(backend: Option<SandboxBackend>) -> bool {
        status_for(backend).allowlist_available
    }

    #[test]
    fn legacy_network_migrates_per_platform() {
        use NetworkMode::{Allowlist, HostNetwork, Off};
        let linux = Some(SandboxBackend::Bubblewrap);
        let macos = Some(SandboxBackend::Seatbelt);
        let no_sandbox = None;
        let enabled = Some(SandboxNetworkConfig::Enabled);
        let disabled = Some(SandboxNetworkConfig::Disabled);
        for (backend, legacy, expected) in [
            (linux, enabled, Allowlist),
            (linux, None, Allowlist),
            (linux, disabled, Off),
            (macos, enabled, HostNetwork),
            (macos, None, HostNetwork),
            (macos, disabled, Off),
            (no_sandbox, enabled, HostNetwork),
            (no_sandbox, None, HostNetwork),
            (no_sandbox, disabled, Off),
        ] {
            let policy = network_access_from_legacy(legacy, allowlist_on(backend));
            assert_eq!(policy.mode, expected, "{backend:?} {legacy:?}");
            assert_eq!(policy.allow_domains, default_allow_domains());
            assert_eq!(policy.deny_domains, default_deny_domains());
        }
    }

    fn sandbox_json(value: serde_json::Value) -> ExecutionSandboxConfig {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_missing_network_access_is_derived_from_the_legacy_switch_here() {
        let here = allowlist_available_here();
        for (legacy, json) in [
            (
                Some(SandboxNetworkConfig::Enabled),
                serde_json::json!({ "network": "enabled" }),
            ),
            (
                Some(SandboxNetworkConfig::Disabled),
                serde_json::json!({ "network": "disabled" }),
            ),
            (None, serde_json::json!({})),
        ] {
            assert_eq!(
                sandbox_json(json).network_access,
                network_access_from_legacy(legacy, here)
            );
        }
        assert_eq!(
            ExecutionSandboxConfig::default().network_access,
            network_access_from_legacy(None, here)
        );
    }

    #[test]
    fn a_stored_network_access_wins_over_the_legacy_switch() {
        let config = sandbox_json(serde_json::json!({
            "network": "disabled",
            "networkAccess": { "mode": "host_network", "allowDomains": ["example.org"], "denyDomains": [] },
            "sessionBus": "deny",
        }));
        assert_eq!(
            config,
            ExecutionSandboxConfig {
                network_access: NetworkPolicy {
                    mode: NetworkMode::HostNetwork,
                    allow_domains: vec!["example.org".to_string()],
                    deny_domains: Vec::new(),
                },
                session_bus: SandboxSessionBusConfig::Deny,
            }
        );
        // Lists missing from a hand-written policy are the defaults.
        let config = sandbox_json(serde_json::json!({ "networkAccess": { "mode": "off" } }));
        assert_eq!(config.network_access.allow_domains, default_allow_domains());
        assert_eq!(config.network_access.deny_domains, default_deny_domains());
    }

    fn policy(mode: NetworkMode) -> ExecutionSandboxConfig {
        ExecutionSandboxConfig {
            network_access: NetworkPolicy {
                mode,
                allow_domains: vec!["github.com".to_string(), "*.example.org".to_string()],
                deny_domains: vec!["bad.example.org".to_string()],
            },
            session_bus: SandboxSessionBusConfig::Allow,
        }
    }

    #[test]
    fn saving_writes_the_policy_and_the_legacy_switch() {
        for (mode, legacy) in [
            (NetworkMode::Off, "disabled"),
            (NetworkMode::Allowlist, "enabled"),
            (NetworkMode::HostNetwork, "enabled"),
        ] {
            let config = policy(mode);
            let json = serde_json::to_value(&config).unwrap();
            assert_eq!(json["network"], legacy, "{mode:?}");
            assert_eq!(json["networkAccess"]["allowDomains"][1], "*.example.org");
            assert_eq!(json["sessionBus"], "allow");
            assert_eq!(sandbox_json(json), config, "{mode:?} round-trips");
        }
    }

    #[test]
    fn validation_names_the_first_invalid_entry_and_its_list() {
        let mut config = policy(NetworkMode::Allowlist);
        assert_eq!(config.network_access.validate(), Ok(()));

        config
            .network_access
            .deny_domains
            .push("S3.amazonaws.com:443".to_string());
        let error = config.network_access.validate().unwrap_err();
        assert!(
            error.starts_with("Denied hosts: `S3.amazonaws.com:443` is not a valid host entry"),
            "{error}"
        );

        config.network_access.allow_domains.push("*.*".to_string());
        let error = config.network_access.validate().unwrap_err();
        assert!(error.starts_with("Allowed hosts: `*.*`"), "{error}");

        // Stored as is: a hand-edited config still loads.
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(sandbox_json(json), config);
    }

    /// `ExecutionSandboxConfig` as the build before `networkAccess` reads it.
    #[derive(Deserialize, Debug, PartialEq)]
    #[serde(rename_all = "camelCase")]
    struct PreviousSandboxConfig {
        #[serde(default)]
        network: PreviousNetwork,
        #[serde(default)]
        session_bus: SandboxSessionBusConfig,
    }

    #[derive(Deserialize, Debug, PartialEq, Default)]
    #[serde(rename_all = "snake_case")]
    enum PreviousNetwork {
        #[default]
        Enabled,
        Disabled,
    }

    #[test]
    fn an_older_build_reads_the_saved_config_with_the_same_network_switch() {
        for (mode, expected) in [
            (NetworkMode::Off, PreviousNetwork::Disabled),
            (NetworkMode::Allowlist, PreviousNetwork::Enabled),
            (NetworkMode::HostNetwork, PreviousNetwork::Enabled),
        ] {
            let json = serde_json::to_value(policy(mode)).unwrap();
            let previous: PreviousSandboxConfig = serde_json::from_value(json).unwrap();
            assert_eq!(
                previous,
                PreviousSandboxConfig {
                    network: expected,
                    session_bus: SandboxSessionBusConfig::Allow,
                },
                "{mode:?}"
            );
        }
    }
}

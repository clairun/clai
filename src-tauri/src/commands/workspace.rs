//! Workspace state persistence commands.
//!
//! These commands handle saving and loading the workspace state
//! (tabs, commands, layout) to/from SQLite.

use crate::assistant::repository;
use crate::assistant::types::{
    AssistantMessage, AssistantRun, AssistantSession, ContentPart, MessageRole, SessionContext,
    SessionKind, ToolInvocation, WorkspaceAgentSummary,
};
use crate::config::{
    agent_instructions_with_skills, workspace_config, AgentConfig, AppConfig,
    ExecutionCapabilityConfig, WorkspaceAgent, WorkspaceConfig,
};
use crate::db::DbPool;
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use tauri::State;

/// Tab context payload.
///
/// This is intentionally schema-less on the Rust side so frontend tab context can
/// evolve without losing fields during persistence round-trips.
pub type TabContext = serde_json::Value;

/// Tile node in the layout tree
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileNode {
    pub id: String,
    #[serde(rename = "type")]
    pub tile_type: String, // "leaf" or "split"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>, // "horizontal" or "vertical"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TileNode>>,
}

/// Tab data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub root_tile: TileNode,
    pub context: TabContext,
}

/// Command data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub id: String,
    #[serde(rename = "type")]
    pub command_type: String,
    pub args: serde_json::Value,
    pub tab_id: String,
    pub tile_id: String,
    pub created_at: i64,
    pub state: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

/// Vestigial: the legacy tabs/tiles UI state shape. Still serialized over
/// the wire because `workspaceStore.js` hasn't been ripped out yet; the
/// load/save Tauri commands return / ignore an empty version of this.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    pub active_tab_id: Option<String>,
    #[serde(default)]
    pub tab_order: Vec<String>, // Array of tab IDs in display order
    pub tabs: HashMap<String, Tab>,
    pub commands: HashMap<String, Command>,
}

const DEFAULT_WORKSPACE_ID: &str = "default";
const MAX_ENTRY_COUNT: usize = 500;
const MAX_FILE_CONTENT_BYTES: usize = 200_000;
const SKIPPED_ARTIFACT_DIRS: &[&str] = &[
    ".cache",
    ".cargo",
    ".clai",
    ".git",
    ".npm",
    ".rustup",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "venv",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileEntry {
    pub path: String,
    pub relative_path: String,
    pub name: String,
    pub viewer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    pub workspace_id: String,
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub assigned_agents: Vec<WorkspaceAgentResponse>,
    #[serde(default)]
    pub tasks: Vec<WorkspaceTaskResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_workspace_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,
    #[serde(default)]
    pub provider_connection_names: Vec<String>,
    #[serde(default)]
    pub selected_mcp_server_ids: Vec<String>,
    #[serde(default)]
    pub selected_mcp_server_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<AssistantSession>,
    #[serde(default)]
    pub messages: Vec<AssistantMessage>,
    #[serde(default)]
    pub runs: Vec<AssistantRun>,
    #[serde(default)]
    pub tool_calls: Vec<ToolInvocation>,
    #[serde(default)]
    pub memories: Vec<WorkspaceFileEntry>,
    #[serde(default)]
    pub artifacts: Vec<WorkspaceFileEntry>,
    // Agent schedule info (only for agent workspaces)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    // Periodic-workspace surface — read from the workspace's default manager
    // agent so the Workspace page can render the same run / pause / resume
    // controls the Fleet card has. Mirrors WorkspaceListEntry.
    pub schedule_enabled: bool,
    pub schedule_paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_in_seconds: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshotOptions {
    #[serde(default)]
    pub include_session_payload: Option<bool>,
    #[serde(default)]
    pub include_files: Option<bool>,
}

impl WorkspaceSnapshotOptions {
    fn include_session_payload(&self) -> bool {
        self.include_session_payload.unwrap_or(true)
    }

    fn include_files(&self) -> bool {
        self.include_files.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgentResponse {
    pub id: String,
    pub workspace_id: String,
    pub agent_definition_id: String,
    pub display_name: String,
    pub role: String,
    pub enabled: bool,
    pub is_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_description: Option<String>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,
    #[serde(default)]
    pub skill_ids: Vec<String>,
    /// MCP server IDs selected for this agent. Surfaced so the Fleet
    /// chat input (which routes messages to the workspace's default
    /// agent) can spin up a session with the right tool wiring.
    #[serde(default)]
    pub selected_mcp_server_ids: Vec<String>,
    /// Execution-capability config (shell mode, allow/block prefixes,
    /// filesystem access, web access). Same rationale as
    /// `selected_mcp_server_ids` — needed to start a usable session
    /// from the Fleet view.
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTaskResponse {
    pub id: String,
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_workspace_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_display_name: Option<String>,
    pub assigned_to_workspace_agent_id: String,
    pub assigned_agent_definition_id: String,
    pub assigned_agent_display_name: String,
    pub title: String,
    pub instructions: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_acknowledged_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_response_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSessionBinding {
    pub session: AssistantSession,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_connection_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileContent {
    pub path: String,
    pub viewer: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReadFileRequest {
    pub workspace_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDownloadRequest {
    pub workspace_id: String,
    pub path: String,
    pub destination: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceWriteFileRequest {
    pub workspace_id: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTaskActionRequest {
    pub workspace_id: String,
    pub task_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTaskFeedbackRequest {
    pub workspace_id: String,
    pub task_id: String,
    pub response: String,
}

#[derive(Debug, Clone)]
struct WorkspaceDescriptor {
    workspace_id: String,
    kind: String,
    title: String,
    agent_id: Option<String>,
    root_path: Option<PathBuf>,
    provider_connection_ids: Vec<String>,
    selected_mcp_server_ids: Vec<String>,
    selected_mcp_server_names: Vec<String>,
    execution: ExecutionCapabilityConfig,
    tool_scopes: Vec<String>,
    automation_name: Option<String>,
    automation_description: Option<String>,
}

#[derive(Debug, Clone)]
struct WorkspaceProviderSelection {
    ids: Vec<String>,
    names: Vec<String>,
    preferred_connection_id: Option<String>,
}

#[derive(Debug, Clone)]
struct VirtualWorkspaceArtifact {
    entry: WorkspaceFileEntry,
    content: String,
}

#[derive(Debug, Clone)]
struct WorkspaceAgentRow {
    id: String,
    workspace_id: String,
    /// Legacy: foreign key into `ClaiConfig.agents`. Will be dropped in Phase 1.7.
    agent_definition_id: String,
    display_name: Option<String>,
    /// Legacy: replaced by `workspaces.default_workspace_agent_id` in Phase 1.6.
    role: String,
    enabled: bool,
    // Inline agent fields (Phase 1.2 writes them, Phase 1.4 reads them).
    name: String,
    description: String,
    selected_skill_ids: Vec<String>,
    selected_mcp_server_ids: Vec<String>,
    provider_connection_ids: Vec<String>,
    execution: ExecutionCapabilityConfig,
    created_at: i64,
    updated_at: i64,
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn resolve_workspace_id(state: &AppState, workspace_id: Option<String>) -> Result<String, String> {
    let requested = workspace_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(id) = requested {
        if id != DEFAULT_WORKSPACE_ID || state.workspace_root(&id).is_some() {
            return Ok(id);
        }
    }

    state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted()
        .first()
        .map(|locator| locator.id.clone())
        .ok_or_else(|| "No workspace is available.".to_string())
}

fn app_config(state: &AppState) -> Result<AppConfig, String> {
    Ok(state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get())
}

fn load_workspace_config_for_id(
    state: &AppState,
    workspace_id: &str,
) -> Result<(PathBuf, WorkspaceConfig), String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    Ok((root, config))
}

fn save_workspace_config_for_root(
    state: &AppState,
    root: &Path,
    config: &WorkspaceConfig,
) -> Result<(), String> {
    workspace_config::save(root, config).map_err(|e| e.to_string())?;
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .insert_config(root.to_path_buf(), config);
    Ok(())
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn viewer_for_path(path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if ext == "canvas" {
        return "canvas".to_string();
    }

    if file_name.ends_with(".dashboard.json") {
        return "dashboard".to_string();
    }

    if ext == "md" || ext == "markdown" {
        return "markdown".to_string();
    }

    if ext == "json" {
        return "json".to_string();
    }

    "text".to_string()
}

fn sort_workspace_entries(entries: &mut [WorkspaceFileEntry]) {
    entries.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.path.cmp(&right.path))
    });
}

fn tab_agent_id(context: &TabContext) -> Option<&str> {
    context
        .get("agent")
        .and_then(|value| value.get("agentId"))
        .and_then(|value| value.as_str())
}

fn virtual_artifact_path(_tab: &Tab, command: &Command, extension: &str) -> String {
    let kind = command.command_type.to_ascii_lowercase();
    let stem = format!("{}-{}", kind, command.id);
    format!("visualizations/{}.{}", stem, extension)
}

fn command_preview(command_type: &str, state: &serde_json::Value) -> Option<String> {
    match command_type {
        "canvas" => Some(format!(
            "{} nodes, {} edges",
            state
                .get("nodes")
                .and_then(|value| value.as_array())
                .map(|value| value.len())
                .unwrap_or(0),
            state
                .get("edges")
                .and_then(|value| value.as_array())
                .map(|value| value.len())
                .unwrap_or(0)
        )),
        "dashboard" => Some(format!(
            "{} charts",
            state
                .get("elements")
                .and_then(|value| value.as_array())
                .map(|value| value.len())
                .unwrap_or(0)
        )),
        _ => None,
    }
}

fn normalize_canvas_artifact_state(state: &serde_json::Value) -> serde_json::Value {
    json!({
        "nodes": state.get("nodes").cloned().unwrap_or_else(|| json!([])),
        "edges": state.get("edges").cloned().unwrap_or_else(|| json!([])),
    })
}

fn normalize_dashboard_artifact_state(state: &serde_json::Value) -> serde_json::Value {
    let time_range = state
        .get("timeRange")
        .and_then(|value| value.as_str())
        .or_else(|| {
            state
                .get("selectedInterval")
                .and_then(|value| value.get("label"))
                .and_then(|value| value.as_str())
        })
        .unwrap_or("1h");

    json!({
        "elements": state.get("elements").cloned().unwrap_or_else(|| json!([])),
        "timeRange": time_range,
    })
}

fn command_to_virtual_artifact(
    tab: &Tab,
    command: &Command,
) -> Result<Option<VirtualWorkspaceArtifact>, String> {
    let (normalized_state, extension, viewer) = match command.command_type.as_str() {
        "canvas" => (
            normalize_canvas_artifact_state(&command.state),
            "canvas",
            "canvas".to_string(),
        ),
        "dashboard" => (
            normalize_dashboard_artifact_state(&command.state),
            "dashboard.json",
            "dashboard".to_string(),
        ),
        _ => return Ok(None),
    };

    let path = virtual_artifact_path(tab, command, extension);
    let content = serde_json::to_string_pretty(&normalized_state)
        .map_err(|error| format!("Failed to serialize {} artifact: {}", command.id, error))?;

    Ok(Some(VirtualWorkspaceArtifact {
        entry: WorkspaceFileEntry {
            path: path.clone(),
            relative_path: path.clone(),
            name: Path::new(&path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string(),
            viewer,
            size: Some(content.len() as u64),
            updated_at: command.updated_at.or(Some(command.created_at)),
            preview: command_preview(&command.command_type, &normalized_state),
        },
        content,
    }))
}

fn file_updated_at(metadata: &fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
}

fn build_file_entry(root: &Path, path: &Path) -> Option<WorkspaceFileEntry> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }

    let relative_path = path.strip_prefix(root).ok()?.to_string_lossy().to_string();
    Some(WorkspaceFileEntry {
        path: relative_path.clone(),
        relative_path,
        name: path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string(),
        viewer: viewer_for_path(path),
        size: Some(metadata.len()),
        updated_at: file_updated_at(&metadata),
        preview: None,
    })
}

fn should_skip_artifact_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|name| SKIPPED_ARTIFACT_DIRS.contains(&name))
        .unwrap_or(false)
}

fn collect_files(
    current: &Path,
    root: &Path,
    entries: &mut Vec<WorkspaceFileEntry>,
    skip_clai: bool,
) -> Result<(), String> {
    if entries.len() >= MAX_ENTRY_COUNT {
        return Ok(());
    }

    let read_dir = match fs::read_dir(current) {
        Ok(value) => value,
        Err(error) => return Err(format!("Failed to read {}: {}", current.display(), error)),
    };

    for entry in read_dir {
        if entries.len() >= MAX_ENTRY_COUNT {
            break;
        }

        let entry =
            entry.map_err(|error| format!("Failed to inspect directory entry: {}", error))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Failed to inspect {}: {}", path.display(), error))?;

        if file_type.is_dir() {
            if skip_clai && should_skip_artifact_dir(&path) {
                continue;
            }
            collect_files(&path, root, entries, skip_clai)?;
            continue;
        }

        if let Some(file) = build_file_entry(root, &path) {
            entries.push(file);
        }
    }

    Ok(())
}

fn ensure_agent_workspace_root(root: &Path) -> Result<(), String> {
    let memory_root = root.join(".clai").join("memory").join("journal");
    fs::create_dir_all(&memory_root).map_err(|error| {
        format!(
            "Failed to prepare agent workspace {}: {}",
            root.display(),
            error
        )
    })
}

fn resolve_workspace_descriptor(
    state: &AppState,
    workspace_id: Option<String>,
) -> Result<WorkspaceDescriptor, String> {
    let workspace_id = resolve_workspace_id(state, workspace_id)?;
    let (root_path, config) = load_workspace_config_for_id(state, &workspace_id)?;
    let app_config = app_config(state)?;
    let manager = config
        .agents
        .iter()
        .find(|agent| agent.id == config.default_agent_id);
    let mut execution = manager
        .map(|agent| agent.execution.clone())
        .unwrap_or_default();
    if manager.is_none() {
        execution.web.enabled = true;
    }
    let selected_mcp_server_ids = manager
        .map(|agent| workspace_config::refs_to_mcp_ids(&app_config, &agent.selected_mcp_servers))
        .unwrap_or_default();
    let selected_mcp_server_names = selected_mcp_server_ids
        .iter()
        .map(|id| {
            app_config
                .mcp_servers
                .iter()
                .find(|server| server.id == *id)
                .map(|server| server.name.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect();

    Ok(WorkspaceDescriptor {
        workspace_id,
        kind: "general".to_string(),
        title: config.title,
        agent_id: None,
        root_path: Some(root_path),
        provider_connection_ids: manager
            .map(|agent| agent.provider_connection_ids.clone())
            .or_else(|| {
                config
                    .preferred_provider_connection_id
                    .clone()
                    .map(|id| vec![id])
            })
            .unwrap_or_default(),
        selected_mcp_server_ids,
        selected_mcp_server_names,
        execution,
        tool_scopes: vec!["fs".to_string(), "web".to_string()],
        automation_name: manager.map(|agent| agent.name.clone()),
        automation_description: manager.map(|agent| agent.description.clone()),
    })
}

fn resolve_workspace_provider_selection(
    state: &AppState,
    descriptor: &WorkspaceDescriptor,
) -> Result<WorkspaceProviderSelection, String> {
    let connections = app_config(state)?.provider_connections;
    let enabled_connections: Vec<_> = connections
        .into_iter()
        .filter(|connection| connection.enabled)
        .collect();

    if descriptor.agent_id.is_some() {
        let filtered: Vec<_> = enabled_connections
            .into_iter()
            .filter(|connection| descriptor.provider_connection_ids.contains(&connection.id))
            .collect();

        return Ok(WorkspaceProviderSelection {
            ids: filtered
                .iter()
                .map(|connection| connection.id.clone())
                .collect(),
            names: filtered
                .iter()
                .map(|connection| connection.name.clone())
                .collect(),
            preferred_connection_id: filtered.first().map(|connection| connection.id.clone()),
        });
    }

    let preferred_connection_id = descriptor
        .provider_connection_ids
        .first()
        .cloned()
        .filter(|id| enabled_connections.iter().any(|c| c.id == *id))
        .or_else(|| enabled_connections.first().map(|c| c.id.clone()));

    Ok(WorkspaceProviderSelection {
        ids: enabled_connections
            .iter()
            .map(|connection| connection.id.clone())
            .collect(),
        names: enabled_connections
            .iter()
            .map(|connection| connection.name.clone())
            .collect(),
        preferred_connection_id,
    })
}

fn agent_runtime_description(state: &AppState, agent: &AgentConfig) -> String {
    let config = state.config_manager.lock().map(|manager| manager.get());

    match config {
        Ok(config) => agent_instructions_with_skills(&config, agent),
        Err(_) => agent.description.clone(),
    }
}

fn workspace_default_agent_id(
    state: &AppState,
    workspace_id: &str,
) -> Result<Option<String>, String> {
    let (_root, config) = load_workspace_config_for_id(state, workspace_id)?;
    Ok(Some(config.default_agent_id))
}

fn set_workspace_default_agent_id(
    state: &AppState,
    workspace_id: &str,
    workspace_agent_id: &str,
) -> Result<(), String> {
    let (root, mut config) = load_workspace_config_for_id(state, workspace_id)?;
    if !config
        .agents
        .iter()
        .any(|agent| agent.id == workspace_agent_id)
    {
        return Err(format!(
            "Workspace agent assignment not found: {}",
            workspace_agent_id
        ));
    }
    config.default_agent_id = workspace_agent_id.to_string();
    config.updated_at = now_millis();
    save_workspace_config_for_root(state, &root, &config)
}

fn load_workspace_agent_rows(
    state: &AppState,
    workspace_id: &str,
) -> Result<Vec<WorkspaceAgentRow>, String> {
    let app_config = app_config(state)?;
    let (_root, config) = load_workspace_config_for_id(state, workspace_id)?;
    let mut rows: Vec<_> = config
        .agents
        .iter()
        .map(|agent| workspace_agent_row_from_config(&app_config, &config, agent))
        .collect();
    rows.sort_by_key(|row| {
        (
            if row.id == config.default_agent_id {
                0
            } else {
                1
            },
            row.created_at,
        )
    });
    Ok(rows)
}

fn workspace_agent_row_from_config(
    app_config: &AppConfig,
    workspace: &WorkspaceConfig,
    agent: &WorkspaceAgent,
) -> WorkspaceAgentRow {
    WorkspaceAgentRow {
        id: agent.id.clone(),
        workspace_id: workspace.id.clone(),
        agent_definition_id: agent.id.clone(),
        display_name: None,
        role: if workspace.default_agent_id == agent.id {
            "manager".to_string()
        } else {
            "member".to_string()
        },
        enabled: agent.enabled,
        name: agent.name.clone(),
        description: agent.description.clone(),
        selected_skill_ids: workspace_config::refs_to_skill_ids(app_config, &agent.selected_skills),
        selected_mcp_server_ids: workspace_config::refs_to_mcp_ids(
            app_config,
            &agent.selected_mcp_servers,
        ),
        provider_connection_ids: agent.provider_connection_ids.clone(),
        execution: agent.execution.clone(),
        created_at: agent.created_at,
        updated_at: agent.updated_at,
    }
}

fn workspace_agent_response_from_row(
    row: WorkspaceAgentRow,
    default_workspace_agent_id: Option<&str>,
) -> WorkspaceAgentResponse {
    let display_name = row
        .display_name
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            if row.name.trim().is_empty() {
                row.agent_definition_id.clone()
            } else {
                row.name.clone()
            }
        });

    let agent_name = if row.name.trim().is_empty() {
        None
    } else {
        Some(row.name.clone())
    };
    let agent_description = if row.description.trim().is_empty() {
        None
    } else {
        Some(row.description.clone())
    };

    WorkspaceAgentResponse {
        id: row.id.clone(),
        workspace_id: row.workspace_id,
        agent_definition_id: row.agent_definition_id,
        display_name,
        role: row.role,
        enabled: row.enabled,
        is_default: default_workspace_agent_id == Some(row.id.as_str()),
        agent_name,
        agent_description,
        provider_connection_ids: row.provider_connection_ids,
        skill_ids: row.selected_skill_ids,
        selected_mcp_server_ids: row.selected_mcp_server_ids,
        execution: row.execution,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn concise_agent_description(description: Option<String>) -> Option<String> {
    let text = description?;
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .to_string();

    const MAX_SUMMARY_CHARS: usize = 240;
    if first_line.chars().count() <= MAX_SUMMARY_CHARS {
        return Some(first_line);
    }

    let mut summary: String = first_line.chars().take(MAX_SUMMARY_CHARS).collect();
    summary.push_str("...");
    Some(summary)
}

async fn list_workspace_agent_responses(
    state: &AppState,
    workspace_id: &str,
) -> Result<(Vec<WorkspaceAgentResponse>, Option<String>), String> {
    let rows = load_workspace_agent_rows(state, workspace_id)?;
    let default_workspace_agent_id = workspace_default_agent_id(state, workspace_id)?;

    // Phase 1.4: no more join with the global agent catalog — every field
    // the response needs lives inline on the workspace_agents row.
    let responses = rows
        .into_iter()
        .map(|row| workspace_agent_response_from_row(row, default_workspace_agent_id.as_deref()))
        .collect();

    Ok((responses, default_workspace_agent_id))
}

async fn list_workspace_task_responses(
    pool: &DbPool,
    state: &AppState,
    workspace_id: &str,
) -> Result<Vec<WorkspaceTaskResponse>, String> {
    let agent_rows = load_workspace_agent_rows(state, workspace_id).unwrap_or_default();
    let agent_names: HashMap<String, String> = agent_rows
        .iter()
        .map(|agent| {
            let name = agent
                .display_name
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| agent.name.clone());
            (agent.id.clone(), name)
        })
        .collect();
    let rows = sqlx::query(
        r#"
        SELECT
            task.id,
            task.created_by_workspace_agent_id,
            task.assigned_to_workspace_agent_id,
            task.assigned_agent_definition_id,
            task.title,
            task.instructions,
            task.status,
            task.result_summary,
            task.error,
            task.session_id,
            task.run_id,
            task.created_at,
            task.updated_at,
            task.completed_at,
            task.attention_acknowledged_at,
            task.user_response,
            task.user_response_at
        FROM workspace_tasks task
        ORDER BY task.updated_at DESC, task.created_at DESC
        LIMIT 50
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load workspace tasks: {}", e))?;

    // task list no longer joins with a global agent catalog; the workspace_tasks
    // SELECT above pulls the assigned agent's display name directly via JOIN
    // with workspace_agents (assigned_display_name).
    let _ = state;

    Ok(rows
        .into_iter()
        .map(|row| {
            let assigned_agent_definition_id: String = row.get("assigned_agent_definition_id");
            let assigned_agent_id: String = row.get("assigned_to_workspace_agent_id");
            let assigned_agent_display_name = agent_names
                .get(&assigned_agent_id)
                .cloned()
                .unwrap_or_else(|| assigned_agent_definition_id.clone());
            let created_by_workspace_agent_id: Option<String> =
                row.get("created_by_workspace_agent_id");
            let created_by_display_name = created_by_workspace_agent_id
                .as_ref()
                .and_then(|id| agent_names.get(id).cloned());

            WorkspaceTaskResponse {
                id: row.get("id"),
                workspace_id: workspace_id.to_string(),
                created_by_workspace_agent_id,
                created_by_display_name,
                assigned_to_workspace_agent_id: assigned_agent_id,
                assigned_agent_definition_id,
                assigned_agent_display_name,
                title: row.get("title"),
                instructions: row.get("instructions"),
                status: row.get("status"),
                result_summary: row.get("result_summary"),
                error: row.get("error"),
                session_id: row.get("session_id"),
                run_id: row.get("run_id"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
                completed_at: row.get("completed_at"),
                attention_acknowledged_at: row.get("attention_acknowledged_at"),
                user_response: row.get("user_response"),
                user_response_at: row.get("user_response_at"),
            }
        })
        .collect())
}

pub(crate) async fn workspace_agent_summaries(
    state: &AppState,
    workspace_id: &str,
) -> Result<Vec<WorkspaceAgentSummary>, String> {
    let (agents, _) = list_workspace_agent_responses(state, workspace_id).await?;

    Ok(agents
        .into_iter()
        .map(|agent| WorkspaceAgentSummary {
            // Phase 1.5: the LLM-visible "agent id" for inter-agent calls is the
            // workspace-local row id. The legacy global `agent_definition_id`
            // column is no longer surfaced (and will be dropped in Phase 1.7).
            agent_definition_id: agent.id.clone(),
            id: agent.id,
            display_name: agent.display_name,
            role: agent.role,
            is_default: agent.is_default,
            description: concise_agent_description(agent.agent_description),
        })
        .collect())
}

/// Build a transient `AgentConfig` from a workspace_agents row's inline fields.
///
/// Phase 1.4: the row carries the full configuration locally; no global
/// catalog lookup is needed.
/// Build a transient `AgentConfig` from a workspace_agents row's inline fields.
/// Schedule fields are populated only for the workspace's default (manager)
/// agent, sourced from the workspace-level `schedule` config; sub-agents
/// never schedule themselves.
fn agent_config_from_row(
    row: &WorkspaceAgentRow,
    schedule: Option<&workspace_config::WorkspaceSchedule>,
) -> AgentConfig {
    let created_at = chrono::DateTime::from_timestamp_millis(row.created_at)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    let updated_at = chrono::DateTime::from_timestamp_millis(row.updated_at)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    AgentConfig {
        id: row.id.clone(),
        workspace_id: row.workspace_id.clone(),
        name: row.name.clone(),
        description: row.description.clone(),
        schedule_enabled: schedule.map(|s| s.enabled).unwrap_or(false),
        interval_minutes: schedule.map(|s| s.interval_minutes).unwrap_or(0),
        enabled: row.enabled,
        selected_mcp_server_ids: row.selected_mcp_server_ids.clone(),
        provider_connection_ids: row.provider_connection_ids.clone(),
        selected_skill_ids: row.selected_skill_ids.clone(),
        execution: row.execution.clone(),
        created_at,
        updated_at,
    }
}

async fn resolve_workspace_manager_agent(
    state: &AppState,
    workspace_id: &str,
) -> Result<Option<AgentConfig>, String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let workspace = workspace_config::load(&root).map_err(|e| e.to_string())?;
    let default_id = Some(workspace.default_agent_id.clone());
    let rows = load_workspace_agent_rows(state, workspace_id)?;

    let manager_row = if let Some(default_id) = default_id.as_deref() {
        rows.iter().find(|row| row.id == default_id)
    } else {
        // Legacy fallback: if no default pointer, prefer role='manager'.
        rows.iter().find(|row| row.role == "manager")
    };

    Ok(manager_row.map(|row| agent_config_from_row(row, Some(&workspace.schedule))))
}

// assign_workspace_agent_row: removed. Workspace agents are created directly
// by commands::workspace_agents::workspace_create_agent or seeded by
// workspace_create's empty-Manager INSERT.

async fn find_workspace_session(
    pool: &DbPool,
    state: &AppState,
    descriptor: &WorkspaceDescriptor,
) -> Result<Option<AssistantSession>, String> {
    let sessions = repository::list_sessions(pool).await?;

    let belongs_to_workspace = |session: &AssistantSession| -> bool {
        session.context.workspace_id.as_deref() == Some(descriptor.workspace_id.as_str())
            || descriptor
                .agent_id
                .as_deref()
                .map(|agent_id| {
                    session.context.agent_workspace_id.as_deref() == Some(agent_id)
                        || session.context.automation_id.as_deref() == Some(agent_id)
                })
                .unwrap_or(false)
    };

    // First preference: the user's *interactive* chat thread. This is the
    // session the user sends messages into via the workspace input bar.
    // The closure wrap on `belongs_to_workspace` is needed: `.iter()` yields
    // `&AssistantSession`, `.filter()` passes a further `&` (so the predicate
    // sees `&&AssistantSession`), and our helper takes `&AssistantSession`.
    // The wrap performs the auto-deref. Clippy's `redundant_closure` lint
    // misfires here because it doesn't model the double-borrow.
    #[allow(clippy::redundant_closure)]
    let interactive = sessions
        .iter()
        .filter(|session| matches!(session.kind, SessionKind::Interactive))
        .filter(|session| belongs_to_workspace(session))
        .max_by_key(|session| session.updated_at)
        .cloned();
    if interactive.is_some() {
        return Ok(interactive);
    }

    // Fallback: if there's no interactive chat (typical for periodic
    // workspaces that have never been chatted with), surface the most
    // recent BackgroundJob session OWNED BY THE WORKSPACE'S DEFAULT
    // MANAGER AGENT. This shows the user what the periodic agent did on
    // its last tick. Inter-agent task delegations also create
    // BackgroundJob sessions, but they're owned by the assignee
    // (`context.automation_id == assignee_agent_id`), not the manager —
    // so filtering by the default agent cleanly excludes them and
    // preserves the "task delegations have their own UI" invariant.
    let manager_id = workspace_default_agent_id(state, &descriptor.workspace_id)
        .ok()
        .flatten();
    let Some(manager_id) = manager_id else {
        return Ok(None);
    };
    #[allow(clippy::redundant_closure)]
    Ok(sessions
        .into_iter()
        .filter(|session| matches!(session.kind, SessionKind::BackgroundJob))
        .filter(|session| belongs_to_workspace(session))
        .filter(|session| session.context.automation_id.as_deref() == Some(manager_id.as_str()))
        .max_by_key(|session| session.updated_at))
}

fn desired_workspace_context(
    state: &AppState,
    descriptor: &WorkspaceDescriptor,
    existing_session: Option<&AssistantSession>,
    workspace_agents: Vec<WorkspaceAgentSummary>,
    workspace_manager: Option<&AgentConfig>,
) -> SessionContext {
    // For general workspaces, MCP servers are user-managed on the session itself
    // (via workspace_update_session_mcp), so preserve the existing session's MCP
    // config instead of overwriting from the descriptor (which has empty defaults).
    let mcp_server_ids = if descriptor.agent_id.is_some() {
        // Agent workspaces: MCP comes from agent config (descriptor)
        descriptor.selected_mcp_server_ids.clone()
    } else if let Some(session) = existing_session {
        // General workspaces: preserve session's MCP config
        session.context.mcp_server_ids.clone()
    } else {
        descriptor.selected_mcp_server_ids.clone()
    };

    let execution = if descriptor.agent_id.is_some() {
        descriptor.execution.clone()
    } else if let Some(manager) = workspace_manager {
        manager.execution.clone()
    } else {
        descriptor.execution.clone()
    };

    let automation_id = descriptor
        .agent_id
        .clone()
        .or_else(|| workspace_manager.map(|agent| agent.id.clone()));
    let agent_workspace_id = descriptor.agent_id.clone().or_else(|| {
        if descriptor.root_path.is_some() {
            Some(descriptor.workspace_id.clone())
        } else {
            None
        }
    });
    let automation_name = descriptor
        .automation_name
        .clone()
        .or_else(|| workspace_manager.map(|agent| agent.name.clone()));
    let automation_description = descriptor
        .automation_description
        .clone()
        .or_else(|| workspace_manager.map(|agent| agent_runtime_description(state, agent)));

    SessionContext {
        space_id: existing_session.and_then(|session| session.context.space_id.clone()),
        room_id: existing_session.and_then(|session| session.context.room_id.clone()),
        workspace_id: Some(descriptor.workspace_id.clone()),
        tool_scopes: descriptor.tool_scopes.clone(),
        mcp_server_ids,
        execution,
        netdata_conversation_id: existing_session
            .and_then(|session| session.context.netdata_conversation_id.clone()),
        cli_session_id: existing_session.and_then(|session| session.context.cli_session_id.clone()),
        automation_id,
        agent_workspace_id,
        automation_name,
        automation_description,
        inter_agent_call: existing_session
            .and_then(|session| session.context.inter_agent_call.clone()),
        workspace_agents,
    }
}

fn resolve_workspace_file_path(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let candidate = normalize_path(root.join(relative_path));
    if !candidate.starts_with(root) {
        return Err(format!(
            "Path {} is outside the workspace root",
            candidate.display()
        ));
    }
    if !candidate.exists() {
        return Err(format!("File not found: {}", candidate.display()));
    }
    if !candidate.is_file() {
        return Err(format!("Not a file: {}", candidate.display()));
    }
    Ok(candidate)
}

fn resolve_workspace_file_target(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let candidate = normalize_path(root.join(relative_path));
    if !candidate.starts_with(root) {
        return Err(format!(
            "Path {} is outside the workspace root",
            candidate.display()
        ));
    }
    Ok(candidate)
}

fn workspace_virtual_artifacts(
    descriptor: &WorkspaceDescriptor,
    workspace_state: &WorkspaceState,
) -> Result<Vec<VirtualWorkspaceArtifact>, String> {
    let Some(agent_id) = descriptor.agent_id.as_deref() else {
        return Ok(Vec::new());
    };

    let mut artifacts = Vec::new();

    for tab in workspace_state.tabs.values() {
        if tab_agent_id(&tab.context) != Some(agent_id) {
            continue;
        }

        for command in workspace_state
            .commands
            .values()
            .filter(|command| command.tab_id == tab.id)
        {
            if let Some(artifact) = command_to_virtual_artifact(tab, command)? {
                artifacts.push(artifact);
            }
        }
    }

    artifacts.sort_by(|left, right| {
        right
            .entry
            .updated_at
            .cmp(&left.entry.updated_at)
            .then_with(|| left.entry.path.cmp(&right.entry.path))
    });

    Ok(artifacts)
}

fn merge_workspace_artifacts(
    mut file_artifacts: Vec<WorkspaceFileEntry>,
    virtual_artifacts: Vec<VirtualWorkspaceArtifact>,
) -> Vec<WorkspaceFileEntry> {
    let existing_paths: std::collections::HashSet<String> = file_artifacts
        .iter()
        .map(|entry| entry.path.clone())
        .collect();

    file_artifacts.extend(
        virtual_artifacts
            .into_iter()
            .filter(|artifact| !existing_paths.contains(&artifact.entry.path))
            .map(|artifact| artifact.entry),
    );

    sort_workspace_entries(&mut file_artifacts);
    file_artifacts
}

/// Vestigial: the legacy tabs/tiles UI no longer exists. Frontend still
/// invokes `load_workspace_state` / `save_workspace_state` from
/// `workspaceStore.js`; we satisfy those callers with empty state until
/// the frontend store is ripped out too.
async fn load_workspace_state_from_pool() -> Result<WorkspaceState, String> {
    Ok(WorkspaceState::default())
}

/// Vestigial: the legacy tabs/tiles UI no longer exists. Frontend still
/// invokes this from `workspaceStore.js`; we return empty state.
#[tauri::command]
pub async fn load_workspace_state() -> Result<WorkspaceState, String> {
    Ok(WorkspaceState::default())
}

/// Vestigial Tauri command — the tabs/tiles UI no longer exists. The
/// frontend's `workspaceStore` still invokes this; we accept and ignore
/// the payload until that store is removed.
#[tauri::command]
pub async fn save_workspace_state(_workspace_state: WorkspaceState) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn workspace_get_snapshot(
    workspace_id: Option<String>,
    options: Option<WorkspaceSnapshotOptions>,
    state: State<'_, AppState>,
) -> Result<WorkspaceSnapshot, String> {
    let options = options.unwrap_or_default();
    let descriptor = resolve_workspace_descriptor(state.inner(), workspace_id)?;
    if let Some(root_path) = &descriptor.root_path {
        ensure_agent_workspace_root(root_path)?;
    }
    let workspace_pool = state.workspace_db(&descriptor.workspace_id).await?;

    let provider_selection = resolve_workspace_provider_selection(state.inner(), &descriptor)?;

    let session = find_workspace_session(&workspace_pool, state.inner(), &descriptor).await?;
    let (messages, runs, tool_calls) = if let Some(session) = &session {
        let runs = repository::list_runs(&workspace_pool, &session.id).await?;
        if !options.include_session_payload() {
            (Vec::new(), runs, Vec::new())
        } else {
            (
                repository::list_messages(&workspace_pool, &session.id).await?,
                runs,
                repository::list_tool_calls(&workspace_pool, &session.id, None).await?,
            )
        }
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    let (memories, artifacts) = if options.include_files() {
        if let Some(root_path) = &descriptor.root_path {
            let memory_root = root_path.join(".clai").join("memory");
            let mut memories = Vec::new();
            if memory_root.exists() {
                collect_files(&memory_root, root_path, &mut memories, false)?;
            }

            let mut artifacts = Vec::new();
            collect_files(root_path, root_path, &mut artifacts, true)?;
            sort_workspace_entries(&mut memories);
            sort_workspace_entries(&mut artifacts);

            let workspace_state = load_workspace_state_from_pool().await?;
            let virtual_artifacts = workspace_virtual_artifacts(&descriptor, &workspace_state)?;
            let artifacts = merge_workspace_artifacts(artifacts, virtual_artifacts);

            (memories, artifacts)
        } else {
            (Vec::new(), Vec::new())
        }
    } else {
        (Vec::new(), Vec::new())
    };

    let (assigned_agents, default_workspace_agent_id) =
        list_workspace_agent_responses(state.inner(), &descriptor.workspace_id).await?;
    let tasks =
        list_workspace_task_responses(&workspace_pool, state.inner(), &descriptor.workspace_id)
            .await?;

    // Workspace-level schedule (no longer per-agent). The scheduled tick
    // invokes the default agent.
    let enabled: Option<bool> = None;
    let workspace_config_for_schedule = state
        .workspace_root(&descriptor.workspace_id)
        .and_then(|root| workspace_config::load(&root).ok());
    let (schedule_enabled, schedule_paused, interval_minutes) =
        match workspace_config_for_schedule.as_ref() {
            Some(cfg) if cfg.schedule.enabled => (
                true,
                cfg.schedule.paused,
                Some(cfg.schedule.interval_minutes),
            ),
            _ => (false, false, None),
        };
    let next_run_in_seconds = if schedule_enabled && !schedule_paused {
        if let Some(manager_id) = default_workspace_agent_id.as_deref() {
            let instance_id = format!("{}::", manager_id);
            let scheduler = state.scheduler.lock().await;
            scheduler
                .get_instance(&instance_id)
                .map(|instance| instance.seconds_until_next_run())
        } else {
            None
        }
    } else {
        None
    };

    Ok(WorkspaceSnapshot {
        workspace_id: descriptor.workspace_id,
        kind: descriptor.kind,
        title: descriptor.title,
        agent_id: descriptor.agent_id,
        assigned_agents,
        tasks,
        default_workspace_agent_id,
        root_path: descriptor.root_path.map(|path| path.display().to_string()),
        provider_connection_ids: provider_selection.ids,
        provider_connection_names: provider_selection.names,
        selected_mcp_server_ids: descriptor.selected_mcp_server_ids,
        selected_mcp_server_names: descriptor.selected_mcp_server_names,
        session,
        messages,
        runs,
        tool_calls,
        memories,
        artifacts,
        enabled,
        schedule_enabled,
        schedule_paused,
        interval_minutes,
        next_run_in_seconds,
    })
}

#[tauri::command]
pub async fn workspace_get_or_create_session(
    workspace_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<WorkspaceSessionBinding, String> {
    let descriptor = resolve_workspace_descriptor(state.inner(), workspace_id)?;
    if let Some(root_path) = &descriptor.root_path {
        ensure_agent_workspace_root(root_path)?;
    }

    let workspace_pool = state.workspace_db(&descriptor.workspace_id).await?;
    let provider_selection = resolve_workspace_provider_selection(state.inner(), &descriptor)?;
    let provider_connection_id = provider_selection.preferred_connection_id;
    let existing = find_workspace_session(&workspace_pool, state.inner(), &descriptor).await?;
    let workspace_agents =
        workspace_agent_summaries(state.inner(), &descriptor.workspace_id).await?;
    let workspace_manager =
        resolve_workspace_manager_agent(state.inner(), &descriptor.workspace_id).await?;
    let session = if let Some(existing) = existing {
        let desired_context = desired_workspace_context(
            state.inner(),
            &descriptor,
            Some(&existing),
            workspace_agents.clone(),
            workspace_manager.as_ref(),
        );
        if existing.title.as_deref() != Some(descriptor.title.as_str())
            || existing.context != desired_context
        {
            let mut updated = existing;
            updated.title = Some(descriptor.title.clone());
            updated.context = desired_context;
            updated.updated_at = chrono::Utc::now().timestamp_millis();
            repository::update_session(&workspace_pool, &updated).await?
        } else {
            existing
        }
    } else {
        repository::create_session(
            &workspace_pool,
            repository::CreateSessionParams {
                kind: if descriptor.agent_id.is_some() {
                    SessionKind::BackgroundJob
                } else {
                    SessionKind::Interactive
                },
                title: Some(descriptor.title.clone()),
                context: desired_workspace_context(
                    state.inner(),
                    &descriptor,
                    None,
                    workspace_agents,
                    workspace_manager.as_ref(),
                ),
            },
        )
        .await?
    };

    Ok(WorkspaceSessionBinding {
        session,
        provider_connection_id,
    })
}

#[tauri::command]
pub async fn workspace_read_file(
    request: WorkspaceReadFileRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceFileContent, String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;

    ensure_agent_workspace_root(root_path)?;
    match resolve_workspace_file_path(root_path, &request.path) {
        Ok(resolved) => {
            let bytes = fs::read(&resolved)
                .map_err(|error| format!("Failed to read {}: {}", resolved.display(), error))?;
            let bytes = if bytes.len() > MAX_FILE_CONTENT_BYTES {
                &bytes[..MAX_FILE_CONTENT_BYTES]
            } else {
                &bytes[..]
            };
            let content = String::from_utf8(bytes.to_vec())
                .map_err(|_| format!("{} does not contain UTF-8 text", resolved.display()))?;

            Ok(WorkspaceFileContent {
                path: request.path,
                viewer: viewer_for_path(&resolved),
                content,
            })
        }
        Err(_) => {
            let workspace_state = load_workspace_state_from_pool().await?;
            if let Some(artifact) = workspace_virtual_artifacts(&descriptor, &workspace_state)?
                .into_iter()
                .find(|artifact| artifact.entry.path == request.path)
            {
                return Ok(WorkspaceFileContent {
                    path: request.path,
                    viewer: artifact.entry.viewer,
                    content: artifact.content,
                });
            }

            Err(format!("File not found: {}", request.path))
        }
    }
}

/// Copy a workspace file to a destination path chosen by the user.
///
/// The frontend is responsible for showing a save dialog via `@tauri-apps/plugin-dialog`
/// and passing the chosen path here.
#[tauri::command]
pub async fn workspace_download_file(
    request: WorkspaceDownloadRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;

    ensure_agent_workspace_root(root_path)?;
    let dest = PathBuf::from(&request.destination);
    match resolve_workspace_file_path(root_path, &request.path) {
        Ok(source) => {
            fs::copy(&source, &dest).map_err(|e| format!("Failed to save file: {}", e))?;
            Ok(dest.to_string_lossy().to_string())
        }
        Err(_) => {
            let workspace_state = load_workspace_state_from_pool().await?;
            if let Some(artifact) = workspace_virtual_artifacts(&descriptor, &workspace_state)?
                .into_iter()
                .find(|artifact| artifact.entry.path == request.path)
            {
                fs::write(&dest, artifact.content)
                    .map_err(|e| format!("Failed to save file: {}", e))?;
                return Ok(dest.to_string_lossy().to_string());
            }

            Err(format!("File not found: {}", request.path))
        }
    }
}

#[tauri::command]
pub async fn workspace_write_file(
    request: WorkspaceWriteFileRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;

    ensure_agent_workspace_root(root_path)?;
    let target = resolve_workspace_file_target(root_path, &request.path)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to prepare {}: {}", parent.display(), error))?;
    }

    fs::write(&target, request.content)
        .map_err(|error| format!("Failed to write {}: {}", target.display(), error))?;

    Ok(request.path)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUpdateMcpRequest {
    pub workspace_id: String,
    pub mcp_server_ids: Vec<String>,
}

/// Update the MCP server IDs for a workspace session.
///
/// For the general workspace, this is the only way to configure MCP servers
/// (agent workspaces get their MCP servers from the agent config).
#[tauri::command]
pub async fn workspace_update_session_mcp(
    request: WorkspaceUpdateMcpRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let workspace_pool = state.workspace_db(&descriptor.workspace_id).await?;
    let existing = find_workspace_session(&workspace_pool, state.inner(), &descriptor).await?;
    let workspace_agents =
        workspace_agent_summaries(state.inner(), &descriptor.workspace_id).await?;
    let workspace_manager =
        resolve_workspace_manager_agent(state.inner(), &descriptor.workspace_id).await?;

    let session = if let Some(session) = existing {
        session
    } else {
        // No session yet — create one so MCP preferences are persisted
        // before the user sends their first message.
        repository::create_session(
            &workspace_pool,
            repository::CreateSessionParams {
                kind: if descriptor.agent_id.is_some() {
                    SessionKind::BackgroundJob
                } else {
                    SessionKind::Interactive
                },
                title: Some(descriptor.title.clone()),
                context: desired_workspace_context(
                    state.inner(),
                    &descriptor,
                    None,
                    workspace_agents.clone(),
                    workspace_manager.as_ref(),
                ),
            },
        )
        .await?
    };

    let mut updated = session;
    updated.context.mcp_server_ids = request.mcp_server_ids.clone();
    updated.context.workspace_agents = workspace_agents;
    if descriptor.agent_id.is_none() {
        if let Some(manager) = workspace_manager.as_ref() {
            updated.context.execution = manager.execution.clone();
            updated.context.automation_id = Some(manager.id.clone());
            updated.context.agent_workspace_id = descriptor
                .root_path
                .as_ref()
                .map(|_| descriptor.workspace_id.clone());
            updated.context.automation_name = Some(manager.name.clone());
            updated.context.automation_description =
                Some(agent_runtime_description(state.inner(), manager));

            // Persist the MCP selection onto the workspace's manager row so
            // it survives across sessions and shows in Workspace Settings.
            let app_config = app_config(state.inner())?;
            let (root, mut config) =
                load_workspace_config_for_id(state.inner(), &descriptor.workspace_id)?;
            if let Some(agent) = config
                .agents
                .iter_mut()
                .find(|agent| agent.id == manager.id)
            {
                agent.selected_mcp_servers =
                    workspace_config::mcp_ids_to_refs(&app_config, &request.mcp_server_ids);
                agent.updated_at = chrono::Utc::now().timestamp_millis();
                config.updated_at = agent.updated_at;
                save_workspace_config_for_root(state.inner(), &root, &config)?;
            }
        }
    }
    updated.updated_at = chrono::Utc::now().timestamp_millis();
    repository::update_session(&workspace_pool, &updated).await?;

    Ok(())
}

/// Set the preferred provider connection for a workspace.
#[tauri::command]
pub async fn workspace_set_provider(
    workspace_id: String,
    provider_connection_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();
    let (root, mut config) = load_workspace_config_for_id(state.inner(), &workspace_id)?;
    config.preferred_provider_connection_id = Some(provider_connection_id.clone());
    if let Some(manager) = config
        .agents
        .iter_mut()
        .find(|agent| agent.id == config.default_agent_id)
    {
        manager.provider_connection_ids = vec![provider_connection_id];
        manager.updated_at = now;
    }
    config.updated_at = now;
    save_workspace_config_for_root(state.inner(), &root, &config)?;

    Ok(())
}

#[tauri::command]
pub async fn workspace_list_agents(
    workspace_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceAgentResponse>, String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;
    let (agents, _) = list_workspace_agent_responses(state.inner(), &workspace_id).await?;
    Ok(agents)
}

// workspace_assign_agent / workspace_unassign_agent: removed.
// Agents are workspace-local now; use the workspace-scoped CRUD in
// `commands::workspace_agents` (workspace_create_agent / workspace_delete_agent).

#[tauri::command]
pub async fn workspace_set_default_agent(
    workspace_id: String,
    workspace_agent_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;
    set_workspace_default_agent_id(state.inner(), &workspace_id, &workspace_agent_id)
}

#[tauri::command]
pub async fn workspace_acknowledge_task(
    request: WorkspaceTaskActionRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(request.workspace_id))?;
    let pool = state.workspace_db(&workspace_id).await?;
    let now = now_millis();
    let result = sqlx::query(
        r#"
        UPDATE workspace_tasks
        SET attention_acknowledged_at = ?,
            updated_at = ?
        WHERE id = ?
        "#,
    )
    .bind(now)
    .bind(now)
    .bind(&request.task_id)
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to acknowledge workspace task: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("Workspace task not found: {}", request.task_id));
    }

    Ok(())
}

#[tauri::command]
pub async fn workspace_submit_task_feedback(
    request: WorkspaceTaskFeedbackRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(request.workspace_id))?;
    let workspace_pool = state.workspace_db(&workspace_id).await?;
    let response = request.response.trim();
    if response.is_empty() {
        return Err("Task feedback cannot be empty.".to_string());
    }

    let current: Option<(String, String, String)> =
        sqlx::query_as("SELECT id, title, status FROM workspace_tasks WHERE id = ? LIMIT 1")
            .bind(&request.task_id)
            .fetch_optional(&workspace_pool)
            .await
            .map_err(|e| format!("Failed to load workspace task: {}", e))?;

    let Some((task_id, title, status)) = current else {
        return Err(format!("Workspace task not found: {}", request.task_id));
    };

    let now = now_millis();
    let next_status = if status == "needs_user_input" {
        "completed"
    } else {
        status.as_str()
    };
    sqlx::query(
        r#"
        UPDATE workspace_tasks
        SET user_response = ?,
            user_response_at = ?,
            attention_acknowledged_at = ?,
            status = ?,
            updated_at = ?,
            completed_at = COALESCE(?, completed_at)
        WHERE id = ?
        "#,
    )
    .bind(response)
    .bind(now)
    .bind(now)
    .bind(next_status)
    .bind(now)
    .bind(if status == "needs_user_input" {
        Some(now)
    } else {
        None
    })
    .bind(&task_id)
    .execute(&workspace_pool)
    .await
    .map_err(|e| format!("Failed to save workspace task feedback: {}", e))?;

    append_task_feedback_to_manager_session(
        state.inner(),
        &workspace_pool,
        &workspace_id,
        &task_id,
        &title,
        response,
    )
    .await?;

    Ok(())
}

async fn append_task_feedback_to_manager_session(
    state: &AppState,
    pool: &DbPool,
    workspace_id: &str,
    task_id: &str,
    title: &str,
    response: &str,
) -> Result<(), String> {
    let descriptor = resolve_workspace_descriptor(state, Some(workspace_id.to_string()))?;
    let Some(session) = find_workspace_session(pool, state, &descriptor).await? else {
        return Ok(());
    };

    repository::create_message(
        pool,
        repository::CreateMessageParams {
            session_id: session.id,
            role: MessageRole::User,
            content: vec![ContentPart::Text {
                text: format!(
                    "User feedback for workspace task `{}` ({}):\n\n{}",
                    title, task_id, response
                ),
            }],
            provider_metadata: None,
        },
    )
    .await?;

    Ok(())
}

// =============================================================================
// Workspace CRUD — create, list, delete general workspaces
// =============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListEntry {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub enabled: bool,
    pub message_count: i64,
    pub artifact_count: i64,
    pub memory_count: i64,
    pub assigned_agent_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_manager_name: Option<String>,
    pub running_task_count: i64,
    pub blocked_task_count: i64,
    pub failed_task_count: i64,
    pub needs_user_input_task_count: i64,
    pub attention_task_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_attention_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_attention_task_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_attention_task_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_attention_task_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_attention_task_updated_at: Option<i64>,
    // Periodic-workspace surface — copied from the workspace's default
    // manager agent. Lets the Fleet UI sort scheduled workspaces ahead of
    // ad-hoc ones without each card needing a separate snapshot fetch.
    pub schedule_enabled: bool,
    pub schedule_paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_minutes: Option<u32>,
    // Seconds until the next scheduled run for this workspace's manager
    // agent, read live from the in-memory scheduler. None when the
    // scheduler has no entry for the manager (e.g. workspace was created
    // this session — the scheduler is only populated at startup).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_in_seconds: Option<u64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default)]
struct WorkspaceTaskAttentionSummary {
    running_task_count: i64,
    blocked_task_count: i64,
    failed_task_count: i64,
    needs_user_input_task_count: i64,
    latest_attention_task_id: Option<String>,
    latest_attention_task_title: Option<String>,
    latest_attention_task_status: Option<String>,
    latest_attention_task_summary: Option<String>,
    latest_attention_task_updated_at: Option<i64>,
}

impl WorkspaceTaskAttentionSummary {
    fn attention_task_count(&self) -> i64 {
        self.blocked_task_count + self.failed_task_count + self.needs_user_input_task_count
    }
}

async fn workspace_team_summary(
    state: &AppState,
    workspace_id: &str,
) -> Result<(usize, Option<String>), String> {
    let agents = workspace_agent_summaries(state, workspace_id).await?;
    let default_manager_name = agents
        .iter()
        .find(|agent| agent.is_default)
        .or_else(|| agents.iter().find(|agent| agent.role == "manager"))
        .map(|agent| agent.display_name.clone());

    // Count every agent. The workspace's default "main" agent is now a
    // first-class entry in the agents list (rendered as "Main" in the UI),
    // so it counts toward the workspace's headline agent total.
    let agent_count = agents.len();

    Ok((agent_count, default_manager_name))
}

/// Create a new general workspace with a UUID and filesystem root.
///
/// Per the workspace-local-agents refactor, every new workspace gets an
/// **empty Manager** agent: no system prompt, no skills, no MCP servers, no
/// provider connection, default execution policy. The user must configure
/// at least a provider before chatting.
#[tauri::command]
pub async fn workspace_create(
    title: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let manager_id = uuid::Uuid::new_v4().to_string();
    let now = now_millis();
    let display_title = title.unwrap_or_else(|| "New Workspace".to_string());

    let root = state.workspace_create_target(None)?.join(&id);
    let memory_dir = root.join(".clai").join("memory").join("journal");
    fs::create_dir_all(&memory_dir)
        .map_err(|e| format!("Failed to create workspace directory: {}", e))?;

    let config = WorkspaceConfig::new(id.clone(), display_title.clone(), now, manager_id);
    workspace_config::save(&root, &config).map_err(|e| e.to_string())?;
    let workspace_pool = crate::db::init_workspace_db(&root).await?;
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .insert_config(root.clone(), &config);
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .attach_pool(id.clone(), workspace_pool);

    tracing::info!(workspace_id = %id, title = %display_title, "Created new general workspace with empty manager");

    Ok(id)
}

/// List all file-backed workspaces.
#[tauri::command]
pub async fn workspace_list(state: State<'_, AppState>) -> Result<Vec<WorkspaceListEntry>, String> {
    let mut entries = Vec::new();
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();

    // Snapshot the scheduler once so each card can show a live "next run
    // in Xm" countdown without N lock acquisitions. Keyed by agent_id —
    // for workspaces, that's the manager workspace_agent's row id
    // (init.rs:populate_scheduler_from_workspace_agents registers with
    // workspace_agents.id as both the definition id and instance agent_id).
    let scheduler_seconds: HashMap<String, u64> = {
        let scheduler = state.scheduler.lock().await;
        scheduler
            .all_instances()
            .map(|instance| (instance.agent_id.clone(), instance.seconds_until_next_run()))
            .collect()
    };

    for locator in locators {
        let id = locator.id.clone();
        let (artifact_count, memory_count) = count_workspace_files(&locator.root_path);
        let workspace_pool = state.workspace_db(&id).await?;
        let message_count = count_session_messages(&workspace_pool, &id).await;
        let task_attention = workspace_task_attention_summary(&workspace_pool, &id).await?;
        let (assigned_agent_count, default_manager_name) =
            workspace_team_summary(state.inner(), &id).await?;

        let manager_id = Some(locator.default_agent_id.clone());
        let schedule_enabled = locator.schedule_enabled;
        let schedule_paused = locator.schedule_paused;
        let interval_minutes = locator.interval_minutes;

        // Only surface next_run for actually-scheduled workspaces that are not
        // paused. Paused workspaces keep their next_run_at intact in-memory
        // (so resume picks up from where they left off), but the FE would
        // mis-render a countdown that won't actually advance until resume.
        let next_run_in_seconds = if schedule_enabled && !schedule_paused {
            manager_id
                .as_deref()
                .and_then(|mid| scheduler_seconds.get(mid).copied())
        } else {
            None
        };

        entries.push(WorkspaceListEntry {
            id,
            kind: "general".to_string(),
            title: locator.title,
            agent_id: None,
            enabled: true,
            message_count,
            artifact_count,
            memory_count,
            assigned_agent_count,
            default_manager_name,
            running_task_count: task_attention.running_task_count,
            blocked_task_count: task_attention.blocked_task_count,
            failed_task_count: task_attention.failed_task_count,
            needs_user_input_task_count: task_attention.needs_user_input_task_count,
            attention_task_count: task_attention.attention_task_count(),
            latest_attention_task_id: task_attention.latest_attention_task_id,
            latest_attention_task_title: task_attention.latest_attention_task_title,
            latest_attention_task_status: task_attention.latest_attention_task_status,
            latest_attention_task_summary: task_attention.latest_attention_task_summary,
            latest_attention_task_updated_at: task_attention.latest_attention_task_updated_at,
            schedule_enabled,
            schedule_paused,
            interval_minutes,
            next_run_in_seconds,
            created_at: load_workspace_config_for_id(state.inner(), &locator.id)
                .map(|(_, config)| config.created_at)
                .unwrap_or_default(),
            updated_at: locator.updated_at,
        });
    }

    // Sort all entries by updated_at descending
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));

    Ok(entries)
}

/// Trigger an immediate run of the workspace's manager agent. Mirrors
/// `fleet_run_now`, but takes a workspace id instead of an agent id so the
/// Fleet UI's workspace cards (which don't carry the manager agent id in
/// the list payload) can wire a "run now" action without an extra lookup.
#[tauri::command]
pub async fn workspace_run_now(
    workspace_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;
    let (_root, config) = load_workspace_config_for_id(state.inner(), &workspace_id)?;
    let manager_id = config.default_agent_id.clone();
    let manager = config
        .agents
        .iter()
        .find(|agent| agent.id == manager_id)
        .ok_or_else(|| "Manager agent not found.".to_string())?;

    // The scheduler only registers agents whose schedule is enabled. An
    // explicit enabled/disabled check up front gives a clearer error than
    // the generic "no scheduler instance" message that `force_ready`
    // returns for unscheduled agents.
    //
    // Paused workspaces are *not* blocked here: `force_ready` sets a
    // one-shot `manual_run_pending` flag on the disabled instance, the
    // runner picks it up once, and `complete_agent` clears the flag so
    // the schedule stays paused afterward. Manual invocation is
    // intentionally orthogonal to auto-tick pause.
    if !manager.enabled {
        return Err("Manager agent is disabled. Enable it first.".to_string());
    }

    let mut scheduler = state.scheduler.lock().await;
    if scheduler.force_ready(&manager_id) {
        Ok(())
    } else {
        Err("Agent is currently running or is not scheduled.".to_string())
    }
}

/// Set the workspace's schedule (enable / disable + interval).
///
/// Writes `WorkspaceConfig.schedule.{enabled, interval_minutes}` and
/// reconciles the live scheduler so the change takes effect immediately —
/// no restart required.
#[tauri::command]
pub async fn workspace_set_schedule(
    workspace_id: String,
    enabled: bool,
    interval_minutes: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if enabled && interval_minutes == 0 {
        return Err("Schedule interval must be at least 1 minute.".to_string());
    }
    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;
    let (root, mut config) = load_workspace_config_for_id(state.inner(), &workspace_id)?;
    config.schedule.enabled = enabled;
    config.schedule.interval_minutes = interval_minutes;
    // Disabling the schedule clears any prior pause — there's nothing to be
    // paused if the workspace isn't scheduled.
    if !enabled {
        config.schedule.paused = false;
    }
    config.updated_at = now_millis();
    save_workspace_config_for_root(state.inner(), &root, &config)?;

    let mut scheduler = state.scheduler.lock().await;
    crate::agents::init::apply_workspace_schedule(&mut scheduler, &config);

    Ok(())
}

/// Pause or resume the workspace's periodic schedule.
///
/// Pausing keeps `schedule.enabled` true (interval is preserved) but
/// disables the live scheduler instance so the runner skips it until
/// resumed.
#[tauri::command]
pub async fn workspace_set_schedule_paused(
    workspace_id: String,
    paused: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;
    let (root, mut config) = load_workspace_config_for_id(state.inner(), &workspace_id)?;
    if !config.schedule.enabled {
        return Err("Workspace is not periodic.".to_string());
    }
    config.schedule.paused = paused;
    config.updated_at = now_millis();
    save_workspace_config_for_root(state.inner(), &root, &config)?;

    let mut scheduler = state.scheduler.lock().await;
    crate::agents::init::apply_workspace_schedule(&mut scheduler, &config);

    Ok(())
}

/// Delete a general workspace — removes metadata, session data, filesystem
/// root, and any in-memory state scoped to it (scheduler definitions /
/// instances, pending permission and path-grant queues). Without this
/// last step, deleting a scheduled workspace at runtime would leave the
/// scheduler firing on a vanished config until the next app restart.
#[tauri::command]
pub async fn workspace_delete(
    workspace_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;

    // Remove from the workspace index first; the returned locator carries
    // the on-disk root we still need below for both config-loading and
    // recursive removal. Closes the cached sqlx pool too.
    let locator = state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .remove_workspace(&workspace_id)
        .ok_or_else(|| format!("Workspace {} not found.", workspace_id))?;

    // Best-effort: read the config (still on disk at this point) to learn
    // which agent IDs the scheduler may have registered for this
    // workspace. A missing/corrupt config isn't fatal — we still want
    // disk cleanup to proceed, we just can't precisely purge scheduler
    // state for an unknowable agent set.
    let agent_ids: Vec<String> = workspace_config::load(&locator.root_path)
        .map(|cfg| cfg.agents.iter().map(|a| a.id.clone()).collect())
        .unwrap_or_default();

    // Drop scheduler entries for every agent the workspace owned. Any
    // task already mid-flight will continue running with its own copy
    // of context, but no further ticks will be scheduled — and the
    // definition map won't leak the stale entry until restart.
    if !agent_ids.is_empty() {
        let mut sched = state.scheduler.lock().await;
        for id in &agent_ids {
            sched.remove_instances_for_agent(id);
            sched.remove_definition(id);
        }
    }

    // Now wipe the on-disk root: `.clai/config.json`, `data.sqlite`,
    // `memory/`, plus any artifact files written into the workspace
    // directory.
    if locator.root_path.exists() {
        fs::remove_dir_all(&locator.root_path).map_err(|e| {
            format!(
                "Failed to delete workspace directory {}: {}",
                locator.root_path.display(),
                e
            )
        })?;
    }

    // Drain in-memory queues for this workspace. Pending bash-tool /
    // path-grant approvals get their oneshot channels dropped, which
    // surfaces as a closed-channel error on the awaiting side — correct,
    // since the workspace they were scoped to no longer exists.
    let purged_approvals = state.pending_approvals.purge_workspace(&workspace_id).await;
    let purged_path_grants = state
        .pending_path_grants
        .purge_workspace(&workspace_id)
        .await;

    tracing::info!(
        workspace_id = %workspace_id,
        agents_cleared = agent_ids.len(),
        approvals_purged = purged_approvals,
        path_grants_purged = purged_path_grants,
        "Deleted general workspace"
    );

    Ok(())
}

/// Rename a workspace.
#[tauri::command]
pub async fn workspace_set_title(
    workspace_id: String,
    title: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("Workspace name cannot be empty.".to_string());
    }
    if trimmed.chars().count() > 100 {
        return Err("Workspace name must be 100 characters or less.".to_string());
    }

    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;
    let (root, mut config) = load_workspace_config_for_id(state.inner(), &workspace_id)?;
    config.title = trimmed.to_string();
    config.updated_at = now_millis();
    save_workspace_config_for_root(state.inner(), &root, &config)?;

    Ok(())
}

/// Count artifacts and memories in a workspace's filesystem.
fn count_workspace_files(root: &Path) -> (i64, i64) {
    let memory_root = root.join(".clai").join("memory");
    let memory_count = count_files_recursive(&memory_root);

    let mut artifact_count: i64 = 0;
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) == Some(".clai") {
                continue;
            }
            if path.is_dir() {
                artifact_count += count_files_recursive(&path);
            } else if path.is_file() {
                artifact_count += 1;
            }
        }
    }

    (artifact_count, memory_count)
}

async fn workspace_task_attention_summary(
    pool: &DbPool,
    _workspace_id: &str,
) -> Result<WorkspaceTaskAttentionSummary, String> {
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN status = 'blocked' AND attention_acknowledged_at IS NULL AND user_response_at IS NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN status = 'failed' AND attention_acknowledged_at IS NULL AND user_response_at IS NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN status = 'needs_user_input' AND attention_acknowledged_at IS NULL AND user_response_at IS NULL THEN 1 ELSE 0 END), 0)
        FROM workspace_tasks
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to count workspace task attention: {}", e))?;

    #[allow(clippy::type_complexity)]
    let latest: Option<(String, String, String, Option<String>, Option<String>, i64)> =
        sqlx::query_as(
            r#"
            SELECT id, title, status, result_summary, error, updated_at
            FROM workspace_tasks
            WHERE status IN ('blocked', 'failed', 'needs_user_input')
                AND attention_acknowledged_at IS NULL
                AND user_response_at IS NULL
            ORDER BY
                CASE status
                    WHEN 'needs_user_input' THEN 0
                    WHEN 'blocked' THEN 1
                    WHEN 'failed' THEN 2
                    ELSE 3
                END,
                updated_at DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to load latest workspace attention task: {}", e))?;

    let mut summary = WorkspaceTaskAttentionSummary {
        running_task_count: counts.0,
        blocked_task_count: counts.1,
        failed_task_count: counts.2,
        needs_user_input_task_count: counts.3,
        ..Default::default()
    };

    if let Some((id, title, status, result_summary, error, updated_at)) = latest {
        summary.latest_attention_task_id = Some(id);
        summary.latest_attention_task_title = Some(title);
        summary.latest_attention_task_status = Some(status);
        summary.latest_attention_task_summary = error.or(result_summary);
        summary.latest_attention_task_updated_at = Some(updated_at);
    }

    Ok(summary)
}

fn count_files_recursive(dir: &Path) -> i64 {
    let mut count: i64 = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files_recursive(&path);
            } else if path.is_file() {
                count += 1;
            }
        }
    }
    count
}

async fn count_session_messages(pool: &DbPool, workspace_id: &str) -> i64 {
    let sessions = repository::list_sessions(pool).await.unwrap_or_default();

    let session = sessions.iter().find(|s| {
        s.context.workspace_id.as_deref() == Some(workspace_id)
            || s.context.agent_workspace_id.as_deref() == Some(workspace_id)
            || s.context.automation_id.as_deref() == Some(workspace_id)
    });

    if let Some(session) = session {
        repository::list_messages(pool, &session.id)
            .await
            .map(|msgs| msgs.len() as i64)
            .unwrap_or(0)
    } else {
        0
    }
}

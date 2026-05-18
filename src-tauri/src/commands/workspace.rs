//! Workspace state persistence commands.
//!
//! These commands handle saving and loading the workspace state
//! (tabs, commands, layout) to/from SQLite.

use crate::assistant::repository;
use crate::assistant::tools::local::agent_workspace_root_for_id;
use crate::assistant::types::{
    AssistantMessage, AssistantRun, AssistantSession, ContentPart, MessageRole, SessionContext,
    SessionKind, ToolInvocation, WorkspaceAgentSummary,
};
use crate::config::{agent_instructions_with_skills, AgentConfig, ExecutionCapabilityConfig};
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

/// Complete workspace state
#[derive(Debug, Clone, Serialize, Deserialize)]
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
const MAX_PREVIEW_CHARS: usize = 280;
const MAX_FILE_CONTENT_BYTES: usize = 200_000;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_in_seconds: Option<u64>,
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
pub struct WorkspaceAssignAgentRequest {
    pub workspace_id: String,
    pub agent_definition_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
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

// Type aliases for SQLite query results to satisfy clippy::type_complexity
type TabRow = (String, String, String, String, Option<i64>, i64, i64);
type CommandRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
);

#[derive(Debug, Clone)]
struct VirtualWorkspaceArtifact {
    entry: WorkspaceFileEntry,
    content: String,
}

#[derive(Debug, Clone)]
struct WorkspaceAgentRow {
    id: String,
    workspace_id: String,
    agent_definition_id: String,
    display_name: Option<String>,
    role: String,
    enabled: bool,
    created_at: i64,
    updated_at: i64,
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn normalize_workspace_id(workspace_id: Option<String>) -> String {
    workspace_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_string())
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

fn read_text_preview(path: &Path, max_bytes: usize, max_chars: usize) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let bytes = if bytes.len() > max_bytes {
        &bytes[..max_bytes]
    } else {
        &bytes[..]
    };
    let text = String::from_utf8(bytes.to_vec()).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut preview: String = trimmed.chars().take(max_chars).collect();
    if trimmed.chars().count() > max_chars {
        preview.push('…');
    }
    Some(preview)
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
        preview: read_text_preview(path, 2048, MAX_PREVIEW_CHARS),
    })
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
            if skip_clai && path.file_name().and_then(|value| value.to_str()) == Some(".clai") {
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
    let workspace_id = normalize_workspace_id(workspace_id);

    // Legacy "default" workspace — treat as a general workspace with no root
    if workspace_id == DEFAULT_WORKSPACE_ID {
        return Ok(WorkspaceDescriptor {
            workspace_id,
            kind: "generic".to_string(),
            title: "General".to_string(),
            agent_id: None,
            root_path: None,
            provider_connection_ids: Vec::new(),
            selected_mcp_server_ids: Vec::new(),
            selected_mcp_server_names: Vec::new(),
            execution: ExecutionCapabilityConfig::default(),
            tool_scopes: Vec::new(),
            automation_name: None,
            automation_description: None,
        });
    }

    // Try agent config first
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|error| format!("Lock error: {}", error))?;

    if let Some(agent) = config_manager.get_agent(&workspace_id) {
        let config = config_manager.get();
        let mcp_servers = config_manager.get_mcp_servers();
        let selected_mcp_server_names = agent
            .selected_mcp_server_ids
            .iter()
            .filter_map(|id| {
                mcp_servers
                    .iter()
                    .find(|server| server.id == *id)
                    .map(|server| server.name.clone())
            })
            .collect();
        drop(config_manager);
        let runtime_description = agent_instructions_with_skills(&config, &agent);
        let mut descriptor = workspace_descriptor_from_agent(agent, selected_mcp_server_names)?;
        descriptor.automation_description = Some(runtime_description);
        return Ok(descriptor);
    }

    drop(config_manager);

    // Not an agent — treat as a general workspace with a filesystem root.
    // The workspace may or may not have a row in the `workspaces` table yet
    // (the row is created by workspace_create, but the descriptor works
    // regardless so that workspace_get_snapshot and other commands can
    // function for any UUID-shaped workspace ID).
    let root_path = agent_workspace_root_for_id(&workspace_id)
        .ok_or_else(|| format!("Could not resolve workspace root for {}", workspace_id))?;

    let mut execution = ExecutionCapabilityConfig::default();
    execution.web.enabled = true;

    Ok(WorkspaceDescriptor {
        workspace_id,
        kind: "general".to_string(),
        title: "Workspace".to_string(),
        agent_id: None,
        root_path: Some(root_path),
        provider_connection_ids: Vec::new(),
        selected_mcp_server_ids: Vec::new(),
        selected_mcp_server_names: Vec::new(),
        execution,
        tool_scopes: vec!["fs".to_string(), "web".to_string()],
        automation_name: None,
        automation_description: None,
    })
}

// Unreachable after the refactor above, but kept for reference during migration.
// The original flow that only handled agents:
#[allow(dead_code)]
fn resolve_workspace_descriptor_agent_only(
    state: &AppState,
    workspace_id: &str,
) -> Result<WorkspaceDescriptor, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|error| format!("Lock error: {}", error))?;
    let agent = config_manager
        .get_agent(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let mcp_servers = config_manager.get_mcp_servers();
    let selected_mcp_server_names = agent
        .selected_mcp_server_ids
        .iter()
        .filter_map(|id| {
            mcp_servers
                .iter()
                .find(|server| server.id == *id)
                .map(|server| server.name.clone())
        })
        .collect();

    workspace_descriptor_from_agent(agent, selected_mcp_server_names)
}

async fn resolve_workspace_provider_selection(
    pool: &DbPool,
    descriptor: &WorkspaceDescriptor,
) -> Result<WorkspaceProviderSelection, String> {
    let connections = repository::list_provider_connections(pool).await?;
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

    // Check for a user-selected preferred provider in the workspaces table
    let preferred: Option<String> = sqlx::query_scalar(
        "SELECT preferred_provider_connection_id FROM workspaces WHERE id = ? LIMIT 1",
    )
    .bind(&descriptor.workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to check workspace provider preference: {}", e))?
    .flatten();

    let preferred_connection_id = preferred
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

fn workspace_descriptor_from_agent(
    agent: AgentConfig,
    selected_mcp_server_names: Vec<String>,
) -> Result<WorkspaceDescriptor, String> {
    let root_path = agent_workspace_root_for_id(&agent.id)
        .ok_or_else(|| format!("Could not resolve workspace root for agent {}", agent.id))?;

    Ok(WorkspaceDescriptor {
        workspace_id: agent.id.clone(),
        kind: "agent".to_string(),
        title: agent.name.clone(),
        agent_id: Some(agent.id.clone()),
        root_path: Some(root_path),
        provider_connection_ids: agent.provider_connection_ids.clone(),
        selected_mcp_server_ids: agent.selected_mcp_server_ids.clone(),
        selected_mcp_server_names,
        execution: agent.execution.clone(),
        tool_scopes: agent
            .required_tools()
            .into_iter()
            .map(str::to_string)
            .collect(),
        automation_name: Some(agent.name),
        automation_description: Some(agent.description),
    })
}

fn agent_runtime_description(state: &AppState, agent: &AgentConfig) -> String {
    let config = state.config_manager.lock().map(|manager| manager.get());

    match config {
        Ok(config) => agent_instructions_with_skills(&config, agent),
        Err(_) => agent.description.clone(),
    }
}

fn normalize_workspace_agent_role(role: Option<String>) -> Result<String, String> {
    let normalized = role
        .unwrap_or_else(|| "member".to_string())
        .trim()
        .to_ascii_lowercase();

    match normalized.as_str() {
        "" => Ok("member".to_string()),
        "member" | "manager" => Ok(normalized),
        _ => Err("Workspace agent role must be either 'member' or 'manager'.".to_string()),
    }
}

fn select_default_agent_definition_id(state: &AppState) -> Result<Option<String>, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|error| format!("Lock error: {}", error))?;

    let agents = config_manager.get_agents();
    Ok(agents
        .iter()
        .find(|agent| agent.is_default())
        .or_else(|| agents.first())
        .map(|agent| agent.id.clone()))
}

async fn workspace_default_agent_id(
    pool: &DbPool,
    workspace_id: &str,
) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT default_workspace_agent_id FROM workspaces WHERE id = ? LIMIT 1")
        .bind(workspace_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to load workspace default agent: {}", e))
        .map(|value: Option<Option<String>>| value.flatten())
}

async fn set_workspace_default_agent_id(
    pool: &DbPool,
    workspace_id: &str,
    workspace_agent_id: Option<&str>,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE workspaces SET default_workspace_agent_id = ?, updated_at = ? WHERE id = ?",
    )
    .bind(workspace_agent_id)
    .bind(now_millis())
    .bind(workspace_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update workspace default agent: {}", e))?;

    Ok(())
}

async fn load_workspace_agent_rows(
    pool: &DbPool,
    workspace_id: &str,
) -> Result<Vec<WorkspaceAgentRow>, String> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, String, String, Option<String>, String, i64, i64, i64)> =
        sqlx::query_as(
            r#"
            SELECT id, workspace_id, agent_definition_id, display_name, role, enabled, created_at, updated_at
            FROM workspace_agents
            WHERE workspace_id = ?
            ORDER BY CASE role WHEN 'manager' THEN 0 ELSE 1 END, created_at ASC
            "#,
        )
        .bind(workspace_id)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to load workspace agents: {}", e))?;

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                workspace_id,
                agent_definition_id,
                display_name,
                role,
                enabled,
                created_at,
                updated_at,
            )| WorkspaceAgentRow {
                id,
                workspace_id,
                agent_definition_id,
                display_name,
                role,
                enabled: enabled != 0,
                created_at,
                updated_at,
            },
        )
        .collect())
}

fn workspace_agent_response_from_row(
    row: WorkspaceAgentRow,
    agent: Option<&AgentConfig>,
    default_workspace_agent_id: Option<&str>,
) -> WorkspaceAgentResponse {
    let display_name = row
        .display_name
        .clone()
        .or_else(|| agent.map(|agent| agent.name.clone()))
        .unwrap_or_else(|| row.agent_definition_id.clone());

    WorkspaceAgentResponse {
        id: row.id.clone(),
        workspace_id: row.workspace_id,
        agent_definition_id: row.agent_definition_id,
        display_name,
        role: row.role,
        enabled: row.enabled,
        is_default: default_workspace_agent_id == Some(row.id.as_str()),
        agent_name: agent.map(|agent| agent.name.clone()),
        agent_description: agent.map(|agent| agent.description.clone()),
        provider_connection_ids: agent
            .map(|agent| agent.provider_connection_ids.clone())
            .unwrap_or_default(),
        skill_ids: agent
            .map(|agent| agent.selected_skill_ids.clone())
            .unwrap_or_default(),
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
    pool: &DbPool,
    state: &AppState,
    workspace_id: &str,
) -> Result<(Vec<WorkspaceAgentResponse>, Option<String>), String> {
    let rows = load_workspace_agent_rows(pool, workspace_id).await?;
    let default_workspace_agent_id = workspace_default_agent_id(pool, workspace_id).await?;
    let agents_by_id: HashMap<String, AgentConfig> = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|error| format!("Lock error: {}", error))?;
        config_manager
            .get_agents()
            .into_iter()
            .map(|agent| (agent.id.clone(), agent))
            .collect()
    };

    let responses = rows
        .into_iter()
        .map(|row| {
            let agent = agents_by_id.get(&row.agent_definition_id);
            workspace_agent_response_from_row(row, agent, default_workspace_agent_id.as_deref())
        })
        .collect();

    Ok((responses, default_workspace_agent_id))
}

async fn list_workspace_task_responses(
    pool: &DbPool,
    state: &AppState,
    workspace_id: &str,
) -> Result<Vec<WorkspaceTaskResponse>, String> {
    let rows = sqlx::query(
        r#"
        SELECT
            task.id,
            task.workspace_id,
            task.created_by_workspace_agent_id,
            creator.display_name AS created_by_display_name,
            task.assigned_to_workspace_agent_id,
            task.assigned_agent_definition_id,
            assigned.display_name AS assigned_display_name,
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
        LEFT JOIN workspace_agents assigned
            ON assigned.id = task.assigned_to_workspace_agent_id
        LEFT JOIN workspace_agents creator
            ON creator.id = task.created_by_workspace_agent_id
        WHERE task.workspace_id = ?
        ORDER BY task.updated_at DESC, task.created_at DESC
        LIMIT 50
        "#,
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load workspace tasks: {}", e))?;

    let agents_by_id: HashMap<String, AgentConfig> = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|error| format!("Lock error: {}", error))?;
        config_manager
            .get_agents()
            .into_iter()
            .map(|agent| (agent.id.clone(), agent))
            .collect()
    };

    Ok(rows
        .into_iter()
        .map(|row| {
            let assigned_agent_definition_id: String = row.get("assigned_agent_definition_id");
            let assigned_display_name: Option<String> = row.get("assigned_display_name");
            let assigned_agent_display_name = assigned_display_name
                .or_else(|| {
                    agents_by_id
                        .get(&assigned_agent_definition_id)
                        .map(|agent| agent.name.clone())
                })
                .unwrap_or_else(|| assigned_agent_definition_id.clone());

            WorkspaceTaskResponse {
                id: row.get("id"),
                workspace_id: row.get("workspace_id"),
                created_by_workspace_agent_id: row.get("created_by_workspace_agent_id"),
                created_by_display_name: row.get("created_by_display_name"),
                assigned_to_workspace_agent_id: row.get("assigned_to_workspace_agent_id"),
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

async fn workspace_agent_summaries(
    pool: &DbPool,
    state: &AppState,
    workspace_id: &str,
) -> Result<Vec<WorkspaceAgentSummary>, String> {
    let (agents, _) = list_workspace_agent_responses(pool, state, workspace_id).await?;

    Ok(agents
        .into_iter()
        .map(|agent| WorkspaceAgentSummary {
            id: agent.id,
            agent_definition_id: agent.agent_definition_id,
            display_name: agent.display_name,
            role: agent.role,
            is_default: agent.is_default,
            description: concise_agent_description(agent.agent_description),
        })
        .collect())
}

fn resolve_workspace_manager_agent(
    state: &AppState,
    workspace_agents: &[WorkspaceAgentSummary],
) -> Result<Option<AgentConfig>, String> {
    let Some(manager) = workspace_agents
        .iter()
        .find(|agent| agent.is_default)
        .or_else(|| {
            workspace_agents
                .iter()
                .find(|agent| agent.role == "manager")
        })
    else {
        return Ok(None);
    };

    let config_manager = state
        .config_manager
        .lock()
        .map_err(|error| format!("Lock error: {}", error))?;

    Ok(config_manager.get_agent(&manager.agent_definition_id))
}

async fn assign_workspace_agent_row(
    pool: &DbPool,
    workspace_id: &str,
    agent_definition_id: &str,
    display_name: Option<String>,
    role: &str,
) -> Result<String, String> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM workspace_agents WHERE workspace_id = ? AND agent_definition_id = ? LIMIT 1",
    )
    .bind(workspace_id)
    .bind(agent_definition_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to check workspace agent assignment: {}", e))?;

    if let Some(id) = existing {
        sqlx::query("UPDATE workspace_agents SET display_name = COALESCE(?, display_name), role = ?, updated_at = ? WHERE id = ?")
            .bind(display_name)
            .bind(role)
            .bind(now_millis())
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to update workspace agent assignment: {}", e))?;
        return Ok(id);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_millis();
    sqlx::query(
        r#"
        INSERT INTO workspace_agents (
            id, workspace_id, agent_definition_id, display_name, role, enabled, created_at, updated_at
        )
        VALUES (?, ?, ?, ?, ?, 1, ?, ?)
        "#,
    )
    .bind(&id)
    .bind(workspace_id)
    .bind(agent_definition_id)
    .bind(display_name)
    .bind(role)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to assign agent to workspace: {}", e))?;

    Ok(id)
}

async fn ensure_workspace_manager_assignment(
    pool: &DbPool,
    state: &AppState,
    workspace_id: &str,
) -> Result<Option<String>, String> {
    if workspace_default_agent_id(pool, workspace_id)
        .await?
        .is_some()
    {
        return Ok(None);
    }

    let Some(agent_definition_id) = select_default_agent_definition_id(state)? else {
        return Ok(None);
    };

    let workspace_agent_id =
        assign_workspace_agent_row(pool, workspace_id, &agent_definition_id, None, "manager")
            .await?;
    set_workspace_default_agent_id(pool, workspace_id, Some(&workspace_agent_id)).await?;

    Ok(Some(workspace_agent_id))
}

async fn find_workspace_session(
    pool: &DbPool,
    descriptor: &WorkspaceDescriptor,
) -> Result<Option<AssistantSession>, String> {
    let sessions = repository::list_sessions(pool, None).await?;

    Ok(sessions
        .into_iter()
        .filter(|session| {
            session.context.workspace_id.as_deref() == Some(descriptor.workspace_id.as_str())
                || descriptor
                    .agent_id
                    .as_deref()
                    .map(|agent_id| {
                        session.context.agent_workspace_id.as_deref() == Some(agent_id)
                            || session.context.automation_id.as_deref() == Some(agent_id)
                    })
                    .unwrap_or(false)
        })
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
        tab_id: existing_session.and_then(|session| session.tab_id.clone()),
        tool_scopes: descriptor.tool_scopes.clone(),
        mcp_server_ids,
        execution,
        netdata_conversation_id: existing_session
            .and_then(|session| session.context.netdata_conversation_id.clone()),
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

async fn load_workspace_state_from_pool(pool: &DbPool) -> Result<WorkspaceState, String> {
    // Load active tab ID from meta
    let active_tab_id: Option<String> =
        sqlx::query_scalar("SELECT value FROM workspace_meta WHERE key = 'active_tab_id'")
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to load active tab ID: {}", e))?;

    // Load all tabs ordered by position
    let tab_rows: Vec<TabRow> = sqlx::query_as(
        "SELECT id, title, root_tile, context, position, created_at, updated_at FROM tabs ORDER BY position",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load tabs: {}", e))?;

    let mut tabs = HashMap::new();
    let mut tab_order = Vec::new();
    for (id, title, root_tile_json, context_json, _position, created_at, _updated_at) in tab_rows {
        let root_tile: TileNode = serde_json::from_str(&root_tile_json)
            .map_err(|e| format!("Failed to parse root_tile for tab {}: {}", id, e))?;
        let context: TabContext = serde_json::from_str(&context_json)
            .map_err(|e| format!("Failed to parse context for tab {}: {}", id, e))?;

        tab_order.push(id.clone());
        tabs.insert(
            id.clone(),
            Tab {
                id,
                title,
                created_at,
                root_tile,
                context,
            },
        );
    }

    // Load all commands
    let cmd_rows: Vec<CommandRow> = sqlx::query_as(
        "SELECT id, tab_id, tile_id, type, args, state, created_at, updated_at FROM commands",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load commands: {}", e))?;

    let mut commands = HashMap::new();
    for (id, tab_id, tile_id, command_type, args_json, state_json, created_at, updated_at) in
        cmd_rows
    {
        let args: serde_json::Value = args_json
            .map(|s| {
                serde_json::from_str(&s).unwrap_or(serde_json::Value::Object(Default::default()))
            })
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let state: serde_json::Value = state_json
            .map(|s| {
                serde_json::from_str(&s).unwrap_or(serde_json::Value::Object(Default::default()))
            })
            .unwrap_or(serde_json::Value::Object(Default::default()));

        commands.insert(
            id.clone(),
            Command {
                id,
                command_type,
                args,
                tab_id,
                tile_id,
                created_at,
                state,
                updated_at: Some(updated_at),
            },
        );
    }

    Ok(WorkspaceState {
        active_tab_id,
        tab_order,
        tabs,
        commands,
    })
}

/// Load workspace state from SQLite
#[tauri::command]
pub async fn load_workspace_state(pool: State<'_, DbPool>) -> Result<WorkspaceState, String> {
    tracing::debug!("Loading workspace state from database");
    let state = load_workspace_state_from_pool(pool.inner()).await?;

    tracing::debug!(
        "Loaded workspace state: {} tabs, {} commands",
        state.tabs.len(),
        state.commands.len()
    );

    Ok(state)
}

/// Save workspace state to SQLite
#[tauri::command]
pub async fn save_workspace_state(
    workspace_state: WorkspaceState,
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    tracing::debug!(
        "Saving workspace state: {} tabs, {} commands",
        workspace_state.tabs.len(),
        workspace_state.commands.len()
    );

    let now = chrono::Utc::now().timestamp_millis();

    // Start a transaction
    let mut tx = pool
        .inner()
        .begin()
        .await
        .map_err(|e| format!("Failed to start transaction: {}", e))?;

    // Save active tab ID to meta
    if let Some(ref active_tab_id) = workspace_state.active_tab_id {
        sqlx::query(
            "INSERT OR REPLACE INTO workspace_meta (key, value) VALUES ('active_tab_id', ?)",
        )
        .bind(active_tab_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to save active tab ID: {}", e))?;
    } else {
        sqlx::query("DELETE FROM workspace_meta WHERE key = 'active_tab_id'")
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("Failed to clear active tab ID: {}", e))?;
    }

    // Get existing tab IDs to detect deletions
    let existing_tab_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM tabs")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to get existing tab IDs: {}", e))?;

    // Delete tabs that are no longer in the state
    for existing_id in &existing_tab_ids {
        if !workspace_state.tabs.contains_key(existing_id) {
            // Commands will be deleted via CASCADE
            sqlx::query("DELETE FROM tabs WHERE id = ?")
                .bind(existing_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to delete tab {}: {}", existing_id, e))?;
        }
    }

    // Upsert tabs using tab_order for position (fall back to HashMap iteration if tab_order is empty)
    let tab_ids: Vec<&String> = if workspace_state.tab_order.is_empty() {
        workspace_state.tabs.keys().collect()
    } else {
        workspace_state.tab_order.iter().collect()
    };

    for (position, id) in tab_ids.iter().enumerate() {
        let tab = match workspace_state.tabs.get(*id) {
            Some(t) => t,
            None => continue, // Skip if tab_order contains an ID not in tabs
        };

        let root_tile_json = serde_json::to_string(&tab.root_tile)
            .map_err(|e| format!("Failed to serialize root_tile: {}", e))?;
        let context_json = serde_json::to_string(&tab.context)
            .map_err(|e| format!("Failed to serialize context: {}", e))?;

        sqlx::query(
            r#"
            INSERT INTO tabs (id, title, root_tile, context, position, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                root_tile = excluded.root_tile,
                context = excluded.context,
                position = excluded.position,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(*id)
        .bind(&tab.title)
        .bind(&root_tile_json)
        .bind(&context_json)
        .bind(position as i64)
        .bind(tab.created_at)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to upsert tab {}: {}", id, e))?;
    }

    // Get existing command IDs to detect deletions
    let existing_cmd_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM commands")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to get existing command IDs: {}", e))?;

    // Delete commands that are no longer in the state
    for existing_id in &existing_cmd_ids {
        if !workspace_state.commands.contains_key(existing_id) {
            sqlx::query("DELETE FROM commands WHERE id = ?")
                .bind(existing_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to delete command {}: {}", existing_id, e))?;
        }
    }

    // Upsert commands
    for (id, cmd) in &workspace_state.commands {
        let args_json = serde_json::to_string(&cmd.args)
            .map_err(|e| format!("Failed to serialize args: {}", e))?;
        let state_json = serde_json::to_string(&cmd.state)
            .map_err(|e| format!("Failed to serialize state: {}", e))?;

        sqlx::query(
            r#"
            INSERT INTO commands (id, tab_id, tile_id, type, args, state, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                tab_id = excluded.tab_id,
                tile_id = excluded.tile_id,
                type = excluded.type,
                args = excluded.args,
                state = excluded.state,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id)
        .bind(&cmd.tab_id)
        .bind(&cmd.tile_id)
        .bind(&cmd.command_type)
        .bind(&args_json)
        .bind(&state_json)
        .bind(cmd.created_at)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to upsert command {}: {}", id, e))?;
    }

    // Commit transaction
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit transaction: {}", e))?;

    tracing::debug!("Workspace state saved successfully");
    Ok(())
}

#[tauri::command]
pub async fn workspace_get_snapshot(
    workspace_id: Option<String>,
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<WorkspaceSnapshot, String> {
    let descriptor = resolve_workspace_descriptor(state.inner(), workspace_id)?;
    if let Some(root_path) = &descriptor.root_path {
        ensure_agent_workspace_root(root_path)?;
    }

    let provider_selection =
        resolve_workspace_provider_selection(pool.inner(), &descriptor).await?;

    let session = find_workspace_session(pool.inner(), &descriptor).await?;
    let (messages, runs, tool_calls) = if let Some(session) = &session {
        (
            repository::list_messages(pool.inner(), &session.id).await?,
            repository::list_runs(pool.inner(), &session.id).await?,
            repository::list_tool_calls(pool.inner(), &session.id, None).await?,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    let (memories, artifacts) = if let Some(root_path) = &descriptor.root_path {
        let memory_root = root_path.join(".clai").join("memory");
        let mut memories = Vec::new();
        if memory_root.exists() {
            collect_files(&memory_root, root_path, &mut memories, false)?;
        }

        let mut artifacts = Vec::new();
        collect_files(root_path, root_path, &mut artifacts, true)?;
        sort_workspace_entries(&mut memories);
        sort_workspace_entries(&mut artifacts);
        (memories, artifacts)
    } else {
        (Vec::new(), Vec::new())
    };

    let workspace_state = load_workspace_state_from_pool(pool.inner()).await?;
    let virtual_artifacts = workspace_virtual_artifacts(&descriptor, &workspace_state)?;
    let artifacts = merge_workspace_artifacts(artifacts, virtual_artifacts);
    let (assigned_agents, default_workspace_agent_id) =
        list_workspace_agent_responses(pool.inner(), state.inner(), &descriptor.workspace_id)
            .await?;
    let tasks =
        list_workspace_task_responses(pool.inner(), state.inner(), &descriptor.workspace_id)
            .await?;

    // Resolve agent schedule info from config + scheduler
    let (enabled, interval_minutes, next_run_in_seconds) =
        if let Some(ref agent_id) = descriptor.agent_id {
            let agent_enabled = {
                let config = state
                    .config_manager
                    .lock()
                    .map_err(|e| format!("Lock error: {}", e))?;
                config
                    .get_agent(agent_id)
                    .map(|a| (a.enabled, a.interval_minutes))
            };
            let next_run = {
                let scheduler = state.scheduler.lock().await;
                let mut found = None;
                for inst in scheduler.all_instances() {
                    if inst.agent_id == *agent_id {
                        found = Some(inst.seconds_until_next_run());
                        break;
                    }
                }
                found
            };
            match agent_enabled {
                Some((en, interval)) => (Some(en), Some(interval), next_run),
                None => (None, None, None),
            }
        } else {
            (None, None, None)
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
        interval_minutes,
        next_run_in_seconds,
    })
}

#[tauri::command]
pub async fn workspace_get_or_create_session(
    workspace_id: Option<String>,
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<WorkspaceSessionBinding, String> {
    let descriptor = resolve_workspace_descriptor(state.inner(), workspace_id)?;
    if let Some(root_path) = &descriptor.root_path {
        ensure_agent_workspace_root(root_path)?;
    }

    let provider_selection =
        resolve_workspace_provider_selection(pool.inner(), &descriptor).await?;
    let provider_connection_id = provider_selection.preferred_connection_id;
    let existing = find_workspace_session(pool.inner(), &descriptor).await?;
    let workspace_agents =
        workspace_agent_summaries(pool.inner(), state.inner(), &descriptor.workspace_id).await?;
    let workspace_manager = resolve_workspace_manager_agent(state.inner(), &workspace_agents)?;
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
            repository::update_session(pool.inner(), &updated).await?
        } else {
            existing
        }
    } else {
        repository::create_session(
            pool.inner(),
            repository::CreateSessionParams {
                tab_id: None,
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
    pool: State<'_, DbPool>,
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
            let workspace_state = load_workspace_state_from_pool(pool.inner()).await?;
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
    pool: State<'_, DbPool>,
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
            let workspace_state = load_workspace_state_from_pool(pool.inner()).await?;
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
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let existing = find_workspace_session(pool.inner(), &descriptor).await?;
    let workspace_agents =
        workspace_agent_summaries(pool.inner(), state.inner(), &descriptor.workspace_id).await?;
    let workspace_manager = resolve_workspace_manager_agent(state.inner(), &workspace_agents)?;

    let session = if let Some(session) = existing {
        session
    } else {
        // No session yet — create one so MCP preferences are persisted
        // before the user sends their first message.
        repository::create_session(
            pool.inner(),
            repository::CreateSessionParams {
                tab_id: None,
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
    updated.context.mcp_server_ids = request.mcp_server_ids;
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
        }
    }
    updated.updated_at = chrono::Utc::now().timestamp_millis();
    repository::update_session(pool.inner(), &updated).await?;

    Ok(())
}

/// Set the preferred provider connection for a general workspace.
/// Upserts into the workspaces table so the preference survives across sessions.
#[tauri::command]
pub async fn workspace_set_provider(
    workspace_id: String,
    provider_connection_id: String,
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();

    // Upsert: insert if not exists, update if exists
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, kind, title, preferred_provider_connection_id, created_at, updated_at)
        VALUES (?, 'general', 'Workspace', ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET preferred_provider_connection_id = ?, updated_at = ?
        "#,
    )
    .bind(&workspace_id)
    .bind(&provider_connection_id)
    .bind(now)
    .bind(now)
    .bind(&provider_connection_id)
    .bind(now)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to set workspace provider: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn workspace_list_agents(
    workspace_id: String,
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<Vec<WorkspaceAgentResponse>, String> {
    let workspace_id = normalize_workspace_id(Some(workspace_id));
    let (agents, _) =
        list_workspace_agent_responses(pool.inner(), state.inner(), &workspace_id).await?;
    Ok(agents)
}

#[tauri::command]
pub async fn workspace_assign_agent(
    request: WorkspaceAssignAgentRequest,
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<WorkspaceAgentResponse, String> {
    let workspace_id = normalize_workspace_id(Some(request.workspace_id));
    let agent_definition_id = request.agent_definition_id.trim().to_string();
    if agent_definition_id.is_empty() {
        return Err("Agent definition ID is required.".to_string());
    }

    let role = normalize_workspace_agent_role(request.role)?;

    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|error| format!("Lock error: {}", error))?;
        if config_manager.get_agent(&agent_definition_id).is_none() {
            return Err(format!(
                "Agent definition not found: {}",
                agent_definition_id
            ));
        }
    }

    let workspace_agent_id = assign_workspace_agent_row(
        pool.inner(),
        &workspace_id,
        &agent_definition_id,
        request.display_name,
        &role,
    )
    .await?;

    let current_default = workspace_default_agent_id(pool.inner(), &workspace_id).await?;
    if role == "manager" || current_default.is_none() {
        let now = now_millis();
        sqlx::query(
            "UPDATE workspace_agents SET role = 'member', updated_at = ? WHERE workspace_id = ? AND id != ? AND role = 'manager'",
        )
        .bind(now)
        .bind(&workspace_id)
        .bind(&workspace_agent_id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to update workspace agent roles: {}", e))?;
        sqlx::query("UPDATE workspace_agents SET role = 'manager', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&workspace_agent_id)
            .execute(pool.inner())
            .await
            .map_err(|e| format!("Failed to mark workspace agent as manager: {}", e))?;
        set_workspace_default_agent_id(pool.inner(), &workspace_id, Some(&workspace_agent_id))
            .await?;
    }

    let (agents, _) =
        list_workspace_agent_responses(pool.inner(), state.inner(), &workspace_id).await?;
    agents
        .into_iter()
        .find(|agent| agent.id == workspace_agent_id)
        .ok_or_else(|| "Workspace agent assignment not found after save.".to_string())
}

#[tauri::command]
pub async fn workspace_unassign_agent(
    workspace_agent_id: String,
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT workspace_id, role FROM workspace_agents WHERE id = ? LIMIT 1")
            .bind(&workspace_agent_id)
            .fetch_optional(pool.inner())
            .await
            .map_err(|e| format!("Failed to load workspace agent assignment: {}", e))?;

    let Some((workspace_id, role)) = row else {
        return Err(format!(
            "Workspace agent assignment not found: {}",
            workspace_agent_id
        ));
    };

    if workspace_default_agent_id(pool.inner(), &workspace_id)
        .await?
        .as_deref()
        == Some(workspace_agent_id.as_str())
        || role == "manager"
    {
        return Err(
            "Cannot remove the workspace manager. Set another agent as manager first.".to_string(),
        );
    }

    sqlx::query("DELETE FROM workspace_agents WHERE id = ?")
        .bind(&workspace_agent_id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to unassign workspace agent: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn workspace_set_default_agent(
    workspace_id: String,
    workspace_agent_id: String,
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    let workspace_id = normalize_workspace_id(Some(workspace_id));
    let row: Option<(String,)> =
        sqlx::query_as("SELECT workspace_id FROM workspace_agents WHERE id = ? LIMIT 1")
            .bind(&workspace_agent_id)
            .fetch_optional(pool.inner())
            .await
            .map_err(|e| format!("Failed to load workspace agent assignment: {}", e))?;

    let Some((assigned_workspace_id,)) = row else {
        return Err(format!(
            "Workspace agent assignment not found: {}",
            workspace_agent_id
        ));
    };

    if assigned_workspace_id != workspace_id {
        return Err("Workspace agent assignment does not belong to this workspace.".to_string());
    }

    let now = now_millis();
    sqlx::query(
        "UPDATE workspace_agents SET role = 'member', updated_at = ? WHERE workspace_id = ? AND id != ? AND role = 'manager'",
    )
    .bind(now)
    .bind(&workspace_id)
    .bind(&workspace_agent_id)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to update workspace agent roles: {}", e))?;

    sqlx::query("UPDATE workspace_agents SET role = 'manager', updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(&workspace_agent_id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to mark workspace agent as manager: {}", e))?;

    set_workspace_default_agent_id(pool.inner(), &workspace_id, Some(&workspace_agent_id)).await?;

    Ok(())
}

#[tauri::command]
pub async fn workspace_acknowledge_task(
    request: WorkspaceTaskActionRequest,
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    let workspace_id = normalize_workspace_id(Some(request.workspace_id));
    let now = now_millis();
    let result = sqlx::query(
        r#"
        UPDATE workspace_tasks
        SET attention_acknowledged_at = ?,
            updated_at = ?
        WHERE id = ? AND workspace_id = ?
        "#,
    )
    .bind(now)
    .bind(now)
    .bind(&request.task_id)
    .bind(&workspace_id)
    .execute(pool.inner())
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
    pool: State<'_, DbPool>,
) -> Result<(), String> {
    let workspace_id = normalize_workspace_id(Some(request.workspace_id));
    let response = request.response.trim();
    if response.is_empty() {
        return Err("Task feedback cannot be empty.".to_string());
    }

    let current: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, title, status FROM workspace_tasks WHERE id = ? AND workspace_id = ? LIMIT 1",
    )
    .bind(&request.task_id)
    .bind(&workspace_id)
    .fetch_optional(pool.inner())
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
        WHERE id = ? AND workspace_id = ?
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
    .bind(&workspace_id)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to save workspace task feedback: {}", e))?;

    append_task_feedback_to_manager_session(
        state.inner(),
        pool.inner(),
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
    let Some(session) = find_workspace_session(pool, &descriptor).await? else {
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
    pool: &DbPool,
    state: &AppState,
    workspace_id: &str,
) -> Result<(usize, Option<String>), String> {
    let agents = workspace_agent_summaries(pool, state, workspace_id).await?;
    let default_manager_name = agents
        .iter()
        .find(|agent| agent.is_default)
        .or_else(|| agents.iter().find(|agent| agent.role == "manager"))
        .map(|agent| agent.display_name.clone());

    Ok((agents.len(), default_manager_name))
}

/// Create a new general workspace with a UUID and filesystem root.
#[tauri::command]
pub async fn workspace_create(
    title: Option<String>,
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_millis();
    let display_title = title.unwrap_or_else(|| "New Workspace".to_string());

    // Create filesystem root
    let root = agent_workspace_root_for_id(&id)
        .ok_or_else(|| "Could not resolve workspace root".to_string())?;
    let memory_dir = root.join(".clai").join("memory").join("journal");
    fs::create_dir_all(&memory_dir)
        .map_err(|e| format!("Failed to create workspace directory: {}", e))?;

    // Insert metadata row
    sqlx::query(
        "INSERT INTO workspaces (id, kind, title, created_at, updated_at) VALUES (?, 'general', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&display_title)
    .bind(now)
    .bind(now)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to create workspace: {}", e))?;

    ensure_workspace_manager_assignment(pool.inner(), state.inner(), &id).await?;

    tracing::info!(workspace_id = %id, title = %display_title, "Created new general workspace");

    Ok(id)
}

/// List all workspaces — both agent (from config) and general (from SQLite).
#[tauri::command]
pub async fn workspace_list(
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<Vec<WorkspaceListEntry>, String> {
    let mut entries = Vec::new();

    // 1. Agent workspaces from config — collect sync data first, then do async work
    let agent_infos: Vec<_> = {
        let config = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config
            .get_agents()
            .into_iter()
            .map(|agent| {
                let root = agent_workspace_root_for_id(&agent.id);
                let (artifact_count, memory_count) = root
                    .as_ref()
                    .map(|r| count_workspace_files(r))
                    .unwrap_or((0, 0));
                let created_at = agent
                    .created_at
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .map(|dt| dt.timestamp_millis())
                    .unwrap_or(0);
                let updated_at = agent
                    .updated_at
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .map(|dt| dt.timestamp_millis())
                    .unwrap_or(0);
                (
                    agent.id.clone(),
                    agent.name.clone(),
                    agent.enabled,
                    artifact_count,
                    memory_count,
                    created_at,
                    updated_at,
                )
            })
            .collect()
    }; // config lock dropped here

    for (id, name, enabled, artifact_count, memory_count, created_at, updated_at) in agent_infos {
        let message_count = count_session_messages(pool.inner(), &id).await;
        let task_attention = workspace_task_attention_summary(pool.inner(), &id).await?;
        let (assigned_agent_count, default_manager_name) =
            workspace_team_summary(pool.inner(), state.inner(), &id).await?;
        entries.push(WorkspaceListEntry {
            id: id.clone(),
            kind: "agent".to_string(),
            title: name,
            agent_id: Some(id),
            enabled,
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
            created_at,
            updated_at,
        });
    }

    // 2. General workspaces from SQLite
    let rows: Vec<(String, String, Option<String>, i64, i64)> = sqlx::query_as(
        "SELECT id, kind, title, created_at, updated_at FROM workspaces ORDER BY updated_at DESC",
    )
    .fetch_all(pool.inner())
    .await
    .map_err(|e| format!("Failed to list workspaces: {}", e))?;

    for (id, kind, title, created_at, updated_at) in rows {
        let root = agent_workspace_root_for_id(&id);
        let (artifact_count, memory_count) = root
            .as_ref()
            .map(|r| count_workspace_files(r))
            .unwrap_or((0, 0));
        let message_count = count_session_messages(pool.inner(), &id).await;
        let task_attention = workspace_task_attention_summary(pool.inner(), &id).await?;
        let (assigned_agent_count, default_manager_name) =
            workspace_team_summary(pool.inner(), state.inner(), &id).await?;

        entries.push(WorkspaceListEntry {
            id,
            kind,
            title: title.unwrap_or_else(|| "Untitled".to_string()),
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
            created_at,
            updated_at,
        });
    }

    // Sort all entries by updated_at descending
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));

    Ok(entries)
}

/// Delete a general workspace — removes metadata, session data, and filesystem root.
#[tauri::command]
pub async fn workspace_delete(workspace_id: String, pool: State<'_, DbPool>) -> Result<(), String> {
    // Only allow deleting general workspaces (not agents)
    let exists: bool =
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM workspaces WHERE id = ? LIMIT 1")
            .bind(&workspace_id)
            .fetch_optional(pool.inner())
            .await
            .map_err(|e| format!("Failed to check workspace: {}", e))?
            .is_some();

    if !exists {
        return Err(format!(
            "Workspace {} is not a general workspace or does not exist",
            workspace_id
        ));
    }

    // Delete associated sessions (cascades to messages, runs, tool_calls)
    let sessions = repository::list_sessions(pool.inner(), None)
        .await
        .unwrap_or_default();
    for session in sessions {
        if session.context.workspace_id.as_deref() == Some(&workspace_id)
            || session.context.agent_workspace_id.as_deref() == Some(&workspace_id)
        {
            let _ = repository::delete_session(pool.inner(), &session.id).await;
        }
    }

    // Delete metadata row
    sqlx::query("DELETE FROM workspaces WHERE id = ?")
        .bind(&workspace_id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to delete workspace: {}", e))?;

    sqlx::query("DELETE FROM workspace_agents WHERE workspace_id = ?")
        .bind(&workspace_id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to delete workspace agent assignments: {}", e))?;

    sqlx::query("DELETE FROM workspace_tasks WHERE workspace_id = ?")
        .bind(&workspace_id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to delete workspace tasks: {}", e))?;

    // Delete filesystem root
    if let Some(root) = agent_workspace_root_for_id(&workspace_id) {
        if root.exists() {
            let _ = fs::remove_dir_all(&root);
        }
    }

    tracing::info!(workspace_id = %workspace_id, "Deleted general workspace");

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
    workspace_id: &str,
) -> Result<WorkspaceTaskAttentionSummary, String> {
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN status = 'blocked' AND attention_acknowledged_at IS NULL AND user_response_at IS NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN status = 'failed' AND attention_acknowledged_at IS NULL AND user_response_at IS NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN status = 'needs_user_input' AND attention_acknowledged_at IS NULL AND user_response_at IS NULL THEN 1 ELSE 0 END), 0)
        FROM workspace_tasks
        WHERE workspace_id = ?
        "#,
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to count workspace task attention: {}", e))?;

    #[allow(clippy::type_complexity)]
    let latest: Option<(String, String, String, Option<String>, Option<String>, i64)> =
        sqlx::query_as(
            r#"
            SELECT id, title, status, result_summary, error, updated_at
            FROM workspace_tasks
            WHERE workspace_id = ? AND status IN ('blocked', 'failed', 'needs_user_input')
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
        .bind(workspace_id)
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
    let sessions = repository::list_sessions(pool, None)
        .await
        .unwrap_or_default();

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

//! Workspace state persistence commands.
//!
//! These commands handle saving and loading the workspace state
//! (tabs, commands, layout) to/from SQLite.

use crate::assistant::repository;
use crate::assistant::types::{
    AssistantMessage, AssistantRun, AssistantSession, ContentPart, SessionContext, SessionKind,
    ToolInvocation, WorkspaceAgentSummary,
};
use crate::config::{
    workspace_config, AgentConfig, AppConfig, ExecutionCapabilityConfig, WorkspaceAgent,
    WorkspaceConfig,
};
use crate::db::DbPool;
use crate::AppState;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use tauri::{AppHandle, State};
use ts_rs::TS;

const DEFAULT_WORKSPACE_ID: &str = "default";
const MAX_ENTRY_COUNT: usize = 500;
const MAX_FILE_CONTENT_BYTES: usize = 200_000;

/// Upper bound on a single resource inlined into an HTML preview bundle.
/// Larger assets (e.g. multi-megabyte videos) are left as broken links
/// rather than ballooning the `srcDoc` string and stalling the webview.
const MAX_PREVIEW_ASSET_BYTES: usize = 10_000_000;
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

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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

/// One entry in a single directory level of the artifact tree, returned by
/// `workspace_list_dir`. Unlike `WorkspaceFileEntry` (always a file), this can
/// be either a file or a directory; directories carry a recursive
/// `child_count` so the UI can show "N files" without descending.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceDirEntry {
    pub path: String,
    pub name: String,
    /// "file" or "directory".
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    /// Recursive file count for directories; `None` for files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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
    /// Attached-but-toggled-off MCP servers for the workspace conversation,
    /// sourced from the workspace config (the canonical store). Disjoint from
    /// `selected_mcp_server_ids`, which is the effective enabled set.
    #[serde(default)]
    pub disabled_mcp_server_ids: Vec<String>,
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
    /// True recursive artifact count for the workspace root, independent of
    /// any list cap. The artifacts panel lazy-loads its tree one directory
    /// level at a time via `workspace_list_dir`, so the header counter reads
    /// this instead of `artifacts.len()`.
    #[serde(default)]
    pub artifact_count: i64,
    /// Latest mtime (unix ms) across the artifact tree — files and
    /// non-skipped directories. Changes on any mutation, including
    /// content-only edits and renames that leave the count unchanged;
    /// the artifacts panel keys its tree refresh on this. 0 when empty.
    #[serde(default)]
    pub artifact_latest_modified_at: i64,
    /// Ids of user messages still pending in the queue (written while a
    /// run was active, not yet picked up). The chat renders these with a
    /// "Queued" chip + remove affordance; live updates flow through the
    /// `QueuedMessagesDelivered` / `MessageDeleted` events.
    #[serde(default)]
    pub queued_message_ids: Vec<String>,
    // Agent schedule info (only for agent workspaces)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    // Periodic-workspace surface — read from the workspace's default manager
    // agent so the Workspace page can render the same run / pause / resume
    // controls the Fleet card has. Mirrors WorkspaceListEntry.
    pub schedule_enabled: bool,
    pub schedule_paused: bool,
    /// The workspace's schedule mode (interval vs cron). Empty when the
    /// workspace isn't scheduled. The frontend reads this to render the
    /// "every Nm" / "Cron: …" label and to populate the workspace
    /// settings modal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_kind: Option<crate::config::workspace_config::ScheduleKind>,
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

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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
    #[ts(type = "unknown")]
    pub execution: ExecutionCapabilityConfig,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
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

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceSessionBinding {
    pub session: AssistantSession,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_connection_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceFileContent {
    pub path: String,
    pub viewer: String,
    pub content: String,
}

/// A workspace file returned as base64-encoded bytes plus a best-effort
/// MIME type. Used by the HTML-artifact preview bundler to inline local
/// resources (stylesheets, scripts, images, fonts) so a multi-file report
/// renders correctly inside the unique-origin preview iframe.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceFileBytes {
    pub path: String,
    pub mime: String,
    pub base64: String,
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
pub struct WorkspaceForkRequest {
    pub workspace_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceDescriptor {
    workspace_id: String,
    kind: String,
    title: String,
    agent_id: Option<String>,
    pub(crate) root_path: Option<PathBuf>,
    provider_connection_ids: Vec<String>,
    selected_mcp_server_ids: Vec<String>,
    selected_mcp_server_names: Vec<String>,
    disabled_mcp_server_ids: Vec<String>,
    execution: ExecutionCapabilityConfig,
    tool_scopes: Vec<String>,
    automation_name: Option<String>,
}

#[derive(Debug, Clone)]
struct WorkspaceProviderSelection {
    ids: Vec<String>,
    names: Vec<String>,
    preferred_connection_id: Option<String>,
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

/// Atomic read-modify-write of a workspace config + index refresh.
///
/// Use this (not `load_workspace_config_for_id` + `save_workspace_config_
/// for_root`) whenever the write depends on the loaded state — bare
/// load→save pairs race the agent runner's run-completion persist (and
/// each other) as lost updates. See `workspace_config::update`.
fn update_workspace_config_for_id<R>(
    state: &AppState,
    workspace_id: &str,
    mutate: impl FnOnce(&mut WorkspaceConfig) -> Result<R, String>,
) -> Result<(R, WorkspaceConfig), String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let (value, config) = workspace_config::update(&root, mutate)?;
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .insert_config(root, &config);
    Ok((value, config))
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

    if ext == "html" || ext == "htm" {
        return "html".to_string();
    }

    if ext == "json" {
        return "json".to_string();
    }

    // Binary / rich formats the in-app preview can't render as UTF-8 text.
    // The frontend treats "external" by launching the OS default app for the
    // file (see `open_workspace_path` target "system") instead of opening the
    // text preview panel, which would otherwise fail with a UTF-8 error.
    if is_external_viewer_ext(&ext) {
        return "external".to_string();
    }

    "text".to_string()
}

/// Extensions whose files are opened with the OS default application rather
/// than the in-app text preview. Kept deliberately broad across documents,
/// archives, media, and binaries — anything a dev workspace might hold that
/// is not UTF-8 text.
fn is_external_viewer_ext(ext: &str) -> bool {
    matches!(
        ext,
        // Documents
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods"
            | "odp" | "rtf" | "epub"
        // Images (no inline image preview yet — open externally)
            | "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tiff" | "tif" | "avif"
        // Audio / video
            | "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac"
            | "mp4" | "mov" | "mkv" | "webm" | "avi" | "wmv" | "m4v"
        // Archives
            | "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst"
        // Binaries / other
            | "exe" | "dll" | "so" | "dylib" | "bin" | "o" | "a"
            | "wasm" | "class" | "jar"
            | "sqlite" | "db" | "parquet"
            | "woff" | "woff2" | "ttf" | "otf" | "eot"
    )
}

/// Best-effort MIME type from a file extension, used to build `data:` URIs
/// when inlining preview resources. Falls back to `application/octet-stream`
/// for unknown extensions — browsers still load most binary assets from a
/// generic data URI, and unknown text resources are read as UTF-8 elsewhere.
fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn sort_workspace_entries(entries: &mut [WorkspaceFileEntry]) {
    entries.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.path.cmp(&right.path))
    });
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

/// Recursive stats for the artifact tree, applying the same skip-list as
/// `workspace_list_dir` / the artifact `collect_files` walk so the header
/// counter matches what the panel can actually surface (excludes `.clai`,
/// `node_modules`, `target`, `.git`, …).
///
/// Returns `(file_count, latest_modified_at_ms)`. The mtime max includes
/// directories, not just files: an in-place edit bumps the file's mtime,
/// while a rename/move bumps the parent directory's — together they make
/// the value change on any mutation, which is what the artifacts panel
/// keys its refresh on (the count alone misses content-only changes).
/// Recursive file count only — used for per-folder `childCount` in
/// `workspace_list_dir`, where the mtime half of the stats isn't needed.
fn count_artifact_files(dir: &Path) -> i64 {
    artifact_tree_stats(dir).0
}

fn artifact_tree_stats(dir: &Path) -> (i64, i64) {
    let mut count: i64 = 0;
    let mut latest_modified_at: i64 = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = path.is_dir();
            // Skipped trees contribute nothing — not even the dir's own
            // mtime, or internal churn in e.g. `.git` would read as an
            // artifact change.
            if is_dir && should_skip_artifact_dir(&path) {
                continue;
            }
            if let Some(mtime) = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| {
                    modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|duration| duration.as_millis() as i64)
                })
            {
                latest_modified_at = latest_modified_at.max(mtime);
            }
            if is_dir {
                let (child_count, child_latest) = artifact_tree_stats(&path);
                count += child_count;
                latest_modified_at = latest_modified_at.max(child_latest);
            } else if path.is_file() {
                count += 1;
            }
        }
    }
    (count, latest_modified_at)
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

fn should_skip_fork_copy_path(relative_path: &Path) -> bool {
    let mut components = relative_path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return false;
    };
    if first != ".clai" {
        return false;
    }

    let Some(Component::Normal(second)) = components.next() else {
        return false;
    };
    if components.next().is_some() {
        return false;
    }

    let name = second.to_string_lossy();
    name == "config.json"
        || name == "config.json.tmp"
        || name.starts_with("data.sqlite")
        || name == "images"
}

fn copy_workspace_durable_files(source_root: &Path, target_root: &Path) -> Result<(), String> {
    fn copy_dir(source_root: &Path, current: &Path, target_root: &Path) -> Result<(), String> {
        let entries = fs::read_dir(current)
            .map_err(|error| format!("Failed to read {}: {}", current.display(), error))?;

        for entry in entries {
            let entry =
                entry.map_err(|error| format!("Failed to inspect directory entry: {}", error))?;
            let source_path = entry.path();
            let relative_path = source_path
                .strip_prefix(source_root)
                .map_err(|error| format!("Failed to resolve fork path: {}", error))?;

            if should_skip_fork_copy_path(relative_path) {
                continue;
            }

            let metadata = fs::symlink_metadata(&source_path).map_err(|error| {
                format!("Failed to inspect {}: {}", source_path.display(), error)
            })?;
            if metadata.file_type().is_symlink() {
                tracing::debug!(
                    path = %source_path.display(),
                    "Skipping symlink while forking workspace"
                );
                continue;
            }

            let target_path = target_root.join(relative_path);
            if metadata.is_dir() {
                fs::create_dir_all(&target_path).map_err(|error| {
                    format!("Failed to create {}: {}", target_path.display(), error)
                })?;
                copy_dir(source_root, &source_path, target_root)?;
            } else if metadata.is_file() {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        format!("Failed to create {}: {}", parent.display(), error)
                    })?;
                }
                fs::copy(&source_path, &target_path).map_err(|error| {
                    format!(
                        "Failed to copy {} to {}: {}",
                        source_path.display(),
                        target_path.display(),
                        error
                    )
                })?;
            }
        }

        Ok(())
    }

    copy_dir(source_root, source_root, target_root)
}

fn write_fork_marker(
    target_root: &Path,
    source_id: &str,
    source_title: &str,
    new_id: &str,
    prompt: Option<&str>,
) -> Result<(), String> {
    let memory_dir = target_root.join(".clai").join("memory");
    fs::create_dir_all(&memory_dir)
        .map_err(|error| format!("Failed to create fork memory directory: {}", error))?;
    let mut content = format!(
        "# Fork Context\n\n\
         This workspace was forked from `{}` (`{}`) at {}.\n\n\
         The new workspace id is `{}`. Copied memories and artifacts are historical context; \
         if they mention the source workspace id or path, translate them to this workspace only when still applicable.\n",
        source_title,
        source_id,
        chrono::Utc::now().to_rfc3339(),
        new_id
    );
    if let Some(prompt) = prompt.map(str::trim).filter(|value| !value.is_empty()) {
        content.push_str("\n## Fork Prompt\n\n");
        content.push_str(prompt);
        content.push('\n');
    }

    fs::write(memory_dir.join("fork.md"), content)
        .map_err(|error| format!("Failed to write fork marker: {}", error))
}

pub(crate) fn resolve_workspace_descriptor(
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
    // Enabled wins on overlap: Workspace Settings rewrites the manager's
    // `selected_mcp_servers` without touching the workspace-level disabled
    // list, so a server re-selected there must not surface as disabled here.
    let disabled_mcp_server_ids: Vec<String> =
        workspace_config::refs_to_mcp_ids(&app_config, &config.disabled_mcp_servers)
            .into_iter()
            .filter(|id| !selected_mcp_server_ids.contains(id))
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
        disabled_mcp_server_ids,
        execution,
        tool_scopes: vec!["fs".to_string(), "web".to_string()],
        automation_name: manager.map(|agent| agent.name.clone()),
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

    // Surface the preferred (actually-used) connection first so the terminal
    // provider picker, which seeds its selection from the first id, reflects
    // what interactive sends and scheduled runs actually use.
    let mut ordered = enabled_connections;
    if let Some(pref) = preferred_connection_id.as_ref() {
        if let Some(pos) = ordered.iter().position(|c| &c.id == pref) {
            let connection = ordered.remove(pos);
            ordered.insert(0, connection);
        }
    }

    Ok(WorkspaceProviderSelection {
        ids: ordered
            .iter()
            .map(|connection| connection.id.clone())
            .collect(),
        names: ordered
            .iter()
            .map(|connection| connection.name.clone())
            .collect(),
        preferred_connection_id,
    })
}

/// Live-compute the system-prompt seed for a workspace agent, reading the
/// current workspace config + the user's enabled skills. Returns `None` when
/// the workspace or agent has been deleted (the session is half-broken; the
/// engine treats it as "no agent instructions" rather than panicking).
///
/// This is the canonical lookup used at prompt-build time. Skills are no
/// longer snapshotted onto `SessionContext.automation_description`; we read
/// fresh on every turn so toggling a skill in the workspace settings
/// modal is immediately visible to the model on the next message.
pub fn workspace_agent_runtime_description(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
) -> Option<String> {
    let root = state.workspace_root(workspace_id)?;
    let workspace_cfg = workspace_config::load(&root).ok()?;
    let agent = workspace_cfg.agents.iter().find(|a| a.id == agent_id)?;
    let app_cfg = state.config_manager.lock().ok()?.get();
    let selected_skill_ids = workspace_config::refs_to_skill_ids(&app_cfg, &agent.selected_skills);
    Some(crate::config::compose_agent_instructions(
        &app_cfg,
        &agent.description,
        &selected_skill_ids,
    ))
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
    update_workspace_config_for_id(state, workspace_id, |config| {
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
        Ok(())
    })
    .map(|_| ())
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
        schedule_kind: schedule.map(|s| s.kind.clone()).unwrap_or_default(),
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

/// Find the canonical session for a workspace's manager agent.
///
/// There is exactly one such session per (workspace, manager) — both
/// user-typed chats and scheduled ticks read/write the same row. Selection
/// is now kind-agnostic: the resolver excludes task-linked sessions via an
/// SQL anti-join on `workspace_tasks.session_id` (see
/// `list_non_task_sessions` in `assistant::repository`), so neither the
/// interactive chat nor a non-task `BackgroundJob` session is missed and a
/// task's own `BackgroundJob` row never hijacks the conversation view.
///
/// The actual selection lives in [`select_workspace_session`] so it can be
/// unit-tested without a database; see the regression tests there for why
/// excluding task sessions — not filtering by `kind` — is load-bearing.
async fn find_workspace_session(
    pool: &DbPool,
    state: &AppState,
    descriptor: &WorkspaceDescriptor,
) -> Result<Option<AssistantSession>, String> {
    let Some(manager_id) =
        workspace_default_agent_id(state, &descriptor.workspace_id).unwrap_or(None)
    else {
        return Ok(None);
    };

    // Candidate conversations = all of this manager's sessions EXCEPT task
    // delegations. Task sessions (workspace_tasks.session_id) are excluded in
    // SQL, so resolution stays independent of task volume and a self-assigned
    // task can't hijack the view. The canonical conversation is then the
    // most-recently-updated remaining session: the interactive chat in the
    // common case, or a non-task BackgroundJob session for workspaces whose
    // scheduled-run conversation was forked into its own row.
    Ok(select_workspace_session(
        repository::list_non_task_sessions(pool).await?,
        &manager_id,
        &descriptor.workspace_id,
        descriptor.agent_id.as_deref(),
    ))
}

/// Pure selection logic behind [`find_workspace_session`], split out so it
/// is unit-testable without a database or `AppState`.
///
/// Picks the single canonical conversation for `(workspace, manager)` as the
/// most-recently-updated session that:
/// - belongs to this workspace's manager (`automation_id == manager`), and
/// - belongs to this workspace (by workspace / agent id).
///
/// Callers pass only NON-task sessions (see
/// [`repository::list_non_task_sessions`]). Sub-agent task delegations run in
/// separate `BackgroundJob` sessions recorded in `workspace_tasks`, and a
/// self-assigned task even carries the manager's own `automation_id`, so they
/// are excluded *upstream* rather than by kind here. Selecting by recency (not
/// by kind) is deliberate: a workspace whose scheduled-run conversation lives
/// in its own non-task `BackgroundJob` session still resolves to that live
/// conversation instead of an abandoned interactive stub.
fn select_workspace_session(
    sessions: Vec<AssistantSession>,
    manager_id: &str,
    workspace_id: &str,
    agent_id: Option<&str>,
) -> Option<AssistantSession> {
    let belongs_to_workspace = |session: &AssistantSession| -> bool {
        session.context.workspace_id.as_deref() == Some(workspace_id)
            || agent_id
                .map(|aid| {
                    session.context.agent_workspace_id.as_deref() == Some(aid)
                        || session.context.automation_id.as_deref() == Some(aid)
                })
                .unwrap_or(false)
    };

    sessions
        .into_iter()
        .filter(|session| session.context.automation_id.as_deref() == Some(manager_id))
        .filter(belongs_to_workspace)
        .max_by_key(|session| session.updated_at)
}

fn desired_workspace_context(
    descriptor: &WorkspaceDescriptor,
    existing_session: Option<&AssistantSession>,
    workspace_agents: Vec<WorkspaceAgentSummary>,
    workspace_manager: Option<&AgentConfig>,
) -> SessionContext {
    // The workspace config is the canonical store for a general workspace's
    // MCP selection: the manager row's `selected_mcp_servers` (surfaced on the
    // descriptor) holds the effective enabled set, and the workspace-level
    // `disabled_mcp_servers` remembers attached-but-toggled-off servers.
    // Sessions only ever receive the derived enabled list. Fall back to the
    // session's list solely for legacy sessions that predate the config
    // mirror, where the config has no selection recorded.
    let mcp_server_ids = if descriptor.agent_id.is_some() {
        // Agent workspaces: MCP comes from agent config (descriptor)
        descriptor.selected_mcp_server_ids.clone()
    } else if workspace_manager.is_none() {
        // No manager row: the config cannot record an enabled selection
        // (only the manager's `selected_mcp_servers` holds it), so the
        // session's list stays canonical — minus anything the config marks
        // disabled, so the context-bar toggle still takes effect.
        existing_session
            .map(|session| {
                session
                    .context
                    .mcp_server_ids
                    .iter()
                    .filter(|id| !descriptor.disabled_mcp_server_ids.contains(*id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    } else if descriptor.selected_mcp_server_ids.is_empty()
        && descriptor.disabled_mcp_server_ids.is_empty()
    {
        // A manager row exists but records nothing: fall back to the
        // session's list for legacy sessions that predate the config mirror.
        // Only an entirely empty config counts as "no selection recorded" —
        // an empty enabled list next to a non-empty disabled list means the
        // user toggled every attached server off, and the session must not
        // resurrect its stale enabled set.
        existing_session
            .map(|session| session.context.mcp_server_ids.clone())
            .unwrap_or_default()
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

    SessionContext {
        space_id: existing_session.and_then(|session| session.context.space_id.clone()),
        room_id: existing_session.and_then(|session| session.context.room_id.clone()),
        workspace_id: Some(descriptor.workspace_id.clone()),
        tool_scopes: descriptor.tool_scopes.clone(),
        mcp_server_ids,
        execution,
        cli_session_id: existing_session.and_then(|session| session.context.cli_session_id.clone()),
        cli_session_provider: existing_session
            .and_then(|session| session.context.cli_session_provider.clone()),
        automation_id,
        agent_workspace_id,
        automation_name,
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
    // Cheap single-table query, so it rides along even on lightweight polls —
    // the "Queued" chips stay accurate without the full session payload.
    let queued_message_ids = if let Some(session) = &session {
        repository::list_pending_queued_message_ids(&workspace_pool, &session.id).await?
    } else {
        Vec::new()
    };

    // Memories are still returned in full (their panel renders the flat list);
    // artifacts are no longer walked here — the panel lazy-loads each directory
    // level via `workspace_list_dir`, so we only need the true recursive count
    // for the header counter. This also keeps the periodic 5s poll bounded:
    // counting is cheaper than building + serializing every entry, and there is
    // no longer a 500-entry cap silently truncating large workspaces.
    let (memories, artifact_count, artifact_latest_modified_at) = if options.include_files() {
        if let Some(root_path) = &descriptor.root_path {
            let memory_root = root_path.join(".clai").join("memory");
            let mut memories = Vec::new();
            if memory_root.exists() {
                collect_files(&memory_root, root_path, &mut memories, false)?;
            }
            sort_workspace_entries(&mut memories);

            let (artifact_count, artifact_latest_modified_at) = artifact_tree_stats(root_path);
            (memories, artifact_count, artifact_latest_modified_at)
        } else {
            (Vec::new(), 0, 0)
        }
    } else {
        (Vec::new(), 0, 0)
    };
    let artifacts: Vec<WorkspaceFileEntry> = Vec::new();

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
    let (schedule_enabled, schedule_paused, schedule_kind) =
        match workspace_config_for_schedule.as_ref() {
            Some(cfg) if cfg.schedule.enabled => {
                (true, cfg.schedule.paused, Some(cfg.schedule.kind.clone()))
            }
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
        disabled_mcp_server_ids: descriptor.disabled_mcp_server_ids,
        session,
        messages,
        runs,
        tool_calls,
        memories,
        artifacts,
        artifact_count,
        artifact_latest_modified_at,
        queued_message_ids,
        enabled,
        schedule_enabled,
        schedule_paused,
        schedule_kind,
        next_run_in_seconds,
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListDirRequest {
    pub workspace_id: String,
    /// Directory to list, relative to the workspace root. `None`/empty = root.
    #[serde(default)]
    pub path: Option<String>,
}

/// List a single directory level of the artifact tree. The artifacts panel
/// calls this lazily — once for the root, then again per folder as the user
/// expands it — so the whole tree never has to be walked or held in memory at
/// once. Applies the same skip-list as the rest of the artifact surface.
#[tauri::command]
pub async fn workspace_list_dir(
    request: WorkspaceListDirRequest,
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceDirEntry>, String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;
    ensure_agent_workspace_root(root_path)?;

    let rel = request.path.unwrap_or_default();
    let target = normalize_path(root_path.join(&rel));
    if !target.starts_with(root_path) {
        return Err(format!(
            "Path {} is outside the workspace root",
            target.display()
        ));
    }
    if !target.is_dir() {
        return Err(format!("Not a directory: {}", rel));
    }

    let read_dir = fs::read_dir(&target)
        .map_err(|error| format!("Failed to read {}: {}", target.display(), error))?;
    let mut entries: Vec<WorkspaceDirEntry> = Vec::new();
    for entry in read_dir {
        let entry =
            entry.map_err(|error| format!("Failed to inspect directory entry: {}", error))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Failed to inspect {}: {}", path.display(), error))?;
        let relative_path = match path.strip_prefix(root_path) {
            Ok(value) => value.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();

        if file_type.is_dir() {
            if should_skip_artifact_dir(&path) {
                continue;
            }
            entries.push(WorkspaceDirEntry {
                path: relative_path,
                name,
                kind: "directory".to_string(),
                viewer: None,
                size: None,
                updated_at: None,
                child_count: Some(count_artifact_files(&path)),
            });
        } else if file_type.is_file() {
            let metadata = fs::metadata(&path).ok();
            entries.push(WorkspaceDirEntry {
                path: relative_path,
                name,
                kind: "file".to_string(),
                viewer: Some(viewer_for_path(&path)),
                size: metadata.as_ref().map(|value| value.len()),
                updated_at: metadata.as_ref().and_then(file_updated_at),
                child_count: None,
            });
        }
    }

    // Folders first, then files; each group sorted by name (case-insensitive).
    entries.sort_by(|a, b| {
        let a_dir = a.kind == "directory";
        let b_dir = b.kind == "directory";
        if a_dir != b_dir {
            return if a_dir {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });

    Ok(entries)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSearchArtifactsRequest {
    pub workspace_id: String,
    pub query: String,
}

/// Upper bound on results returned by `workspace_search_artifacts`. The walk
/// stops early once this many matches are collected; the command logs a
/// warning so a silently-truncated result set is visible in the logs.
const ARTIFACT_SEARCH_CAP: usize = 1000;

fn search_artifact_files(
    current: &Path,
    root: &Path,
    needle: &str,
    matches: &mut Vec<WorkspaceFileEntry>,
) {
    if matches.len() >= ARTIFACT_SEARCH_CAP {
        return;
    }
    let Ok(read_dir) = fs::read_dir(current) else {
        return;
    };
    for entry in read_dir.flatten() {
        if matches.len() >= ARTIFACT_SEARCH_CAP {
            break;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if should_skip_artifact_dir(&path) {
                continue;
            }
            search_artifact_files(&path, root, needle, matches);
        } else if file_type.is_file() {
            let matches_name = path
                .strip_prefix(root)
                .ok()
                .map(|relative| relative.to_string_lossy().to_lowercase().contains(needle))
                .unwrap_or(false);
            if matches_name {
                if let Some(file) = build_file_entry(root, &path) {
                    matches.push(file);
                }
            }
        }
    }
}

/// Walk the whole artifact tree server-side and return file entries whose
/// relative path matches `query` (case-insensitive substring). Backs the
/// artifacts panel search box: since the panel only lazy-loads the directory
/// levels the user has opened, client-side filtering could only ever see those
/// — this command searches everything regardless of what's expanded.
#[tauri::command]
pub async fn workspace_search_artifacts(
    request: WorkspaceSearchArtifactsRequest,
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceFileEntry>, String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;
    ensure_agent_workspace_root(root_path)?;

    let needle = request.query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }

    let mut matches = Vec::new();
    search_artifact_files(root_path, root_path, &needle, &mut matches);
    if matches.len() >= ARTIFACT_SEARCH_CAP {
        tracing::warn!(
            query = %request.query,
            cap = ARTIFACT_SEARCH_CAP,
            "workspace_search_artifacts results truncated"
        );
    }
    sort_workspace_entries(&mut matches);
    Ok(matches)
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
        // One Interactive session per (workspace, manager). Both
        // user-typed chats and scheduled ticks land here — there is no
        // separate BackgroundJob row for the manager. Sub-agent task
        // delegations still get their own BackgroundJob rows via
        // `tools::workspace_tasks` (they're owned by the assignee, not
        // the manager).
        repository::create_session(
            &workspace_pool,
            repository::CreateSessionParams {
                kind: SessionKind::Interactive,
                title: Some(descriptor.title.clone()),
                context: desired_workspace_context(
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
        Err(_) => Err(format!("File not found: {}", request.path)),
    }
}

/// Read an arbitrary workspace file as base64-encoded bytes plus a
/// best-effort MIME type.
///
/// Backs the HTML-artifact preview bundler: when a report references local
/// siblings (`<link href="assets/x.css">`, `<img src>`, fonts, …) the
/// frontend resolves each path relative to the HTML file and pulls the bytes
/// through here to inline them. Resolution is constrained to the workspace
/// root by `resolve_workspace_file_path`, so this cannot read outside it, and
/// oversized assets are rejected to keep the preview bundle bounded.
#[tauri::command]
pub async fn workspace_read_file_base64(
    request: WorkspaceReadFileRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceFileBytes, String> {
    use base64::Engine as _;

    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;

    ensure_agent_workspace_root(root_path)?;
    let resolved = resolve_workspace_file_path(root_path, &request.path)?;
    let bytes = fs::read(&resolved)
        .map_err(|error| format!("Failed to read {}: {}", resolved.display(), error))?;
    if bytes.len() > MAX_PREVIEW_ASSET_BYTES {
        return Err(format!(
            "{} is too large to inline ({} bytes)",
            resolved.display(),
            bytes.len()
        ));
    }

    Ok(WorkspaceFileBytes {
        path: request.path,
        mime: mime_for_path(&resolved).to_string(),
        base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
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
        Err(_) => Err(format!("File not found: {}", request.path)),
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
pub struct WorkspaceDeletePathRequest {
    pub workspace_id: String,
    /// Path to delete, relative to the workspace root.
    pub path: String,
}

/// True when the path's first component is a protected/skipped directory
/// (`.clai`, `.git`, `target`, …). These never surface as artifact rows, but
/// the delete command enforces it regardless so a crafted request can't wipe
/// the DB, git, or build state.
fn is_protected_artifact_path(relative_path: &Path) -> bool {
    match relative_path.components().next() {
        Some(Component::Normal(first)) => first
            .to_str()
            .map(|name| SKIPPED_ARTIFACT_DIRS.contains(&name))
            .unwrap_or(false),
        _ => true, // empty, `..`, or non-normal leading component: refuse
    }
}

/// Delete a single artifact file or folder (recursive) from the workspace.
///
/// Guards: the resolved path must stay inside the workspace root, must not be
/// the root itself, and must not target a protected directory
/// (`SKIPPED_ARTIFACT_DIRS`). Folders are removed recursively.
#[tauri::command]
pub async fn workspace_delete_path(
    request: WorkspaceDeletePathRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;
    ensure_agent_workspace_root(root_path)?;

    let rel = request.path.trim();
    if rel.is_empty() {
        return Err("Cannot delete the workspace root.".to_string());
    }
    let relative = Path::new(rel);
    if is_protected_artifact_path(relative) {
        return Err(format!("Refusing to delete a protected path: {}", rel));
    }

    let target = normalize_path(root_path.join(relative));
    if !target.starts_with(root_path) {
        return Err(format!("Path {} is outside the workspace root", rel));
    }
    if target == *root_path {
        return Err("Cannot delete the workspace root.".to_string());
    }

    let metadata = fs::symlink_metadata(&target)
        .map_err(|error| format!("Path not found: {}: {}", rel, error))?;
    if metadata.is_dir() {
        fs::remove_dir_all(&target)
            .map_err(|error| format!("Failed to delete {}: {}", rel, error))?;
    } else {
        fs::remove_file(&target).map_err(|error| format!("Failed to delete {}: {}", rel, error))?;
    }

    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCopyPathRequest {
    pub source_workspace_id: String,
    /// Path to copy, relative to the SOURCE workspace root.
    pub path: String,
    pub dest_workspace_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMovePathRequest {
    pub workspace_id: String,
    /// Path to move, relative to the workspace root.
    pub path: String,
    /// Destination directory, relative to the same workspace root.
    pub dest_dir_path: String,
}

/// Copy a file or folder from one workspace into another (drag-and-drop from
/// the artifacts drawer). Always a copy — never a move, so the source can't be
/// destroyed by a drag. The item lands at the destination workspace ROOT under
/// a collision-free name; folders are copied recursively. Returns the created
/// name (relative to the destination root).
#[tauri::command]
pub async fn workspace_copy_path(
    request: WorkspaceCopyPathRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let source_desc =
        resolve_workspace_descriptor(state.inner(), Some(request.source_workspace_id.clone()))?;
    let source_root = source_desc
        .root_path
        .as_ref()
        .ok_or_else(|| "The source workspace does not expose a filesystem root".to_string())?;
    ensure_agent_workspace_root(source_root)?;

    let dest_desc =
        resolve_workspace_descriptor(state.inner(), Some(request.dest_workspace_id.clone()))?;
    let dest_root = dest_desc
        .root_path
        .as_ref()
        .ok_or_else(|| "The destination workspace does not expose a filesystem root".to_string())?;
    ensure_agent_workspace_root(dest_root)?;

    let (source, is_dir, name) = resolve_copy_source(source_root, &request.path)?;

    // The traversal + copy is unbounded (a folder can be arbitrarily large),
    // so run it off the async executor thread.
    let dest_root = dest_root.clone();
    let name_for_copy = name.clone();
    let created = tokio::task::spawn_blocking(move || {
        copy_artifact_to_unique_destination(&source, &dest_root, &name_for_copy, is_dir)
    })
    .await
    .map_err(|error| format!("Copy task did not complete: {}", error))??;

    Ok(created
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&name)
        .to_string())
}

/// Move a file or folder into another folder within the same workspace. The
/// source lands under a collision-free name and the returned path is relative
/// to the workspace root.
#[tauri::command]
pub async fn workspace_move_path(
    request: WorkspaceMovePathRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;
    ensure_agent_workspace_root(root_path)?;

    move_artifact_within_workspace_root(root_path, &request.path, &request.dest_dir_path).await
}

async fn move_artifact_within_workspace_root(
    root_path: &Path,
    path: &str,
    dest_dir_path: &str,
) -> Result<String, String> {
    let (source, is_dir, name) = resolve_copy_source(root_path, path)?;
    let dest_dir = resolve_artifact_destination_dir(root_path, dest_dir_path)?;
    ensure_move_destination_allowed(&source, &dest_dir, is_dir)?;

    let canon_root = root_path
        .canonicalize()
        .map_err(|error| format!("Failed to resolve the workspace root: {}", error))?;

    let source_for_move = source.clone();
    let dest_dir_for_move = dest_dir.clone();
    let name_for_move = name.clone();
    let moved = tokio::task::spawn_blocking(move || {
        move_artifact_to_unique_destination(&source_for_move, &dest_dir_for_move, &name_for_move)
    })
    .await
    .map_err(|error| format!("Move task did not complete: {}", error))??;

    workspace_relative_path(&canon_root, &moved)
}

/// Validate + resolve a copy *source* path relative to `source_root`.
///
/// Rejects: the root itself, traversal/non-normal components, a protected dir
/// (`SKIPPED_ARTIFACT_DIRS`) at ANY depth (not just the first component), and a
/// symlink item. Resolves intermediate symlinks via `canonicalize` and
/// requires the canonical path to stay within the canonical root, so a
/// symlinked parent component can't escape the workspace. Returns
/// `(canonical source, is_dir, display name)`.
fn resolve_copy_source(source_root: &Path, rel: &str) -> Result<(PathBuf, bool, String), String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err("Cannot copy the workspace root.".to_string());
    }
    let relative = Path::new(rel);
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                if name
                    .to_str()
                    .map(|n| SKIPPED_ARTIFACT_DIRS.contains(&n))
                    .unwrap_or(false)
                {
                    return Err(format!("Refusing to copy a protected path: {}", rel));
                }
            }
            Component::CurDir => {}
            // `..`, absolute prefixes, root: refuse before touching the fs.
            _ => return Err(format!("Path {} is outside the workspace root", rel)),
        }
    }

    let requested = normalize_path(source_root.join(relative));
    if !requested.starts_with(source_root) {
        return Err(format!("Path {} is outside the workspace root", rel));
    }
    if requested == *source_root {
        return Err("Cannot copy the workspace root.".to_string());
    }

    let link_meta = fs::symlink_metadata(&requested)
        .map_err(|error| format!("Path not found: {}: {}", rel, error))?;
    if link_meta.file_type().is_symlink() {
        return Err(format!("Refusing to copy a symlink: {}", rel));
    }

    // Resolve intermediate symlinks and re-check containment.
    let canon_root = source_root
        .canonicalize()
        .map_err(|error| format!("Failed to resolve the source workspace root: {}", error))?;
    let canon_source = requested
        .canonicalize()
        .map_err(|error| format!("Failed to resolve {}: {}", rel, error))?;
    if !canon_source.starts_with(&canon_root) {
        return Err(format!("Path {} resolves outside the workspace root", rel));
    }

    let name = requested
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("`{}` has no file name.", rel))?
        .to_string();

    Ok((canon_source, link_meta.file_type().is_dir(), name))
}

/// Validate + resolve a move destination directory relative to `workspace_root`.
fn resolve_artifact_destination_dir(workspace_root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    let relative = Path::new(rel);
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                if name
                    .to_str()
                    .map(|n| SKIPPED_ARTIFACT_DIRS.contains(&n))
                    .unwrap_or(false)
                {
                    return Err(format!("Refusing to move into a protected path: {}", rel));
                }
            }
            Component::CurDir => {}
            _ => return Err(format!("Path {} is outside the workspace root", rel)),
        }
    }

    let requested = normalize_path(workspace_root.join(relative));
    if !requested.starts_with(workspace_root) {
        return Err(format!("Path {} is outside the workspace root", rel));
    }

    let link_meta = fs::symlink_metadata(&requested)
        .map_err(|error| format!("Destination folder not found: {}: {}", rel, error))?;
    if link_meta.file_type().is_symlink() {
        return Err(format!("Refusing to move into a symlink: {}", rel));
    }
    if !link_meta.file_type().is_dir() {
        return Err(format!("Destination is not a folder: {}", rel));
    }

    let canon_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("Failed to resolve the workspace root: {}", error))?;
    let canon_dest = requested
        .canonicalize()
        .map_err(|error| format!("Failed to resolve destination {}: {}", rel, error))?;
    if !canon_dest.starts_with(&canon_root) {
        return Err(format!(
            "Destination {} resolves outside the workspace root",
            rel
        ));
    }

    Ok(canon_dest)
}

fn ensure_move_destination_allowed(
    source: &Path,
    dest_dir: &Path,
    is_dir: bool,
) -> Result<(), String> {
    if is_dir && dest_dir.starts_with(source) {
        return Err("Cannot move a folder into itself or one of its subfolders.".to_string());
    }
    Ok(())
}

/// Copy `source` (file or dir) into `dest_dir` under a collision-free name
/// (`foo` → `foo (1)` → …). Files reuse the exclusive-create import helper;
/// dirs are created exclusively then filled by [`copy_dir_recursive`].
fn copy_artifact_to_unique_destination(
    source: &Path,
    dest_dir: &Path,
    name: &str,
    is_dir: bool,
) -> Result<std::path::PathBuf, String> {
    if !is_dir {
        return crate::commands::system_apps::copy_to_unique_destination(source, dest_dir, name);
    }
    for n in 0u32.. {
        let candidate = crate::commands::system_apps::destination_candidate(dest_dir, name, n);
        match fs::create_dir(&candidate) {
            Ok(()) => {
                if let Err(error) = copy_dir_recursive(source, &candidate) {
                    let _ = fs::remove_dir_all(&candidate);
                    return Err(error);
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create `{}`: {}",
                    candidate.display(),
                    error
                ));
            }
        }
    }
    unreachable!("u32 exhausted finding a unique directory name")
}

fn move_artifact_to_unique_destination(
    source: &Path,
    dest_dir: &Path,
    name: &str,
) -> Result<PathBuf, String> {
    if source
        .parent()
        .map(|parent| parent == dest_dir)
        .unwrap_or(false)
    {
        return Ok(source.to_path_buf());
    }

    for n in 0u32.. {
        let candidate = crate::commands::system_apps::destination_candidate(dest_dir, name, n);
        if candidate == source {
            return Ok(source.to_path_buf());
        }

        match rename_no_replace(source, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to move `{}` to `{}`: {}",
                    source.display(),
                    candidate.display(),
                    error
                ));
            }
        }
    }
    unreachable!("u32 exhausted finding a unique move name")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_to_cstring(path: &Path) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Path contains an interior NUL byte: {}", path.display()),
        )
    })
}

#[cfg(target_os = "linux")]
fn rename_no_replace(source: &Path, dest: &Path) -> std::io::Result<()> {
    let source = path_to_cstring(source)?;
    let dest = path_to_cstring(dest)?;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            dest.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_no_replace(source: &Path, dest: &Path) -> std::io::Result<()> {
    let source = path_to_cstring(source)?;
    let dest = path_to_cstring(dest)?;
    let rc = unsafe { libc::renamex_np(source.as_ptr(), dest.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_no_replace(source: &Path, dest: &Path) -> std::io::Result<()> {
    let source = path_to_windows_wide(source)?;
    let dest = path_to_windows_wide(dest)?;
    let rc = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(source.as_ptr(), dest.as_ptr(), 0)
    };
    if rc != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn path_to_windows_wide(path: &Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Path contains an interior NUL byte: {}", path.display()),
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rename_no_replace(_source: &Path, _dest: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        "atomic no-overwrite rename is not supported on this platform",
    ))
}

fn workspace_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| format!("Path {} is outside the workspace root", path.display()))?;
    let rel = rel
        .to_str()
        .ok_or_else(|| format!("Path {} is not valid UTF-8", path.display()))?;
    Ok(rel.replace(std::path::MAIN_SEPARATOR, "/"))
}

/// Recursively copy `src` into the (already-created) `dst`. Symlinks are
/// skipped (avoids loops and escaping the workspace tree), and protected/heavy
/// dirs (`.git`, `node_modules`, `target`, …) are skipped so a folder copy
/// doesn't drag in VCS/build state.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in
        fs::read_dir(src).map_err(|e| format!("Failed to read `{}`: {}", src.display(), e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read `{}`: {}", src.display(), e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to stat `{}`: {}", entry.path().display(), e))?;
        if file_type.is_symlink() {
            continue;
        }
        let from = entry.path();
        if file_type.is_dir() && should_skip_artifact_dir(&from) {
            continue;
        }
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir(&to)
                .map_err(|e| format!("Failed to create `{}`: {}", to.display(), e))?;
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .map_err(|e| format!("Failed to copy `{}`: {}", from.display(), e))?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStoreImageRequest {
    pub workspace_id: String,
    /// Base64-encoded image bytes (no `data:` URL prefix).
    pub data_base64: String,
    /// Source MIME type (e.g. `image/png`); normalized server-side.
    pub media_type: String,
    #[serde(default)]
    pub filename: Option<String>,
}

/// Persist a pasted/attached image under the workspace's image store and
/// return a ready-to-attach [`ContentPart::Image`] reference.
///
/// The bytes live on disk (see [`crate::assistant::image_store`]); the returned
/// part only carries a relative path, so it stays small in `content_json` and
/// gives CLI transports a real file to read at send time.
#[tauri::command]
pub async fn workspace_store_image(
    request: WorkspaceStoreImageRequest,
    state: State<'_, AppState>,
) -> Result<ContentPart, String> {
    use base64::Engine as _;

    let descriptor =
        resolve_workspace_descriptor(state.inner(), Some(request.workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;
    ensure_agent_workspace_root(root_path)?;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(request.data_base64.trim())
        .map_err(|error| format!("Invalid base64 image data: {}", error))?;

    let stored = crate::assistant::image_store::store_image(
        root_path,
        &bytes,
        &request.media_type,
        request.filename,
    )?;

    Ok(ContentPart::Image {
        id: stored.id,
        path: stored.path,
        media_type: stored.media_type,
        filename: stored.filename,
        width: None,
        height: None,
    })
}

/// Guess an image MIME type from a file extension.
///
/// The native file dialog returns a path, not clipboard bytes, so there is no
/// MIME to read — derive it from the extension. Returns `None` for anything the
/// image store would reject anyway, so the caller surfaces a clear error.
fn image_media_type_from_extension(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

/// Persist an image chosen via the native file dialog and return a
/// ready-to-attach [`ContentPart::Image`].
///
/// This is the reliable cross-platform attach path: clipboard image paste is
/// unreliable in the Linux WebKitGTK webview, so the composer also offers a
/// file picker that routes here. Reads the host file the user explicitly
/// selected, infers the MIME from its extension, then reuses the same image
/// store (size cap, MIME allowlist, workspace-root sandbox) as the paste path.
#[tauri::command]
pub async fn workspace_pick_and_store_image(
    workspace_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<ContentPart>, String> {
    use tauri_plugin_dialog::DialogExt;

    let descriptor = resolve_workspace_descriptor(state.inner(), Some(workspace_id.clone()))?;
    let root_path = descriptor
        .root_path
        .as_ref()
        .ok_or_else(|| "This workspace does not expose a filesystem root".to_string())?;
    ensure_agent_workspace_root(root_path)?;

    // The dialog runs in the backend, so the chosen path comes from a genuine
    // OS user selection — never a renderer-supplied string. A compromised
    // renderer can `invoke` this command, but cannot dictate which file is
    // read (it can only pop the picker), so it can't exfiltrate arbitrary
    // files (e.g. `~/.ssh/id_rsa`) into the image store. This command is async,
    // so `blocking_pick_file` runs off the main thread (required by the plugin).
    let picked = app
        .dialog()
        .file()
        .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
        .blocking_pick_file();

    let Some(file_path) = picked else {
        return Ok(None); // user cancelled — no attachment, no error
    };
    let source = file_path
        .into_path()
        .map_err(|error| format!("Invalid file path: {}", error))?;

    let media_type = image_media_type_from_extension(&source)
        .ok_or_else(|| "Unsupported image type. Use PNG, JPEG, GIF, or WebP.".to_string())?;
    let filename = source
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_string());

    let bytes = tokio::fs::read(&source)
        .await
        .map_err(|error| format!("Could not read image file: {}", error))?;

    let stored =
        crate::assistant::image_store::store_image(root_path, &bytes, media_type, filename)?;

    Ok(Some(ContentPart::Image {
        id: stored.id,
        path: stored.path,
        media_type: stored.media_type,
        filename: stored.filename,
        width: None,
        height: None,
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUpdateMcpRequest {
    pub workspace_id: String,
    pub mcp_server_ids: Vec<String>,
    /// Attached servers the user toggled off. Defaults to empty so older
    /// frontends that only send `mcp_server_ids` keep working.
    #[serde(default)]
    pub disabled_mcp_server_ids: Vec<String>,
}

/// Split an MCP update request into the effective (enabled) list and the
/// disabled list, enforcing that the two are disjoint: a server listed as
/// disabled is removed from the enabled set no matter what the caller sent.
fn sanitize_mcp_selection(request: &WorkspaceUpdateMcpRequest) -> (Vec<String>, Vec<String>) {
    let disabled: Vec<String> = request.disabled_mcp_server_ids.clone();
    let enabled: Vec<String> = request
        .mcp_server_ids
        .iter()
        .filter(|id| !disabled.contains(id))
        .cloned()
        .collect();
    (enabled, disabled)
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
                // Always Interactive: this row IS the canonical workspace
                // conversation that find_workspace_session resolves. The
                // resolver selects the most-recent non-task session for the
                // manager; the workspace itself only ever owns one such
                // session, so we keep it Interactive to match the historical
                // shape (and so any non-task BackgroundJob rows from older
                // scheduled-run code paths resolve ahead of this one only
                // because they were updated more recently, not by virtue of
                // their kind).
                kind: SessionKind::Interactive,
                title: Some(descriptor.title.clone()),
                context: desired_workspace_context(
                    &descriptor,
                    None,
                    workspace_agents.clone(),
                    workspace_manager.as_ref(),
                ),
            },
        )
        .await?
    };

    let (enabled_mcp_ids, disabled_mcp_ids) = sanitize_mcp_selection(&request);
    let mut updated = session;
    updated.context.mcp_server_ids = enabled_mcp_ids.clone();
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
        }

        // Persist the MCP selection into the workspace config — the canonical
        // store. The manager row keeps the effective enabled set (scheduled
        // runs and Workspace Settings read it); the workspace-level disabled
        // list remembers attached-but-toggled-off servers so the context-bar
        // toggle survives app restarts. The disabled list is written even
        // when no manager row exists, so the toggle still persists for a
        // manager-less workspace.
        let app_config = app_config(state.inner())?;
        let manager_id = workspace_manager.as_ref().map(|manager| manager.id.clone());
        update_workspace_config_for_id(state.inner(), &descriptor.workspace_id, |config| {
            let now = chrono::Utc::now().timestamp_millis();
            config.disabled_mcp_servers =
                workspace_config::mcp_ids_to_refs(&app_config, &disabled_mcp_ids);
            if let Some(manager_id) = manager_id.as_deref() {
                if let Some(agent) = config
                    .agents
                    .iter_mut()
                    .find(|agent| agent.id == manager_id)
                {
                    agent.selected_mcp_servers =
                        workspace_config::mcp_ids_to_refs(&app_config, &enabled_mcp_ids);
                    agent.updated_at = now;
                }
            }
            config.updated_at = now;
            Ok(())
        })?;
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
    update_workspace_config_for_id(state.inner(), &workspace_id, |config| {
        config.preferred_provider_connection_id = Some(provider_connection_id.clone());
        let default_agent_id = config.default_agent_id.clone();
        if let Some(manager) = config
            .agents
            .iter_mut()
            .find(|agent| agent.id == default_agent_id)
        {
            manager.provider_connection_ids = vec![provider_connection_id];
            manager.updated_at = now;
        }
        config.updated_at = now;
        Ok(())
    })?;

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

// =============================================================================
// Workspace CRUD — create, list, delete general workspaces
// =============================================================================

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceListEntry {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub enabled: bool,
    pub message_count: i64,
    pub assigned_agent_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_manager_name: Option<String>,
    pub running_task_count: i64,
    pub blocked_task_count: i64,
    pub failed_task_count: i64,
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
    /// Schedule mode (interval vs cron). Empty when the workspace is
    /// not scheduled. The Fleet card reads this to render the cadence
    /// label (`every 5m` for Interval, `0 9 * * 1-5` for Cron).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_kind: Option<crate::config::workspace_config::ScheduleKind>,
    // Seconds until the next scheduled run for this workspace's manager
    // agent, read live from the in-memory scheduler. None when the
    // scheduler has no entry for the manager (e.g. workspace was created
    // this session — the scheduler is only populated at startup).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_in_seconds: Option<u64>,
    /// A run completed in this workspace after the user last opened it —
    /// the rail renders an "unread" dot until `workspace_mark_opened`
    /// clears it.
    pub unread: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default)]
struct WorkspaceTaskAttentionSummary {
    running_task_count: i64,
    blocked_task_count: i64,
    failed_task_count: i64,
    latest_attention_task_id: Option<String>,
    latest_attention_task_title: Option<String>,
    latest_attention_task_status: Option<String>,
    latest_attention_task_summary: Option<String>,
    latest_attention_task_updated_at: Option<i64>,
}

impl WorkspaceTaskAttentionSummary {
    fn attention_task_count(&self) -> i64 {
        self.blocked_task_count + self.failed_task_count
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

    let mut config = WorkspaceConfig::new(id.clone(), display_title.clone(), now, manager_id);
    // Pre-wire the workspace's main agent to an existing provider so the user
    // can start chatting / scheduling without a detour to Settings.
    config.attach_default_provider(&app_config(state.inner())?.provider_connections, now);
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

    tracing::info!(workspace_id = %id, title = %display_title, "Created new general workspace");

    Ok(id)
}

/// Fork a workspace into a brand-new workspace.
///
/// Copies the durable setup and files: agents (with fresh ids), skills, MCP
/// servers, provider selection, execution policy / permissions, memories, and
/// artifacts. Runtime state is deliberately reset: the fork gets a fresh empty
/// SQLite DB, no runs, no queued messages, no tasks, and its schedule starts
/// disabled so it never auto-runs immediately after creation.
#[tauri::command]
pub async fn workspace_fork(
    request: WorkspaceForkRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let source_id = resolve_workspace_id(state.inner(), Some(request.workspace_id))?;
    let (source_root, source_config) = load_workspace_config_for_id(state.inner(), &source_id)?;

    let now = now_millis();
    let new_id = uuid::Uuid::new_v4().to_string();

    // Regenerate every (workspace-local) agent id and remember old→new so we
    // can remap the default-agent pointer. Everything else on the agent —
    // skills, MCP, providers, execution — is copied as-is via `..agent`.
    let mut id_map: HashMap<String, String> = HashMap::new();
    let agents: Vec<WorkspaceAgent> = source_config
        .agents
        .iter()
        .map(|agent| {
            let cloned_id = uuid::Uuid::new_v4().to_string();
            id_map.insert(agent.id.clone(), cloned_id.clone());
            WorkspaceAgent {
                id: cloned_id,
                created_at: now,
                updated_at: now,
                ..agent.clone()
            }
        })
        .collect();

    let default_agent_id = id_map
        .get(&source_config.default_agent_id)
        .cloned()
        .or_else(|| agents.first().map(|a| a.id.clone()))
        .ok_or_else(|| "Source workspace has no agents to fork".to_string())?;

    // Preserve the cadence but never auto-run a fresh fork.
    let mut schedule = source_config.schedule.clone();
    schedule.enabled = false;
    schedule.paused = false;
    schedule.next_run_at_unix_ms = None;

    let requested_title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let fork_title = requested_title
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} (Fork)", source_config.title));

    let forked = WorkspaceConfig {
        id: new_id.clone(),
        title: fork_title.clone(),
        created_at: now,
        updated_at: now,
        // A fresh fork has no runs — don't inherit the source's
        // unread/seen state.
        last_run_completed_at: 0,
        last_opened_at: 0,
        default_agent_id,
        schedule,
        agents,
        // Carries version + preferred_provider_connection_id (+ any future
        // top-level config fields) over unchanged.
        ..source_config.clone()
    };

    // Mirror workspace_create's filesystem + index + pool plumbing.
    let root = state.workspace_create_target(None)?.join(&new_id);
    let memory_dir = root.join(".clai").join("memory").join("journal");
    fs::create_dir_all(&memory_dir)
        .map_err(|e| format!("Failed to create workspace directory: {}", e))?;

    workspace_config::save(&root, &forked).map_err(|e| e.to_string())?;
    let workspace_pool = crate::db::init_workspace_db(&root).await?;
    copy_workspace_durable_files(&source_root, &root)?;
    workspace_config::save(&root, &forked).map_err(|e| e.to_string())?;
    write_fork_marker(
        &root,
        &source_id,
        &source_config.title,
        &new_id,
        request.prompt.as_deref(),
    )?;

    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .insert_config(root.clone(), &forked);
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .attach_pool(new_id.clone(), workspace_pool);

    tracing::info!(
        source_workspace_id = %source_id,
        workspace_id = %new_id,
        agents = forked.agents.len(),
        title = %fork_title,
        "Forked workspace into a new workspace"
    );

    Ok(new_id)
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
        let workspace_pool = state.workspace_db(&id).await?;
        let message_count = count_session_messages(&workspace_pool, state.inner(), &id).await;
        let task_attention = workspace_task_attention_summary(&workspace_pool, &id).await?;
        let (assigned_agent_count, default_manager_name) =
            workspace_team_summary(state.inner(), &id).await?;

        let manager_id = Some(locator.default_agent_id.clone());
        let schedule_enabled = locator.schedule_enabled;
        let schedule_paused = locator.schedule_paused;
        let schedule_kind = locator.schedule_kind.clone();

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
            assigned_agent_count,
            default_manager_name,
            running_task_count: task_attention.running_task_count,
            blocked_task_count: task_attention.blocked_task_count,
            failed_task_count: task_attention.failed_task_count,
            attention_task_count: task_attention.attention_task_count(),
            latest_attention_task_id: task_attention.latest_attention_task_id,
            latest_attention_task_title: task_attention.latest_attention_task_title,
            latest_attention_task_status: task_attention.latest_attention_task_status,
            latest_attention_task_summary: task_attention.latest_attention_task_summary,
            latest_attention_task_updated_at: task_attention.latest_attention_task_updated_at,
            schedule_enabled,
            schedule_paused,
            schedule_kind,
            next_run_in_seconds,
            unread: locator.last_run_completed_at > 0
                && locator.last_run_completed_at > locator.last_opened_at,
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

/// Trigger an immediate run of the workspace's manager agent. Takes a
/// workspace id (rather than the manager agent's id) so the Fleet UI's
/// workspace cards — which don't carry the manager agent id in the list
/// payload — can wire a "run now" action without an extra lookup.
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

/// Set the workspace's schedule (enable / disable + mode).
///
/// `kind = None` disables the schedule entirely; `kind = Some(Interval)`
/// or `kind = Some(Cron)` enables it with the chosen mode. The Cron
/// expression and timezone are validated server-side via
/// [`agents::schedule::compute_next_run_at`] before the config is saved,
/// so a malformed cron string returns a structured error to the FE
/// instead of silently breaking the scheduler. The persisted
/// `next_run_at_unix_ms` is recomputed against the new mode so the
/// schedule is exact ("next run at ...") from the user's perspective.
#[tauri::command]
pub async fn workspace_set_schedule(
    workspace_id: String,
    kind: Option<crate::config::workspace_config::ScheduleKind>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;
    let ((), config) = update_workspace_config_for_id(state.inner(), &workspace_id, |config| {
        match kind {
            None => {
                // Disabling clears the pause + next-run anchor so a future
                // re-enable starts from a fresh schedule.
                config.schedule.enabled = false;
                config.schedule.paused = false;
                config.schedule.next_run_at_unix_ms = None;
            }
            Some(new_kind) => {
                // Validate before saving — surfaces "invalid cron" /
                // "unknown timezone" / "interval=0" as a clean error to the UI.
                let now = now_millis();
                let next = crate::agents::schedule::compute_next_run_at(&new_kind, now)?;
                config.schedule.enabled = true;
                config.schedule.kind = new_kind;
                config.schedule.next_run_at_unix_ms = Some(next);
            }
        }
        config.updated_at = now_millis();
        Ok(())
    })?;

    let mut scheduler = state.scheduler.lock().await;
    crate::agents::init::apply_workspace_schedule(&mut scheduler, &config);

    Ok(())
}

/// Preview the next `n` fire times for a candidate schedule without
/// persisting it. Used by the workspace settings modal so the user can
/// see what "0 9 * * 1-5 America/New_York" actually means before saving.
///
/// Returns the next fire times as Unix-ms (UTC); the FE formats them
/// in the user's locale. For interval mode there's only one meaningful
/// "next" (now + N minutes), so the result is a single-element vec.
#[tauri::command]
pub async fn workspace_preview_schedule(
    kind: crate::config::workspace_config::ScheduleKind,
    count: Option<usize>,
) -> Result<Vec<i64>, String> {
    use crate::config::workspace_config::ScheduleKind;
    let now = now_millis();
    let n = count.unwrap_or(3).clamp(1, 10);
    match &kind {
        ScheduleKind::Interval { .. } => {
            // For interval mode the next-N preview is just repeated
            // increments — `now + N*interval` is informative enough
            // and avoids implying the cadence is anchored to wall-clock
            // boundaries (it isn't; it anchors to completion).
            Ok(vec![crate::agents::schedule::compute_next_run_at(
                &kind, now,
            )?])
        }
        ScheduleKind::Cron { .. } => crate::agents::schedule::upcoming_cron_runs(&kind, now, n),
    }
}

/// Best-effort detection of the host's IANA timezone for use as the
/// default value when the user first switches to cron mode. Falls back
/// to `"UTC"` if the platform can't resolve.
#[tauri::command]
pub fn workspace_host_timezone() -> String {
    crate::agents::schedule::host_timezone()
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
    let ((), config) = update_workspace_config_for_id(state.inner(), &workspace_id, |config| {
        if !config.schedule.enabled {
            return Err("Workspace is not periodic.".to_string());
        }
        config.schedule.paused = paused;
        config.updated_at = now_millis();
        Ok(())
    })?;

    let mut scheduler = state.scheduler.lock().await;
    crate::agents::init::apply_workspace_schedule(&mut scheduler, &config);

    Ok(())
}

/// Globally pause or resume the agent scheduler ("pause all workspaces").
///
/// A persisted overlay on top of per-workspace pause: while paused, NO
/// workspace's scheduled tick runs, regardless of its individual
/// `schedule.paused` state. That per-workspace state is preserved and takes
/// effect again on resume. In-flight runs are not interrupted — only new
/// ticks are suppressed. The choice is persisted to `AppConfig` so it survives
/// an app restart (restored in `populate_scheduler_from_workspace_agents`).
#[tauri::command]
pub async fn set_scheduler_paused(paused: bool, state: State<'_, AppState>) -> Result<(), String> {
    // Persist first so the choice survives a restart even if the process dies
    // right after; the in-memory scheduler flip below is idempotent.
    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager
            .update(|config| config.scheduler_paused = paused)
            .map_err(|e| format!("Failed to persist scheduler pause state: {}", e))?;
    }

    let mut scheduler = state.scheduler.lock().await;
    if paused {
        scheduler.pause("Paused by user".to_string());
    } else {
        scheduler.resume();
    }

    Ok(())
}

/// Whether the agent scheduler is globally paused (the "pause all" overlay).
/// Reads the persisted user intent so the UI can render the paused banner on
/// load.
#[tauri::command]
pub fn get_scheduler_paused(state: State<'_, AppState>) -> Result<bool, String> {
    let config_manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    Ok(config_manager.get().scheduler_paused)
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

    {
        let workspace_pool = state.workspace_db(&workspace_id).await?;
        if repository::workspace_has_active_run(&workspace_pool).await? {
            return Err(format!(
                "Workspace {} has an active run. Cancel or wait for it to finish before deleting the workspace.",
                workspace_id
            ));
        }
    }

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

    // Drain in-memory queues for this workspace. The purge helpers cancel
    // runs before dropping pending senders, so workspace deletion cannot be
    // mistaken for an interactive-request supersede.
    let purged_approvals = state
        .pending_approvals
        .purge_workspace_canceling_runs(&workspace_id, |run_id| {
            let _ = crate::assistant::runtime::cancel_run(run_id);
        })
        .await;
    let purged_path_grants = state
        .pending_path_grants
        .purge_workspace_canceling_runs(&workspace_id, |run_id| {
            let _ = crate::assistant::runtime::cancel_run(run_id);
        })
        .await;
    let mut purged_run_ids: Vec<String> = purged_approvals
        .iter()
        .chain(purged_path_grants.iter())
        .cloned()
        .collect();
    purged_run_ids.sort();
    purged_run_ids.dedup();

    tracing::info!(
        workspace_id = %workspace_id,
        agents_cleared = agent_ids.len(),
        approvals_purged = purged_approvals.len(),
        path_grants_purged = purged_path_grants.len(),
        runs_cancelled = purged_run_ids.len(),
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
    update_workspace_config_for_id(state.inner(), &workspace_id, |config| {
        config.title = trimmed.to_string();
        config.updated_at = now_millis();
        Ok(())
    })?;

    Ok(())
}

/// Record that the user opened (is viewing) a workspace, clearing its
/// "unread" indicator in the rail. Touches only `last_opened_at` — NOT
/// `updated_at` — so looking at a workspace never reorders the
/// recency-sorted list.
#[tauri::command]
pub async fn workspace_mark_opened(
    workspace_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace_id = resolve_workspace_id(state.inner(), Some(workspace_id))?;
    // Atomic RMW: this command fires on run-ends-while-viewing, i.e. at
    // the same instant the runner persists the schedule's next-run
    // anchor — a bare load→save here clobbered that anchor and made the
    // schedule re-fire on every app restart.
    update_workspace_config_for_id(state.inner(), &workspace_id, |config| {
        config.last_opened_at = now_millis();
        Ok(())
    })?;

    Ok(())
}

async fn workspace_task_attention_summary(
    pool: &DbPool,
    _workspace_id: &str,
) -> Result<WorkspaceTaskAttentionSummary, String> {
    let counts: (i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN status = 'blocked' AND attention_acknowledged_at IS NULL AND user_response_at IS NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN status = 'failed' AND attention_acknowledged_at IS NULL AND user_response_at IS NULL THEN 1 ELSE 0 END), 0)
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
            WHERE status IN ('blocked', 'failed')
                AND attention_acknowledged_at IS NULL
                AND user_response_at IS NULL
            ORDER BY
                CASE status
                    WHEN 'blocked' THEN 0
                    WHEN 'failed' THEN 1
                    ELSE 2
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

/// Count messages on the canonical workspace session — the *exact* same row
/// the card-click path loads via [`find_workspace_session`]. Both now route
/// through [`select_workspace_session`], so the card number and the
/// conversation length reference the same session by construction and can
/// never drift. (Previously this duplicated the selection inline and omitted
/// the `kind == Interactive` filter, so a self-task `BackgroundJob` session
/// could inflate the card count while the chat view showed the real
/// Interactive conversation.)
async fn count_session_messages(pool: &DbPool, state: &AppState, workspace_id: &str) -> i64 {
    let Ok(descriptor) = resolve_workspace_descriptor(state, Some(workspace_id.to_string())) else {
        return 0;
    };
    let Some(manager_id) =
        workspace_default_agent_id(state, &descriptor.workspace_id).unwrap_or(None)
    else {
        return 0;
    };

    let sessions = repository::list_non_task_sessions(pool)
        .await
        .unwrap_or_default();
    let Some(session) = select_workspace_session(
        sessions,
        &manager_id,
        &descriptor.workspace_id,
        descriptor.agent_id.as_deref(),
    ) else {
        return 0;
    };

    // COUNT(*) instead of loading every row just to take .len(). Counts the
    // head session only (unchanged behavior); ancestors are excluded.
    repository::count_session_chain_messages(pool, &session.id, false)
        .await
        .map(|n| n as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = "ws-1";
    const MGR: &str = "mgr-1";

    fn session(
        id: &str,
        kind: SessionKind,
        automation_id: &str,
        workspace_id: &str,
        updated_at: i64,
    ) -> AssistantSession {
        AssistantSession {
            id: id.to_string(),
            kind,
            title: None,
            context: SessionContext {
                workspace_id: Some(workspace_id.to_string()),
                automation_id: Some(automation_id.to_string()),
                ..Default::default()
            },
            created_at: 0,
            updated_at,
        }
    }

    // A workspace whose scheduled-run conversation was forked into its own
    // (non-task) BackgroundJob session must resolve to that live session, not
    // an older abandoned interactive stub. Selection is kind-agnostic: task
    // sessions are excluded upstream (list_non_task_sessions), so by the time
    // selection runs a newer background session here is a real conversation.
    #[test]
    fn picks_most_recent_non_task_session_regardless_of_kind() {
        let interactive_stub = session("stub", SessionKind::Interactive, MGR, WS, 100);
        let scheduled = session("scheduled", SessionKind::BackgroundJob, MGR, WS, 999);
        let chosen = select_workspace_session(vec![interactive_stub, scheduled], MGR, WS, None);
        assert_eq!(chosen.map(|s| s.id), Some("scheduled".to_string()));
    }

    #[test]
    fn ignores_sessions_owned_by_other_automations() {
        let other = session("other", SessionKind::Interactive, "someone-else", WS, 100);
        assert!(select_workspace_session(vec![other], MGR, WS, None).is_none());
    }

    #[test]
    fn picks_most_recent_interactive_when_several_match() {
        let older = session("old", SessionKind::Interactive, MGR, WS, 10);
        let newer = session("new", SessionKind::Interactive, MGR, WS, 20);
        let chosen = select_workspace_session(vec![older, newer], MGR, WS, None);
        assert_eq!(chosen.map(|s| s.id), Some("new".to_string()));
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        assert!(select_workspace_session(vec![], MGR, WS, None).is_none());
    }

    // Pin the skip list for `copy_workspace_durable_files`. Image
    // attachments live under `.clai/images/` and are big binaries
    // already correctly owned by the *source* workspace's store —
    // copying them into a forked workspace's store just doubles
    // disk usage and risks one store losing track of a file the
    // other still references.
    #[test]
    fn fork_copy_skips_workspace_local_image_store() {
        use std::path::Path;

        // The image store is a top-level child of `.clai/`, so the
        // skip is matched at that depth. The caller only ever hands
        // us paths of the shape `.clai/<name>`, never `.clai/.../...`.
        assert!(should_skip_fork_copy_path(Path::new(".clai/images")));
        // Pre-existing skips must still hold.
        assert!(should_skip_fork_copy_path(Path::new(".clai/config.json")));
        assert!(should_skip_fork_copy_path(Path::new(
            ".clai/config.json.tmp"
        )));
        assert!(should_skip_fork_copy_path(Path::new(".clai/data.sqlite")));
        assert!(should_skip_fork_copy_path(Path::new(
            ".clai/data.sqlite-wal"
        )));
        // Unrelated top-level children of `.clai/` are still copied
        // (e.g. user-installed skills, terminals, agent templates).
        assert!(!should_skip_fork_copy_path(Path::new(".clai/skills")));
        assert!(!should_skip_fork_copy_path(Path::new(".clai/agents")));
        // Paths outside `.clai/` are not its concern.
        assert!(!should_skip_fork_copy_path(Path::new("images")));
        assert!(!should_skip_fork_copy_path(Path::new("src/main.rs")));
    }

    #[test]
    fn protected_artifact_path_guards_dbs_and_traversal() {
        // Protected first-component dirs are refused.
        assert!(is_protected_artifact_path(Path::new(".clai")));
        assert!(is_protected_artifact_path(Path::new(".clai/data.sqlite")));
        assert!(is_protected_artifact_path(Path::new(".git/config")));
        assert!(is_protected_artifact_path(Path::new("target/debug")));
        assert!(is_protected_artifact_path(Path::new("node_modules/x")));
        // Empty / traversal / non-normal leading components are refused too.
        assert!(is_protected_artifact_path(Path::new("")));
        assert!(is_protected_artifact_path(Path::new("../escape")));
        assert!(is_protected_artifact_path(Path::new("/abs")));
        // Ordinary artifacts are deletable.
        assert!(!is_protected_artifact_path(Path::new("report.md")));
        assert!(!is_protected_artifact_path(Path::new(
            "work/repo/src/main.rs"
        )));
        assert!(!is_protected_artifact_path(Path::new("images/pic.png")));
    }

    #[test]
    fn viewer_for_path_classifies_binary_as_external() {
        use std::path::Path;
        assert_eq!(viewer_for_path(Path::new("report.pdf")), "external");
        assert_eq!(viewer_for_path(Path::new("photo.PNG")), "external");
        assert_eq!(viewer_for_path(Path::new("archive.zip")), "external");
        assert_eq!(viewer_for_path(Path::new("data.sqlite")), "external");
        // Text/known viewers are unchanged.
        assert_eq!(viewer_for_path(Path::new("notes.md")), "markdown");
        assert_eq!(viewer_for_path(Path::new("main.rs")), "text");
        assert_eq!(viewer_for_path(Path::new("data.json")), "json");
    }

    #[test]
    fn copy_artifact_handles_files_dirs_collisions_and_skips() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        // A file copies, and a second copy of the same name gets suffixed.
        std::fs::write(src.path().join("note.md"), b"hello").unwrap();
        let a = copy_artifact_to_unique_destination(
            &src.path().join("note.md"),
            dst.path(),
            "note.md",
            false,
        )
        .unwrap();
        assert_eq!(a.file_name().unwrap(), "note.md");
        let b = copy_artifact_to_unique_destination(
            &src.path().join("note.md"),
            dst.path(),
            "note.md",
            false,
        )
        .unwrap();
        assert_eq!(b.file_name().unwrap(), "note (1).md");
        assert_eq!(std::fs::read(dst.path().join("note.md")).unwrap(), b"hello");

        // A directory copies recursively, but symlinks and protected dirs
        // (.git) are skipped.
        let tree = src.path().join("proj");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::create_dir_all(tree.join(".git")).unwrap();
        std::fs::write(tree.join("a.txt"), b"a").unwrap();
        std::fs::write(tree.join("sub/b.txt"), b"b").unwrap();
        std::fs::write(tree.join(".git/config"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hostname", tree.join("link")).unwrap();

        let out = copy_artifact_to_unique_destination(&tree, dst.path(), "proj", true).unwrap();
        assert_eq!(out.file_name().unwrap(), "proj");
        assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(out.join("sub/b.txt")).unwrap(), b"b");
        assert!(!out.join(".git").exists(), ".git must be skipped");
        #[cfg(unix)]
        assert!(!out.join("link").exists(), "symlink must be skipped");
    }

    #[test]
    fn resolve_copy_source_guards_traversal_and_protected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proj/.git")).unwrap();
        std::fs::write(root.path().join("proj/.git/config"), b"x").unwrap();
        std::fs::write(root.path().join("proj/a.txt"), b"a").unwrap();

        let (src, is_dir, name) = resolve_copy_source(root.path(), "proj/a.txt").unwrap();
        assert!(!is_dir);
        assert_eq!(name, "a.txt");
        assert!(src.ends_with("a.txt"));

        // Protected dir at a non-leading component must be rejected (not just
        // the first-component delete policy).
        assert!(resolve_copy_source(root.path(), "proj/.git/config").is_err());
        // Traversal / non-normal components rejected before touching the fs.
        assert!(resolve_copy_source(root.path(), "../etc/hosts").is_err());
        assert!(resolve_copy_source(root.path(), "proj/../../etc").is_err());
        // Empty / root rejected.
        assert!(resolve_copy_source(root.path(), "   ").is_err());
    }

    #[test]
    fn resolve_artifact_destination_dir_guards_traversal_protected_and_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proj/out")).unwrap();
        std::fs::create_dir_all(root.path().join("proj/.git/inner")).unwrap();
        std::fs::write(root.path().join("proj/file.txt"), b"x").unwrap();

        let root_dest = resolve_artifact_destination_dir(root.path(), "").unwrap();
        assert_eq!(root_dest, root.path().canonicalize().unwrap());

        let nested = resolve_artifact_destination_dir(root.path(), "proj/out").unwrap();
        assert_eq!(nested, root.path().join("proj/out").canonicalize().unwrap());

        assert!(resolve_artifact_destination_dir(root.path(), "../outside").is_err());
        assert!(resolve_artifact_destination_dir(root.path(), "proj/.git").is_err());
        assert!(resolve_artifact_destination_dir(root.path(), "proj/.git/inner").is_err());
        assert!(resolve_artifact_destination_dir(root.path(), "proj/file.txt").is_err());
    }

    #[tokio::test]
    async fn move_artifact_within_workspace_root_moves_and_returns_relative_path() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::create_dir_all(root.path().join("dest")).unwrap();
        std::fs::write(root.path().join("src/note.md"), b"new").unwrap();
        std::fs::write(root.path().join("dest/note.md"), b"existing").unwrap();

        let moved = move_artifact_within_workspace_root(root.path(), "src/note.md", "dest")
            .await
            .unwrap();

        assert_eq!(moved, "dest/note (1).md");
        assert!(!root.path().join("src/note.md").exists());
        assert_eq!(
            std::fs::read(root.path().join("dest/note.md")).unwrap(),
            b"existing"
        );
        assert_eq!(
            std::fs::read(root.path().join("dest/note (1).md")).unwrap(),
            b"new"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn rename_no_replace_rejects_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        let dest = root.path().join("dest.txt");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&dest, b"dest").unwrap();

        let error = rename_no_replace(&source, &dest).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&source).unwrap(), b"source");
        assert_eq!(std::fs::read(&dest).unwrap(), b"dest");
    }

    #[test]
    fn move_artifact_handles_collisions_and_current_parent_noop() {
        let root = tempfile::tempdir().unwrap();
        let source_parent = root.path().join("src");
        let dest = root.path().join("dest");
        std::fs::create_dir_all(&source_parent).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(source_parent.join("note.md"), b"new").unwrap();
        std::fs::write(dest.join("note.md"), b"existing").unwrap();

        let moved =
            move_artifact_to_unique_destination(&source_parent.join("note.md"), &dest, "note.md")
                .unwrap();
        assert_eq!(moved.file_name().unwrap(), "note (1).md");
        assert!(!source_parent.join("note.md").exists());
        assert_eq!(std::fs::read(dest.join("note.md")).unwrap(), b"existing");
        assert_eq!(std::fs::read(dest.join("note (1).md")).unwrap(), b"new");

        std::fs::write(dest.join("same-parent.txt"), b"same").unwrap();
        let same_parent = move_artifact_to_unique_destination(
            &dest.join("same-parent.txt"),
            &dest,
            "same-parent.txt",
        )
        .unwrap();
        assert_eq!(same_parent, dest.join("same-parent.txt"));
        assert_eq!(
            std::fs::read(dest.join("same-parent.txt")).unwrap(),
            b"same"
        );
    }

    #[test]
    fn move_artifact_rejects_folder_into_self_or_descendant() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proj/child")).unwrap();
        let source = root.path().join("proj").canonicalize().unwrap();
        let child = root.path().join("proj/child").canonicalize().unwrap();
        let parent = root.path().canonicalize().unwrap();

        assert!(ensure_move_destination_allowed(&source, &source, true).is_err());
        assert!(ensure_move_destination_allowed(&source, &child, true).is_err());
        assert!(ensure_move_destination_allowed(&source, &parent, true).is_ok());
        assert!(ensure_move_destination_allowed(&source, &child, false).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_copy_source_rejects_symlink_item_and_escaping_parent() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"s").unwrap();

        // A top-level symlink item pointing outside is rejected as a symlink.
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("link"))
            .unwrap();
        assert!(resolve_copy_source(root.path(), "link").is_err());

        // An intermediate symlinked directory that escapes the root is rejected
        // by canonical containment (the final component is a real file).
        std::os::unix::fs::symlink(outside.path(), root.path().join("outdir")).unwrap();
        assert!(resolve_copy_source(root.path(), "outdir/secret").is_err());
    }

    fn descriptor(agent_id: Option<&str>) -> WorkspaceDescriptor {
        WorkspaceDescriptor {
            workspace_id: WS.to_string(),
            kind: "general".to_string(),
            title: "Test".to_string(),
            agent_id: agent_id.map(str::to_string),
            root_path: None,
            provider_connection_ids: vec![],
            selected_mcp_server_ids: vec![],
            selected_mcp_server_names: vec![],
            disabled_mcp_server_ids: vec![],
            execution: ExecutionCapabilityConfig::default(),
            tool_scopes: vec![],
            automation_name: None,
        }
    }

    #[test]
    fn sanitize_mcp_selection_keeps_lists_disjoint() {
        let request = WorkspaceUpdateMcpRequest {
            workspace_id: WS.to_string(),
            mcp_server_ids: vec!["a".into(), "b".into(), "c".into()],
            disabled_mcp_server_ids: vec!["b".into(), "d".into()],
        };
        let (enabled, disabled) = sanitize_mcp_selection(&request);
        assert_eq!(enabled, vec!["a".to_string(), "c".to_string()]);
        assert_eq!(disabled, vec!["b".to_string(), "d".to_string()]);
    }

    #[test]
    fn sanitize_mcp_selection_defaults_to_no_disabled() {
        // Older frontends omit disabledMcpServerIds; serde defaults it to
        // empty and the enabled list must pass through untouched.
        let request: WorkspaceUpdateMcpRequest = serde_json::from_value(serde_json::json!({
            "workspaceId": WS,
            "mcpServerIds": ["a", "b"],
        }))
        .unwrap();
        let (enabled, disabled) = sanitize_mcp_selection(&request);
        assert_eq!(enabled, vec!["a".to_string(), "b".to_string()]);
        assert!(disabled.is_empty());
    }

    #[test]
    fn desired_workspace_context_general_workspaces_take_mcp_from_config() {
        // The workspace config (via the descriptor) is the canonical store:
        // its enabled list wins over whatever the session row carries.
        let mut existing = session("s-1", SessionKind::Interactive, MGR, WS, 1);
        existing.context.mcp_server_ids = vec!["stale".into()];

        let mut desc = descriptor(None);
        desc.selected_mcp_server_ids = vec!["a".into()];
        let manager = manager_config();
        let context = desired_workspace_context(&desc, Some(&existing), vec![], Some(&manager));
        assert_eq!(context.mcp_server_ids, vec!["a".to_string()]);
    }

    fn manager_config() -> crate::config::AgentConfig {
        crate::config::AgentConfig::new(
            "Manager".to_string(),
            String::new(),
            crate::config::workspace_config::ScheduleKind::default(),
        )
    }

    #[test]
    fn desired_workspace_context_all_disabled_config_beats_stale_session() {
        // With a manager row present, selected=[] + disabled=[b] means
        // "every attached server toggled off" — the fallback must not
        // resurrect the session's list.
        let mut existing = session("s-1", SessionKind::Interactive, MGR, WS, 1);
        existing.context.mcp_server_ids = vec!["stale".into()];

        let mut desc = descriptor(None);
        desc.disabled_mcp_server_ids = vec!["b".into()];
        let manager = manager_config();
        let context = desired_workspace_context(&desc, Some(&existing), vec![], Some(&manager));
        assert!(context.mcp_server_ids.is_empty());
    }

    #[test]
    fn desired_workspace_context_manager_less_keeps_session_minus_disabled() {
        // Without a manager row the config cannot record an enabled
        // selection, so the session's list stays canonical — but the
        // config-level disabled toggle still applies to it.
        let mut existing = session("s-1", SessionKind::Interactive, MGR, WS, 1);
        existing.context.mcp_server_ids = vec!["a".into(), "b".into()];

        let mut desc = descriptor(None);
        desc.disabled_mcp_server_ids = vec!["b".into()];
        let context = desired_workspace_context(&desc, Some(&existing), vec![], None);
        assert_eq!(context.mcp_server_ids, vec!["a".to_string()]);
    }

    #[test]
    fn desired_workspace_context_falls_back_to_legacy_session_mcp() {
        // Sessions that predate the config mirror keep their MCP selection
        // when the config has none recorded.
        let mut existing = session("s-1", SessionKind::Interactive, MGR, WS, 1);
        existing.context.mcp_server_ids = vec!["legacy".into()];

        let manager = manager_config();
        let context =
            desired_workspace_context(&descriptor(None), Some(&existing), vec![], Some(&manager));
        assert_eq!(context.mcp_server_ids, vec!["legacy".to_string()]);
    }

    #[test]
    fn workspace_config_disabled_mcp_servers_backward_compatible() {
        // Config files written before this field existed must parse with an
        // empty disabled list, and the field must round-trip when present.
        let mut config = workspace_config::WorkspaceConfig::new(
            WS.to_string(),
            "Test".to_string(),
            1,
            MGR.to_string(),
        );
        let mut legacy = serde_json::to_value(&config).unwrap();
        legacy.as_object_mut().unwrap().remove("disabledMcpServers");
        let parsed: workspace_config::WorkspaceConfig = serde_json::from_value(legacy).unwrap();
        assert!(parsed.disabled_mcp_servers.is_empty());

        config.disabled_mcp_servers = vec![workspace_config::McpRef {
            name: "b".to_string(),
        }];
        let json = serde_json::to_value(&config).unwrap();
        let back: workspace_config::WorkspaceConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.disabled_mcp_servers.len(), 1);
        assert_eq!(back.disabled_mcp_servers[0].name, "b");
    }
}

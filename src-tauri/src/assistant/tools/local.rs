use glob::{MatchOptions, Pattern};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use tokio::time::Duration;

use crate::assistant::sandbox::{
    run_command, SandboxCommand, SandboxEnv, SandboxNetworkMode, SandboxPathAccess,
    SandboxPathGrant, SandboxProfile, SandboxSessionBusMode,
};
use crate::assistant::types::RunNoticeKind;
use crate::config::{
    ExecutionCapabilityConfig, FilesystemPathAccess, FilesystemPathGrant, GrantOrigin,
    SandboxNetworkConfig, SandboxSessionBusConfig, ShellAccessMode,
};

use super::ToolExecutionContext;

const DEFAULT_FILE_READ_LIMIT: usize = 20_000;
const MAX_FILE_READ_LIMIT: usize = 200_000;
const DEFAULT_FS_LIST_LIMIT: usize = 200;
const MAX_FS_LIST_LIMIT: usize = 2_000;
const DEFAULT_FS_GLOB_LIMIT: usize = 200;
const MAX_FS_GLOB_LIMIT: usize = 2_000;
const DEFAULT_BASH_TIMEOUT_MS: u64 = 120_000;
const MAX_BASH_TIMEOUT_MS: u64 = 600_000;
const DEFAULT_BASH_OUTPUT_LIMIT: usize = 20_000;
const MAX_BASH_OUTPUT_LIMIT: usize = 200_000;
const DEFAULT_WEB_FETCH_CONTENT_LIMIT: usize = 20_000;
const MAX_WEB_FETCH_CONTENT_LIMIT: usize = 100_000;
const DEFAULT_WEB_FETCH_TIMEOUT_MS: u64 = 15_000;
const MAX_WEB_FETCH_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_WEB_SEARCH_MAX_RESULTS: usize = 10;
const MAX_WEB_SEARCH_MAX_RESULTS: usize = 20;
const WEB_SEARCH_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsListParams {
    #[serde(default = "default_current_path")]
    path: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsGlobParams {
    pattern: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsReadParams {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsWriteParams {
    path: String,
    content: String,
    #[serde(default)]
    create_parents: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BashExecParams {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_output_chars: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsRequestGrantParams {
    /// Path the agent wants access to. Absolute or `~`-prefixed; the
    /// backend resolves `~` against the host `HOME` env var.
    path: String,
    /// `read_only` or `read_write`.
    access: FilesystemPathAccess,
    /// Why the agent needs this access. Shown to the user in the modal so
    /// they can make an informed decision.
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebSearchParams {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebFetchParams {
    url: String,
    #[serde(default)]
    max_content_chars: Option<usize>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessKind {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone)]
struct ResolvedGrant {
    root: PathBuf,
    access: AccessKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilesystemEntry {
    path: PathBuf,
    kind: &'static str,
}

pub async fn execute_local_tool(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match tool_name {
        "fs_list" => {
            let params: FsListParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid fs_list params: {}", e))?;
            execute_fs_list(context, params)
        }
        "fs_glob" => {
            let params: FsGlobParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid fs_glob params: {}", e))?;
            execute_fs_glob(context, params)
        }
        "fs_read" => {
            let params: FsReadParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid fs_read params: {}", e))?;
            execute_fs_read(context, params)
        }
        "fs_write" => {
            let params: FsWriteParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid fs_write params: {}", e))?;
            execute_fs_write(context, params)
        }
        "bash_exec" => {
            let params: BashExecParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid bash_exec params: {}", e))?;
            execute_bash_exec(deps, context, params).await
        }
        "fs_request_grant" => {
            let params: FsRequestGrantParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid fs_request_grant params: {}", e))?;
            execute_fs_request_grant(deps, context, params).await
        }
        "web_search" => {
            let params: WebSearchParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid web_search params: {}", e))?;
            execute_web_search(params).await
        }
        "web_fetch" => {
            let params: WebFetchParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid web_fetch params: {}", e))?;
            execute_web_fetch(params).await
        }
        _ => Err(format!("Unknown local tool: {}", tool_name)),
    }
}

fn default_current_path() -> String {
    ".".to_string()
}

fn execute_fs_list(
    context: &ToolExecutionContext,
    params: FsListParams,
) -> Result<serde_json::Value, String> {
    let grants = filesystem_grants(context)?;
    let path = resolve_allowed_existing_path(&params.path, &grants, false).inspect_err(|e| {
        if e.contains("outside the agent's allowed filesystem grants") || e.contains("not writable")
        {
            context.add_notice(RunNoticeKind::PathDenied, e.clone());
        }
    })?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_FS_LIST_LIMIT)
        .min(MAX_FS_LIST_LIMIT);
    let (entries, truncated) = list_entries_at_path(&path, params.recursive, limit)?;

    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "entries": serialize_entries(&entries),
        "recursive": params.recursive,
        "truncated": truncated,
        "limit": limit
    }))
}

fn execute_fs_glob(
    context: &ToolExecutionContext,
    params: FsGlobParams,
) -> Result<serde_json::Value, String> {
    let grants = filesystem_grants(context)?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_FS_GLOB_LIMIT)
        .min(MAX_FS_GLOB_LIMIT);
    let (entries, truncated) =
        glob_allowed_paths(&params.pattern, &grants, limit).inspect_err(|e| {
            if e.contains("outside the agent's allowed filesystem grants") {
                context.add_notice(RunNoticeKind::PathDenied, e.clone());
            }
        })?;

    Ok(serde_json::json!({
        "pattern": params.pattern,
        "matches": serialize_entries(&entries),
        "truncated": truncated,
        "limit": limit
    }))
}

fn execute_fs_read(
    context: &ToolExecutionContext,
    params: FsReadParams,
) -> Result<serde_json::Value, String> {
    let grants = filesystem_grants(context)?;
    let path = resolve_allowed_existing_path(&params.path, &grants, false).inspect_err(|e| {
        if e.contains("outside the agent's allowed filesystem grants") || e.contains("not writable")
        {
            context.add_notice(RunNoticeKind::PathDenied, e.clone());
        }
    })?;
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let offset = params.offset.unwrap_or(0);
    let limit = params
        .limit
        .unwrap_or(DEFAULT_FILE_READ_LIMIT)
        .min(MAX_FILE_READ_LIMIT);
    let chars: Vec<char> = content.chars().collect();
    let start = offset.min(chars.len());
    let end = (start + limit).min(chars.len());
    let slice: String = chars[start..end].iter().collect();

    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "content": slice,
        "truncated": end < chars.len(),
        "offset": start,
        "limit": limit
    }))
}

fn execute_fs_write(
    context: &ToolExecutionContext,
    params: FsWriteParams,
) -> Result<serde_json::Value, String> {
    let grants = filesystem_grants(context)?;
    let path = resolve_allowed_path(&params.path, &grants, true).inspect_err(|e| {
        if e.contains("outside the agent's allowed filesystem grants") || e.contains("not writable")
        {
            context.add_notice(RunNoticeKind::PathDenied, e.clone());
        }
    })?;

    if params.create_parents {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Failed to create parent directories for {}: {}",
                    path.display(),
                    e
                )
            })?;
        }
    }

    fs::write(&path, params.content.as_bytes())
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;

    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "bytesWritten": params.content.len()
    }))
}

async fn execute_bash_exec(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    params: BashExecParams,
) -> Result<serde_json::Value, String> {
    if matches!(context.execution.shell.mode, ShellAccessMode::Off) {
        return Err("Shell access is disabled for this agent".to_string());
    }

    let cwd = resolve_shell_cwd(context, params.cwd.as_deref())?;
    let workspace_root = ensure_workspace_root(context)?;

    match evaluate_command_policy(&context.execution, &params.command) {
        PolicyResult::Allow => { /* proceed */ }
        PolicyResult::Block {
            segment_text,
            matched_prefix,
        } => {
            let message = if segment_text.is_empty() {
                "Shell command cannot be empty".to_string()
            } else {
                format!(
                    "Command `{}` is blocked for this agent (matched prefix `{}`)",
                    segment_text, matched_prefix
                )
            };
            return Err(message);
        }
        PolicyResult::NeedsApproval(segments) => {
            await_user_permission(deps, context, &params.command, segments).await?;
        }
    }

    let timeout_ms = params
        .timeout_ms
        .unwrap_or(DEFAULT_BASH_TIMEOUT_MS)
        .min(MAX_BASH_TIMEOUT_MS);
    let output_limit = params
        .max_output_chars
        .unwrap_or(DEFAULT_BASH_OUTPUT_LIMIT)
        .min(MAX_BASH_OUTPUT_LIMIT);

    let output = run_command(SandboxCommand {
        argv: vec!["/bin/sh".into(), "-lc".into(), params.command.into()],
        cwd,
        timeout_ms,
        max_output_chars: output_limit,
        profile: sandbox_profile(context, workspace_root)?,
    })
    .await
    .inspect_err(|error| {
        // Surface sandbox-availability failures (bwrap missing, kernel
        // refused namespaces, etc.) as a first-class run notice so the
        // run completes with a SandboxUnavailable warning rather than a
        // generic tool failure. The sandbox runner uses the
        // "Sandboxed shell is unavailable" sentinel prefix for exactly
        // this case; other errors stay as plain tool failures.
        if error.starts_with("Sandboxed shell is unavailable") {
            context.add_notice(RunNoticeKind::SandboxUnavailable, error.clone());
        }
    })?;

    Ok(serde_json::json!({
        "cwd": output.cwd.display().to_string(),
        "exitCode": output.exit_code,
        "success": output.success,
        "stdout": output.stdout,
        "stderr": output.stderr
    }))
}

fn filesystem_grants(context: &ToolExecutionContext) -> Result<Vec<ResolvedGrant>, String> {
    let mut grants = vec![ResolvedGrant {
        root: ensure_workspace_root(context)?,
        access: AccessKind::ReadWrite,
    }];

    for grant in &context.execution.filesystem.extra_paths {
        let resolved = resolve_grant(grant)?;
        if !grants
            .iter()
            .any(|existing| existing.root == resolved.root && existing.access == resolved.access)
        {
            grants.push(resolved);
        }
    }

    // Run-scoped grants accepted via the fs_request_grant modal. These come
    // last so the dedup above doesn't drop them in favour of weaker durable
    // entries — but path resolution still goes through the same helper so
    // a session grant on `~/.cargo` lands in the same shape as a durable one.
    for grant in context.session_grants_snapshot() {
        let resolved = resolve_grant(&grant)?;
        if !grants
            .iter()
            .any(|existing| existing.root == resolved.root && existing.access == resolved.access)
        {
            grants.push(resolved);
        }
    }

    Ok(grants)
}

fn list_entries_at_path(
    path: &Path,
    recursive: bool,
    limit: usize,
) -> Result<(Vec<FilesystemEntry>, bool), String> {
    let mut entries = Vec::new();

    if path.is_dir() {
        let truncated = collect_dir_entries(path, recursive, limit, &mut entries)?;
        Ok((entries, truncated))
    } else {
        Ok((vec![describe_path(path)?], false))
    }
}

fn collect_dir_entries(
    dir: &Path,
    recursive: bool,
    limit: usize,
    entries: &mut Vec<FilesystemEntry>,
) -> Result<bool, String> {
    let mut dir_entries = fs::read_dir(dir)
        .map_err(|e| format!("Failed to list {}: {}", dir.display(), e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to list {}: {}", dir.display(), e))?;
    dir_entries.sort_by_key(|entry| entry.path());

    for entry in dir_entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to inspect {}: {}", path.display(), e))?;
        entries.push(FilesystemEntry {
            path: path.clone(),
            kind: classify_file_type(&file_type),
        });

        if entries.len() >= limit {
            return Ok(true);
        }

        if recursive && file_type.is_dir() && collect_dir_entries(&path, true, limit, entries)? {
            return Ok(true);
        }
    }

    Ok(false)
}

fn glob_allowed_paths(
    pattern: &str,
    grants: &[ResolvedGrant],
    limit: usize,
) -> Result<(Vec<FilesystemEntry>, bool), String> {
    let normalized_pattern = normalize_pattern_string(pattern);
    if normalized_pattern.is_empty() {
        return Err("Glob pattern cannot be empty".to_string());
    }

    let matcher =
        Pattern::new(&normalized_pattern).map_err(|e| format!("Invalid glob pattern: {}", e))?;
    let absolute_pattern = Path::new(&normalized_pattern).is_absolute();

    if absolute_pattern {
        if let Some(prefix) = literal_path_prefix(&normalized_pattern) {
            let intersects_grants = grants
                .iter()
                .any(|grant| path_prefix_intersects(&prefix, &grant.root));
            if !intersects_grants {
                return Err(format!(
                    "Pattern {} is outside the agent's allowed filesystem grants",
                    normalized_pattern
                ));
            }
        }
    }

    let options = MatchOptions {
        case_sensitive: !cfg!(windows),
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };

    let mut ctx = GlobWalkContext {
        matcher,
        options,
        absolute_pattern,
        limit,
        seen: std::collections::HashSet::new(),
        matches: Vec::new(),
    };

    for grant in grants {
        if collect_glob_matches(&grant.root, &grant.root, &mut ctx)? {
            return Ok((ctx.matches, true));
        }
    }

    Ok((ctx.matches, false))
}

struct GlobWalkContext {
    matcher: Pattern,
    options: MatchOptions,
    absolute_pattern: bool,
    limit: usize,
    seen: std::collections::HashSet<PathBuf>,
    matches: Vec<FilesystemEntry>,
}

fn collect_glob_matches(
    root: &Path,
    current: &Path,
    ctx: &mut GlobWalkContext,
) -> Result<bool, String> {
    let mut dir_entries = fs::read_dir(current)
        .map_err(|e| format!("Failed to list {}: {}", current.display(), e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to list {}: {}", current.display(), e))?;
    dir_entries.sort_by_key(|entry| entry.path());

    for entry in dir_entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to inspect {}: {}", path.display(), e))?;
        let candidate = if ctx.absolute_pattern {
            path_to_match_string(&path)
        } else {
            let relative_path = path.strip_prefix(root).map_err(|e| {
                format!(
                    "Failed to resolve relative path for {}: {}",
                    path.display(),
                    e
                )
            })?;
            path_to_match_string(relative_path)
        };

        if ctx.matcher.matches_with(&candidate, ctx.options) && ctx.seen.insert(path.clone()) {
            ctx.matches.push(FilesystemEntry {
                path: path.clone(),
                kind: classify_file_type(&file_type),
            });
            if ctx.matches.len() >= ctx.limit {
                return Ok(true);
            }
        }

        if file_type.is_dir() && collect_glob_matches(root, &path, ctx)? {
            return Ok(true);
        }
    }

    Ok(false)
}

fn resolve_grant(grant: &FilesystemPathGrant) -> Result<ResolvedGrant, String> {
    Ok(ResolvedGrant {
        root: resolve_configured_path(&grant.path)?,
        access: match grant.access {
            FilesystemPathAccess::ReadOnly => AccessKind::ReadOnly,
            FilesystemPathAccess::ReadWrite => AccessKind::ReadWrite,
        },
    })
}

fn ensure_workspace_root(context: &ToolExecutionContext) -> Result<PathBuf, String> {
    let workspace_root = context.workspace_root.clone().ok_or_else(|| {
        "Agent workspace is unavailable because this session is not tied to an automation"
            .to_string()
    })?;

    // Create workspace root and pre-seed .clai/memory/ (including journal/)
    // so agents can use it immediately without spending a tool call.
    let memory_dir = workspace_root.join(".clai").join("memory");
    let journal_dir = memory_dir.join("journal");
    fs::create_dir_all(&journal_dir).map_err(|e| {
        format!(
            "Failed to create agent workspace {}: {}",
            workspace_root.display(),
            e
        )
    })?;

    Ok(workspace_root)
}

fn resolve_shell_cwd(
    context: &ToolExecutionContext,
    requested_cwd: Option<&str>,
) -> Result<PathBuf, String> {
    let agent_workspace = ensure_workspace_root(context)?;
    let base = requested_cwd.unwrap_or(".");

    let cwd = if base == "." {
        agent_workspace.clone()
    } else {
        let grants = filesystem_grants(context)?;
        resolve_allowed_path(base, &grants, true)
            .or_else(|_| resolve_allowed_path(base, &grants, false))?
    };

    Ok(cwd)
}

fn sandbox_profile(
    context: &ToolExecutionContext,
    workspace_root: PathBuf,
) -> Result<SandboxProfile, String> {
    let mut path_grants = Vec::new();
    for grant in filesystem_grants(context)? {
        if grant.root == workspace_root {
            continue;
        }
        path_grants.push(SandboxPathGrant {
            host_path: grant.root,
            access: match grant.access {
                AccessKind::ReadOnly => SandboxPathAccess::ReadOnly,
                AccessKind::ReadWrite => SandboxPathAccess::ReadWrite,
            },
        });
    }

    let network = match context.execution.sandbox.network {
        SandboxNetworkConfig::Enabled => SandboxNetworkMode::Host,
        SandboxNetworkConfig::Disabled => SandboxNetworkMode::Disabled,
    };

    let session_bus = match context.execution.sandbox.session_bus {
        SandboxSessionBusConfig::Deny => SandboxSessionBusMode::Deny,
        SandboxSessionBusConfig::Allow => SandboxSessionBusMode::Allow,
    };

    // HOME env points at the user's real $HOME so `~/.foo` resolves the
    // same way the user's own shell does. The user's `extra_paths` config
    // is the source of truth for what's actually visible: a new agent's
    // defaults include `$HOME` (RO) as a normal entry, and the user can
    // remove it from agent settings if they want a fully-isolated agent.
    // If host HOME is unset (rare service contexts), fall back to the
    // workspace so the env still has a valid HOME.
    let env_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.clone());

    Ok(SandboxProfile {
        env: SandboxEnv::filtered_from_current(&env_home, session_bus),
        workspace_root,
        path_grants,
        network,
        session_bus,
    })
}

fn resolve_allowed_existing_path(
    path: &str,
    grants: &[ResolvedGrant],
    require_write: bool,
) -> Result<PathBuf, String> {
    let candidate = resolve_allowed_path(path, grants, require_write)?;
    if !candidate.exists() {
        return Err(format!("Path does not exist: {}", candidate.display()));
    }
    Ok(candidate)
}

fn resolve_allowed_path(
    path: &str,
    grants: &[ResolvedGrant],
    require_write: bool,
) -> Result<PathBuf, String> {
    let candidate = resolve_candidate_path(path, grants)?;

    for grant in grants {
        if candidate.starts_with(&grant.root) {
            if require_write && grant.access != AccessKind::ReadWrite {
                return Err(format!(
                    "Path {} is not writable for this agent",
                    candidate.display()
                ));
            }
            return Ok(candidate);
        }
    }

    Err(format!(
        "Path {} is outside the agent's allowed filesystem grants",
        candidate.display()
    ))
}

fn resolve_candidate_path(path: &str, grants: &[ResolvedGrant]) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    if raw.is_absolute() {
        return Ok(normalize_path(raw.to_path_buf()));
    }

    if let Some(base) = grants.first() {
        return Ok(normalize_path(base.root.join(raw)));
    }

    Err("No filesystem grants are configured for this agent".to_string())
}

fn resolve_configured_path(path: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    if raw.is_absolute() {
        Ok(normalize_path(raw.to_path_buf()))
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to resolve current directory: {}", e))?;
        Ok(normalize_path(cwd.join(raw)))
    }
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

fn classify_file_type(file_type: &fs::FileType) -> &'static str {
    if file_type.is_dir() {
        "directory"
    } else if file_type.is_file() {
        "file"
    } else if file_type.is_symlink() {
        "symlink"
    } else {
        "other"
    }
}

fn describe_path(path: &Path) -> Result<FilesystemEntry, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("Failed to inspect {}: {}", path.display(), e))?;
    Ok(FilesystemEntry {
        path: path.to_path_buf(),
        kind: classify_file_type(&metadata.file_type()),
    })
}

fn serialize_entries(entries: &[FilesystemEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry.path.display().to_string(),
                "kind": entry.kind
            })
        })
        .collect()
}

fn normalize_pattern_string(pattern: &str) -> String {
    pattern.trim().replace('\\', "/")
}

fn literal_path_prefix(pattern: &str) -> Option<PathBuf> {
    let normalized = normalize_pattern_string(pattern);
    let is_absolute = normalized.starts_with('/');
    let mut prefix = if is_absolute {
        PathBuf::from(std::path::MAIN_SEPARATOR_STR)
    } else {
        PathBuf::new()
    };
    let mut saw_literal_segment = false;

    for segment in normalized.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment_contains_glob(segment) {
            break;
        }
        prefix.push(segment);
        saw_literal_segment = true;
    }

    if is_absolute || saw_literal_segment {
        Some(prefix)
    } else {
        None
    }
}

fn segment_contains_glob(segment: &str) -> bool {
    segment.contains('*') || segment.contains('?') || segment.contains('[') || segment.contains('{')
}

fn path_prefix_intersects(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn path_to_match_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Why a command was denied. Retained for the back-compat
/// [`enforce_command_policy`] wrapper used by unit tests; new code paths
/// branch on [`PolicyResult`] directly so they can surface per-segment
/// info to the approval modal.
#[allow(dead_code)]
enum CommandDenial {
    /// The command prefix is in the explicit block list — expected, no notice needed.
    ExplicitlyBlocked(String),
    /// The command prefix is not in the (non-empty) allow list — the user may not
    /// have anticipated this, so a run notice is warranted.
    NotInAllowList(String),
}

#[allow(dead_code)]
impl CommandDenial {
    fn message(&self) -> &str {
        match self {
            CommandDenial::ExplicitlyBlocked(msg) | CommandDenial::NotInAllowList(msg) => msg,
        }
    }
}

/// Richer per-segment policy evaluation result. Distinguishes a hard
/// block (any segment matched the blocklist) from "needs approval"
/// (some segments aren't in the allowlist but aren't blocked either).
/// The approval flow in `execute_bash_exec` uses this to decide whether
/// to deny outright or raise an interactive prompt.
pub(crate) enum PolicyResult {
    /// All segments pass.
    Allow,
    /// One or more segments hit a blocklist entry. Whole command denied.
    Block {
        segment_text: String,
        matched_prefix: String,
    },
    /// Segments that need user approval (Simple-not-in-allowlist or any
    /// Opaque in Restricted mode). Each carries the smart-prefix
    /// suggestion the modal pre-fills.
    NeedsApproval(Vec<crate::commands::permissions::SegmentApproval>),
}

pub(crate) fn evaluate_command_policy(
    execution: &ExecutionCapabilityConfig,
    command: &str,
) -> PolicyResult {
    use crate::assistant::tools::command_splitter::{split_command, Segment};
    use crate::assistant::tools::prefix_detector::suggest_prefix;
    use crate::commands::permissions::{SegmentApproval, SegmentKind};

    let segments = split_command(command);
    if segments.is_empty() {
        return PolicyResult::Block {
            segment_text: String::new(),
            matched_prefix: "<empty>".to_string(),
        };
    }

    let mut approvals: Vec<SegmentApproval> = Vec::new();

    for segment in &segments {
        let text = segment.text();

        // Blocklist applies to every segment, Simple or Opaque.
        if let Some(matched) = find_matching_prefix(&execution.shell.blocked_command_prefixes, text)
        {
            return PolicyResult::Block {
                segment_text: text.to_string(),
                matched_prefix: matched,
            };
        }

        // Allowlist check only matters in Restricted mode.
        if !matches!(execution.shell.mode, ShellAccessMode::Restricted) {
            continue;
        }

        match segment {
            Segment::Opaque(_) => {
                // Opaque segments can never be allowlisted: substitutions,
                // executor heads, redirects, control flow. Always require
                // fresh approval. No suggested prefix (can't be saved).
                approvals.push(SegmentApproval {
                    text: text.to_string(),
                    kind: SegmentKind::Opaque,
                    suggested_prefix: String::new(),
                });
            }
            Segment::Simple(_) => {
                if find_matching_prefix(&execution.shell.allowed_command_prefixes, text).is_none() {
                    approvals.push(SegmentApproval {
                        text: text.to_string(),
                        kind: SegmentKind::Simple,
                        suggested_prefix: suggest_prefix(text),
                    });
                }
            }
        }
    }

    if approvals.is_empty() {
        PolicyResult::Allow
    } else {
        PolicyResult::NeedsApproval(approvals)
    }
}

/// Runs the interactive approval round-trip for a command that needs
/// user input. Registers the request in app state, emits the request
/// and attention events, waits (with timeout) for the user's decisions,
/// and resolves to `Ok(())` if every segment was allowed or `Err(_)` on
/// any deny / timeout / channel close. Persistence of "always" grants
/// is performed by [`crate::commands::permissions::submit_permission_decision`]
/// before the oneshot is fired, so the grant is durable across crashes
/// between user click and command execution.
async fn await_user_permission(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    command: &str,
    segments: Vec<crate::commands::permissions::SegmentApproval>,
) -> Result<(), String> {
    use crate::commands::permissions::{
        emit_attention, PermissionRequest, APPROVAL_TIMEOUT, PERMISSION_REQUEST_EVENT,
    };
    use tauri::{Emitter, Manager};

    let app_state = deps.app.state::<crate::AppState>();
    let workspace_id = context.workspace_id.clone();
    let agent_id = context.automation_id.clone();

    let request = PermissionRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        workspace_id: workspace_id.clone(),
        agent_id,
        // Agent display name isn't on the runtime context; the frontend
        // resolves it from agent_id via existing workspace queries.
        agent_name: None,
        command: command.to_string(),
        segments,
    };
    let request_id = request.request_id.clone();

    let (rx, count) = app_state.pending_approvals.register(request.clone()).await;

    if let Err(e) = deps.app.emit(PERMISSION_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit permission request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    let decisions = match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
        Ok(Ok(d)) => d,
        Ok(Err(_)) => {
            let msg = "Permission approval channel closed before a decision was made";
            context.add_notice(RunNoticeKind::CommandDenied, msg.to_string());
            return Err(msg.to_string());
        }
        Err(_) => {
            // Timeout — clean the pending entry and emit attention so the
            // fleet card clears its badge.
            if let Some((_, remaining)) = app_state.pending_approvals.take(&request_id).await {
                emit_attention(&deps.app, workspace_id.clone(), remaining);
            }
            let msg = "Permission approval timed out (24h)";
            context.add_notice(RunNoticeKind::CommandDenied, msg.to_string());
            return Err(msg.to_string());
        }
    };

    if decisions.iter().any(|d| !d.is_allow()) {
        let msg = "User denied one or more command segments";
        context.add_notice(RunNoticeKind::CommandDenied, msg.to_string());
        return Err(msg.to_string());
    }
    Ok(())
}

async fn execute_fs_request_grant(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    params: FsRequestGrantParams,
) -> Result<serde_json::Value, String> {
    let canonical = canonicalize_requested_path(&params.path)?;
    let canonical_str = canonical.to_string_lossy().into_owned();

    // If the path is already covered (by extra_paths, the preset, or an
    // earlier session grant), short-circuit — no user prompt, just say yes.
    // This keeps repeated requests cheap and avoids modal spam when the LLM
    // forgets it already has the grant.
    let existing_grants = filesystem_grants(context)?;
    if path_already_covered(&existing_grants, &canonical, params.access) {
        return Ok(serde_json::json!({
            "granted": true,
            "path": canonical_str,
            "access": access_to_str(params.access),
            "scope": "already-granted",
        }));
    }

    let request = crate::commands::path_grants::PathGrantRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        workspace_id: context.workspace_id.clone(),
        agent_id: context.automation_id.clone(),
        agent_name: None,
        requested_path: canonical_str.clone(),
        requested_access: params.access,
        reason: params.reason.clone(),
    };

    let decision = await_path_grant_decision(deps, context, request).await?;

    match decision {
        crate::commands::path_grants::PathGrantDecision::Deny => {
            let msg = format!("User denied path grant for `{}`", canonical_str);
            context.add_notice(RunNoticeKind::PathGrantDenied, msg.clone());
            Err(msg)
        }
        crate::commands::path_grants::PathGrantDecision::AllowOnce { path, access } => {
            apply_session_grant(context, &path, access);
            let msg = format!(
                "User granted `{}` ({}) for this run",
                path,
                access_to_str(access)
            );
            context.add_notice(RunNoticeKind::PathGranted, msg);
            Ok(serde_json::json!({
                "granted": true,
                "path": path,
                "access": access_to_str(access),
                "scope": "once",
            }))
        }
        crate::commands::path_grants::PathGrantDecision::AllowAlways {
            path,
            access,
            scope: _,
        } => {
            // The submit handler already persisted to the agent's DB row;
            // we only need to make the grant visible in the *current* run.
            apply_session_grant(context, &path, access);
            let msg = format!(
                "User granted `{}` ({}) and persisted to agent settings",
                path,
                access_to_str(access)
            );
            context.add_notice(RunNoticeKind::PathGranted, msg);
            Ok(serde_json::json!({
                "granted": true,
                "path": path,
                "access": access_to_str(access),
                "scope": "always",
            }))
        }
    }
}

fn canonicalize_requested_path(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("fs_request_grant requires a non-empty path".to_string());
    }
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| "Cannot expand ~ in path: HOME is unset".to_string())?;
        PathBuf::from(home).join(rest)
    } else if trimmed == "~" {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| "Cannot expand ~ in path: HOME is unset".to_string())?;
        PathBuf::from(home)
    } else {
        PathBuf::from(trimmed)
    };
    if !expanded.is_absolute() {
        return Err(format!(
            "fs_request_grant requires an absolute or ~-prefixed path; got `{}`",
            input
        ));
    }
    // Try to canonicalize (resolve symlinks). If the path doesn't exist
    // yet, fall back to the normalized form — the user can still grant
    // access to a not-yet-existing path (e.g. a future cache dir).
    Ok(std::fs::canonicalize(&expanded).unwrap_or_else(|_| normalize_path(expanded)))
}

fn path_already_covered(
    grants: &[ResolvedGrant],
    path: &Path,
    required: FilesystemPathAccess,
) -> bool {
    grants.iter().any(|grant| {
        let covers_path = path == grant.root || path.starts_with(&grant.root);
        if !covers_path {
            return false;
        }
        match (grant.access, required) {
            (AccessKind::ReadWrite, _) => true,
            (AccessKind::ReadOnly, FilesystemPathAccess::ReadOnly) => true,
            (AccessKind::ReadOnly, FilesystemPathAccess::ReadWrite) => false,
        }
    })
}

fn apply_session_grant(context: &ToolExecutionContext, path: &str, access: FilesystemPathAccess) {
    context.add_session_grant(FilesystemPathGrant {
        path: path.to_string(),
        access,
        origin: Some(GrantOrigin::Approval {
            reason: String::new(),
            granted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        }),
    });
}

fn access_to_str(access: FilesystemPathAccess) -> &'static str {
    match access {
        FilesystemPathAccess::ReadOnly => "read_only",
        FilesystemPathAccess::ReadWrite => "read_write",
    }
}

/// Awaits the user's decision on a path-grant request. Mirrors
/// [`await_user_permission`] for shape; the only differences are the
/// request type, the registry it talks to, and the single-decision
/// return shape (path grants aren't per-segment).
async fn await_path_grant_decision(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    request: crate::commands::path_grants::PathGrantRequest,
) -> Result<crate::commands::path_grants::PathGrantDecision, String> {
    use crate::commands::path_grants::{
        emit_attention, PATH_GRANT_REQUEST_EVENT, PATH_GRANT_TIMEOUT,
    };
    use tauri::{Emitter, Manager};

    let app_state = deps.app.state::<crate::AppState>();
    let workspace_id = context.workspace_id.clone();
    let request_id = request.request_id.clone();

    let (rx, count) = app_state
        .pending_path_grants
        .register(request.clone())
        .await;

    if let Err(e) = deps.app.emit(PATH_GRANT_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit path-grant request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    match tokio::time::timeout(PATH_GRANT_TIMEOUT, rx).await {
        Ok(Ok(decision)) => Ok(decision),
        Ok(Err(_)) => {
            let msg = "Path-grant approval channel closed before a decision was made".to_string();
            context.add_notice(RunNoticeKind::PathGrantDenied, msg.clone());
            Err(msg)
        }
        Err(_) => {
            if let Some((_, remaining)) = app_state.pending_path_grants.take(&request_id).await {
                emit_attention(&deps.app, workspace_id.clone(), remaining);
            }
            let msg = "Path-grant approval timed out (24h)".to_string();
            context.add_notice(RunNoticeKind::PathGrantDenied, msg.clone());
            Err(msg)
        }
    }
}

/// Back-compat wrapper around [`evaluate_command_policy`]. Maps the
/// richer result to the historic `Result<(), CommandDenial>` shape used
/// by existing tests and by the legacy "silent deny" code path when the
/// approval round-trip is unavailable (e.g., during shutdown).
#[allow(dead_code)]
fn enforce_command_policy(
    execution: &ExecutionCapabilityConfig,
    _workspace_root: Option<&std::path::Path>,
    command: &str,
) -> Result<(), CommandDenial> {
    match evaluate_command_policy(execution, command) {
        PolicyResult::Allow => Ok(()),
        PolicyResult::Block {
            segment_text,
            matched_prefix,
        } => {
            if segment_text.is_empty() {
                Err(CommandDenial::ExplicitlyBlocked(
                    "Shell command cannot be empty".to_string(),
                ))
            } else {
                Err(CommandDenial::ExplicitlyBlocked(format!(
                    "Command `{}` is blocked for this agent (matched prefix `{}`)",
                    segment_text, matched_prefix
                )))
            }
        }
        PolicyResult::NeedsApproval(segs) => {
            let first = segs.into_iter().next().unwrap();
            let short = command_preview(&first.text);
            Err(CommandDenial::NotInAllowList(format!(
                "Command `{}` is not in the allowed list. Add `{}` to allow it.",
                short, short
            )))
        }
    }
}

/// Extract the first two words of a command for display (e.g. "kubectl get").
fn command_preview(command: &str) -> String {
    let mut words = command.split_whitespace();
    match (words.next(), words.next()) {
        (Some(cmd), Some(sub)) if !sub.starts_with('-') => format!("{} {}", cmd, sub),
        (Some(cmd), _) => cmd.to_string(),
        _ => command.to_string(),
    }
}

/// Word-boundary prefix match for command-policy lists. Returns the first
/// matching prefix on a hit.
///
/// Matching rules:
///   - prefix "kubectl get" matches command "kubectl get pods"
///   - prefix "kubectl" matches command "kubectl delete pods"
///   - prefix "kubectl get" does NOT match command "kubectl delete pods"
fn find_matching_prefix(prefixes: &[String], command: &str) -> Option<String> {
    prefixes
        .iter()
        .find(|p| matches_prefix(p, command))
        .cloned()
}

fn matches_prefix(prefix: &str, command: &str) -> bool {
    let p = prefix.trim();
    if p.is_empty() {
        return false;
    }
    command == p || (command.starts_with(p) && command.as_bytes().get(p.len()) == Some(&b' '))
}

// =============================================================================
// Web tools
// =============================================================================

async fn execute_web_search(params: WebSearchParams) -> Result<serde_json::Value, String> {
    let max_results = params
        .max_results
        .unwrap_or(DEFAULT_WEB_SEARCH_MAX_RESULTS)
        .min(MAX_WEB_SEARCH_MAX_RESULTS);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(WEB_SEARCH_TIMEOUT_MS))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let response = client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", &params.query)])
        .header("User-Agent", "CLAI/1.0")
        .send()
        .await
        .map_err(|e| format!("Web search request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Web search returned HTTP {}",
            response.status().as_u16()
        ));
    }

    let html = response
        .text()
        .await
        .map_err(|e| format!("Failed to read search response: {}", e))?;

    let results = parse_duckduckgo_results(&html, max_results);

    Ok(serde_json::json!({
        "query": params.query,
        "results": results,
        "resultCount": results.len()
    }))
}

fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<serde_json::Value> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let result_selector = Selector::parse(".result").unwrap();
    let link_selector = Selector::parse(".result__a").unwrap();
    let snippet_selector = Selector::parse(".result__snippet").unwrap();

    let mut results = Vec::new();

    for element in document.select(&result_selector) {
        if results.len() >= max_results {
            break;
        }

        let link = match element.select(&link_selector).next() {
            Some(a) => a,
            None => continue,
        };

        let title = link.text().collect::<String>().trim().to_string();
        if title.is_empty() {
            continue;
        }

        // DuckDuckGo HTML wraps URLs through a redirect; extract the actual URL
        let href = link.value().attr("href").unwrap_or_default();
        let url = extract_ddg_url(href);
        if url.is_empty() {
            continue;
        }

        let snippet = element
            .select(&snippet_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(serde_json::json!({
            "title": title,
            "url": url,
            "snippet": snippet
        }));
    }

    results
}

/// Extract the actual URL from DuckDuckGo's redirect wrapper.
/// DDG HTML results use hrefs like `//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=...`
fn extract_ddg_url(href: &str) -> String {
    if let Some(pos) = href.find("uddg=") {
        let encoded = &href[pos + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        urldecode(encoded)
    } else if href.starts_with("http") {
        href.to_string()
    } else {
        String::new()
    }
}

/// Minimal percent-decoding for URL extraction.
fn urldecode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                result.push((h << 4 | l) as char);
            } else {
                result.push('%');
                result.push(hi as char);
                result.push(lo as char);
            }
        } else if b == b'+' {
            result.push(' ');
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

async fn execute_web_fetch(params: WebFetchParams) -> Result<serde_json::Value, String> {
    let content_limit = params
        .max_content_chars
        .unwrap_or(DEFAULT_WEB_FETCH_CONTENT_LIMIT)
        .min(MAX_WEB_FETCH_CONTENT_LIMIT);
    let timeout_ms = params
        .timeout_ms
        .unwrap_or(DEFAULT_WEB_FETCH_TIMEOUT_MS)
        .min(MAX_WEB_FETCH_TIMEOUT_MS);

    // Basic URL validation
    let url = params.url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL must start with http:// or https://".to_string());
    }

    // Block private/local IPs to prevent SSRF
    if is_private_url(url) {
        return Err("Fetching private/local URLs is not allowed".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let response = client
        .get(url)
        .header("User-Agent", "CLAI/1.0")
        .header("Accept", "text/html, text/plain, application/xhtml+xml")
        .send()
        .await
        .map_err(|e| format!("Fetch failed: {}", e))?;

    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(format!("Fetch returned HTTP {}", status));
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    // Convert HTML to markdown, or return plain text as-is
    let markdown = if content_type.contains("text/html") || content_type.contains("xhtml") {
        html_to_markdown(&body)
    } else {
        body
    };

    let truncated = markdown.len() > content_limit;
    let content = if truncated {
        let chars: Vec<char> = markdown.chars().collect();
        let end = content_limit.min(chars.len());
        chars[..end].iter().collect::<String>()
    } else {
        markdown
    };

    Ok(serde_json::json!({
        "url": url,
        "contentType": content_type,
        "content": content,
        "truncated": truncated,
        "contentLength": content.len()
    }))
}

fn html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| {
        // Fallback: strip tags with scraper and return plain text
        let document = scraper::Html::parse_document(html);
        document.root_element().text().collect::<String>()
    })
}

/// Check if a URL points to a private/local address (SSRF protection).
fn is_private_url(url: &str) -> bool {
    // Extract host from URL
    let host = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");

    matches!(
        host,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]"
    ) || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("172.16.")
        || host.starts_with("172.17.")
        || host.starts_with("172.18.")
        || host.starts_with("172.19.")
        || host.starts_with("172.20.")
        || host.starts_with("172.21.")
        || host.starts_with("172.22.")
        || host.starts_with("172.23.")
        || host.starts_with("172.24.")
        || host.starts_with("172.25.")
        || host.starts_with("172.26.")
        || host.starts_with("172.27.")
        || host.starts_with("172.28.")
        || host.starts_with("172.29.")
        || host.starts_with("172.30.")
        || host.starts_with("172.31.")
        || host.ends_with(".local")
        || host.ends_with(".internal")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn grant_for(path: &Path) -> ResolvedGrant {
        ResolvedGrant {
            root: path.to_path_buf(),
            access: AccessKind::ReadWrite,
        }
    }

    // ------------------------------------------------------------------
    // path_already_covered short-circuit predicate
    // ------------------------------------------------------------------

    #[test]
    fn path_already_covered_recognises_exact_match() {
        let grants = vec![ResolvedGrant {
            root: PathBuf::from("/a/b"),
            access: AccessKind::ReadOnly,
        }];
        assert!(path_already_covered(
            &grants,
            Path::new("/a/b"),
            FilesystemPathAccess::ReadOnly
        ));
    }

    #[test]
    fn path_already_covered_recognises_descendant() {
        let grants = vec![ResolvedGrant {
            root: PathBuf::from("/a"),
            access: AccessKind::ReadOnly,
        }];
        assert!(path_already_covered(
            &grants,
            Path::new("/a/b/c"),
            FilesystemPathAccess::ReadOnly
        ));
    }

    #[test]
    fn path_already_covered_requires_rw_for_rw_request() {
        let grants = vec![ResolvedGrant {
            root: PathBuf::from("/a"),
            access: AccessKind::ReadOnly,
        }];
        assert!(!path_already_covered(
            &grants,
            Path::new("/a/b"),
            FilesystemPathAccess::ReadWrite
        ));
    }

    #[test]
    fn path_already_covered_rw_grant_satisfies_ro_request() {
        let grants = vec![ResolvedGrant {
            root: PathBuf::from("/a"),
            access: AccessKind::ReadWrite,
        }];
        assert!(path_already_covered(
            &grants,
            Path::new("/a/b"),
            FilesystemPathAccess::ReadOnly
        ));
    }

    // ------------------------------------------------------------------
    // canonicalize_requested_path
    // ------------------------------------------------------------------

    #[test]
    fn canonicalize_rejects_empty_path() {
        let err = canonicalize_requested_path("   ").unwrap_err();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn canonicalize_rejects_relative_paths() {
        let err = canonicalize_requested_path("relative/path").unwrap_err();
        assert!(err.contains("absolute"));
    }

    #[test]
    fn canonicalize_expands_tilde_with_real_home() {
        let temp = tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", temp.path());
        }

        let resolved = canonicalize_requested_path("~/some/subpath").unwrap();
        // Use canonicalize on the temp dir for comparison since macOS
        // symlinks /var → /private/var which would otherwise mismatch.
        let expected_parent = std::fs::canonicalize(temp.path()).unwrap();
        assert!(resolved.starts_with(&expected_parent));
        assert!(resolved.ends_with("some/subpath"));

        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn fs_list_returns_sorted_entries_and_recursive_children() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("b.txt"), "b").unwrap();
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::create_dir(root.join("notes")).unwrap();
        fs::write(root.join("notes").join("todo.md"), "todo").unwrap();

        let (top_level, truncated) = list_entries_at_path(root, false, 10).unwrap();
        let top_level_paths: Vec<String> = top_level
            .iter()
            .map(|entry| {
                entry
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(top_level_paths, vec!["a.txt", "b.txt", "notes"]);
        assert!(!truncated);

        let (recursive, recursive_truncated) = list_entries_at_path(root, true, 10).unwrap();
        let recursive_paths: Vec<String> = recursive
            .iter()
            .map(|entry| {
                entry
                    .path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(
            recursive_paths,
            vec!["a.txt", "b.txt", "notes", "notes/todo.md"]
        );
        assert!(!recursive_truncated);
    }

    #[test]
    fn fs_glob_matches_relative_patterns_within_allowed_grants() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".clai").join("memory").join("checkpoints")).unwrap();
        fs::write(root.join(".clai").join("memory").join("status.md"), "ok").unwrap();
        fs::write(root.join(".clai").join("memory").join("facts.json"), "{}").unwrap();
        fs::write(
            root.join(".clai")
                .join("memory")
                .join("checkpoints")
                .join("restart.md"),
            "resume",
        )
        .unwrap();

        let (matches, truncated) =
            glob_allowed_paths(".clai/memory/**/*.md", &[grant_for(root)], 10).unwrap();
        let matched_paths: Vec<String> = matches
            .iter()
            .map(|entry| {
                entry
                    .path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(
            matched_paths,
            vec![
                ".clai/memory/checkpoints/restart.md",
                ".clai/memory/status.md"
            ]
        );
        assert!(!truncated);
    }

    #[test]
    fn fs_glob_rejects_absolute_patterns_outside_allowed_grants() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        let error = glob_allowed_paths("/etc/**/*.conf", &[grant_for(root)], 10).unwrap_err();

        assert!(error.contains("outside the agent's allowed filesystem grants"));
    }

    #[test]
    fn extract_ddg_url_decodes_redirect_wrapper() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc123";
        assert_eq!(extract_ddg_url(href), "https://example.com/page");
    }

    #[test]
    fn extract_ddg_url_passes_through_direct_urls() {
        assert_eq!(
            extract_ddg_url("https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn extract_ddg_url_returns_empty_for_unknown_format() {
        assert_eq!(extract_ddg_url("/some/relative/path"), "");
    }

    #[test]
    fn urldecode_handles_percent_encoding() {
        assert_eq!(urldecode("hello%20world"), "hello world");
        assert_eq!(
            urldecode("https%3A%2F%2Fexample.com"),
            "https://example.com"
        );
        assert_eq!(urldecode("a+b"), "a b");
        assert_eq!(urldecode("plain"), "plain");
    }

    #[test]
    fn is_private_url_blocks_local_addresses() {
        assert!(is_private_url("http://localhost/foo"));
        assert!(is_private_url("http://127.0.0.1:8080/api"));
        assert!(is_private_url("http://192.168.1.1/"));
        assert!(is_private_url("http://10.0.0.1/"));
        assert!(is_private_url("http://172.16.0.1/"));
        assert!(is_private_url("http://myhost.local/"));
        assert!(is_private_url("http://service.internal/"));
    }

    #[test]
    fn is_private_url_allows_public_addresses() {
        assert!(!is_private_url("https://example.com/"));
        assert!(!is_private_url("https://docs.rust-lang.org/"));
        assert!(!is_private_url("http://8.8.8.8/"));
    }

    #[test]
    fn parse_duckduckgo_results_extracts_from_html() {
        let html = r#"
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org&rut=x">Rust Language</a>
            <a class="result__snippet">A systems programming language</a>
        </div>
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs&rut=y">Docs.rs</a>
            <a class="result__snippet">Rust package documentation</a>
        </div>
        "#;

        let results = parse_duckduckgo_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "Rust Language");
        assert_eq!(results[0]["url"], "https://rust-lang.org");
        assert_eq!(results[1]["title"], "Docs.rs");
    }

    #[test]
    fn parse_duckduckgo_results_respects_max_results() {
        let html = r#"
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com&rut=x">A</a>
        </div>
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fb.com&rut=y">B</a>
        </div>
        "#;

        let results = parse_duckduckgo_results(html, 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn html_to_markdown_converts_basic_html() {
        let html = "<html><body><h1>Title</h1><p>Hello <strong>world</strong></p></body></html>";
        let md = html_to_markdown(html);
        assert!(md.contains("Title"));
        assert!(md.contains("**world**"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_non_http_urls() {
        let params = WebFetchParams {
            url: "ftp://example.com/file".to_string(),
            max_content_chars: None,
            timeout_ms: None,
        };
        let result = execute_web_fetch(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("http:// or https://"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_private_urls() {
        let params = WebFetchParams {
            url: "http://192.168.1.1/admin".to_string(),
            max_content_chars: None,
            timeout_ms: None,
        };
        let result = execute_web_fetch(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("private/local"));
    }

    // ------------------------------------------------------------------
    // enforce_command_policy — agent policy wiring
    // ------------------------------------------------------------------

    use crate::config::types::ShellCapabilityConfig;
    use crate::config::{ExecutionCapabilityConfig, ShellAccessMode};

    fn restricted_execution_config(
        allowed: &[&str],
        blocked: &[&str],
    ) -> ExecutionCapabilityConfig {
        ExecutionCapabilityConfig {
            shell: ShellCapabilityConfig {
                mode: ShellAccessMode::Restricted,
                allowed_command_prefixes: allowed.iter().map(|s| s.to_string()).collect(),
                blocked_command_prefixes: blocked.iter().map(|s| s.to_string()).collect(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn policy_blocks_using_agent_blocklist() {
        let exec = restricted_execution_config(&[], &["rm"]);
        let err = enforce_command_policy(&exec, None, "rm -rf /tmp").unwrap_err();
        assert!(matches!(err, CommandDenial::ExplicitlyBlocked(_)));
    }

    #[test]
    fn policy_allows_using_agent_allowlist_even_without_workspace_root() {
        let exec = restricted_execution_config(&["git status"], &[]);
        assert!(enforce_command_policy(&exec, None, "git status -s").is_ok());
    }

    #[test]
    fn policy_workspace_root_is_not_a_policy_source() {
        // Legacy `.clai/permissions.json` files are no longer part of command
        // policy. The root argument is accepted only for the old test wrapper.
        let temp = tempdir().unwrap();
        let exec = restricted_execution_config(&["git"], &[]);
        assert!(enforce_command_policy(&exec, Some(temp.path()), "git status").is_ok());
    }

    #[test]
    fn policy_restricted_with_no_allow_anywhere_denies() {
        let temp = tempdir().unwrap();
        let exec = restricted_execution_config(&[], &[]);
        let err = enforce_command_policy(&exec, Some(temp.path()), "git status").unwrap_err();
        assert!(matches!(err, CommandDenial::NotInAllowList(_)));
    }

    // ------------------------------------------------------------------
    // Per-segment evaluation
    // ------------------------------------------------------------------

    #[test]
    fn policy_pipeline_passes_when_all_segments_allowed() {
        let exec = restricted_execution_config(&["git log", "head", "wc"], &[]);
        assert!(enforce_command_policy(&exec, None, "git log | head | wc -l").is_ok());
    }

    #[test]
    fn policy_pipeline_denies_when_any_segment_unmatched() {
        // `git log` is allowed, but `obscure-tool` isn't.
        let exec = restricted_execution_config(&["git log"], &[]);
        let err = enforce_command_policy(&exec, None, "git log | obscure-tool").unwrap_err();
        assert!(matches!(err, CommandDenial::NotInAllowList(_)));
        // Denial should specifically reference the unapproved segment.
        let msg = err.message();
        assert!(
            msg.contains("obscure-tool"),
            "denial msg should mention the offending segment, got: {msg}"
        );
    }

    #[test]
    fn policy_pipeline_block_in_one_segment_denies_whole() {
        // Allow git, block rm — the rm segment denies even when git log is fine.
        let exec = restricted_execution_config(&["git log"], &["rm"]);
        let err = enforce_command_policy(&exec, None, "git log | rm -rf /tmp/foo").unwrap_err();
        assert!(matches!(err, CommandDenial::ExplicitlyBlocked(_)));
    }

    #[test]
    fn policy_closes_pipe_bypass_foot_gun() {
        // The motivating bug: pre-segment matcher would word-boundary-match
        // `git log` as a prefix of the whole pipeline and silently approve
        // arbitrary downstream commands. Per-segment evaluation now treats
        // the second segment as its own command — and `rm -rf` (with no
        // allow entry) denies the pipeline.
        let exec = restricted_execution_config(&["git log"], &[]);
        assert!(enforce_command_policy(&exec, None, "git log | rm -rf ~/").is_err());
    }

    #[test]
    fn policy_opaque_segment_in_restricted_always_denies_until_approval_flow() {
        // `bash -c "..."` is Opaque. Even if a permissive prefix appears to
        // cover it, Opaque can't be allowlisted in the new design.
        let exec = restricted_execution_config(&["bash"], &[]);
        let err = enforce_command_policy(&exec, None, r#"bash -c "echo hi""#).unwrap_err();
        assert!(matches!(err, CommandDenial::NotInAllowList(_)));
    }

    #[test]
    fn policy_opaque_segment_in_full_mode_passes() {
        // Full mode skips the allowlist check, so Opaque segments only
        // fail if they hit a blocklist entry.
        let exec = ExecutionCapabilityConfig {
            shell: ShellCapabilityConfig {
                mode: ShellAccessMode::Full,
                allowed_command_prefixes: vec![],
                blocked_command_prefixes: vec![],
            },
            ..Default::default()
        };
        assert!(enforce_command_policy(&exec, None, r#"bash -c "echo hi""#).is_ok());
    }

    #[test]
    fn policy_mixed_pipeline_with_opaque_segment_denies_in_restricted() {
        // `find . | xargs rm` — xargs is Opaque. Even if `find .` is allowed,
        // the xargs Opaque segment in Restricted mode forces denial.
        let exec = restricted_execution_config(&["find", "xargs"], &[]);
        let err = enforce_command_policy(&exec, None, "find . | xargs rm").unwrap_err();
        assert!(matches!(err, CommandDenial::NotInAllowList(_)));
    }

    #[test]
    fn policy_redirect_segment_in_restricted_denies() {
        // Redirects mark the segment Opaque (can't safely allowlist).
        let exec = restricted_execution_config(&["cat"], &[]);
        let err = enforce_command_policy(&exec, None, "cat /etc/hosts > /tmp/x").unwrap_err();
        assert!(matches!(err, CommandDenial::NotInAllowList(_)));
    }

    #[test]
    fn policy_empty_command_still_errors() {
        let exec = restricted_execution_config(&[], &[]);
        let err = enforce_command_policy(&exec, None, "   ").unwrap_err();
        assert!(matches!(err, CommandDenial::ExplicitlyBlocked(_)));
    }
}

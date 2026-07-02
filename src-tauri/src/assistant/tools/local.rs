use futures::StreamExt;
use glob::{MatchOptions, Pattern};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

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
const DEFAULT_BASH_TIMEOUT_MS: u64 = 300_000;
const MAX_BASH_TIMEOUT_MS: u64 = 1_800_000;
const DEFAULT_BASH_OUTPUT_LIMIT: usize = 20_000;
const MAX_BASH_OUTPUT_LIMIT: usize = 200_000;
const DEFAULT_WEB_FETCH_CONTENT_LIMIT: usize = 20_000;
const MAX_WEB_FETCH_CONTENT_LIMIT: usize = 100_000;
const MAX_WEB_FETCH_BODY_BYTES: usize = MAX_WEB_FETCH_CONTENT_LIMIT * 4;
const DEFAULT_WEB_FETCH_TIMEOUT_MS: u64 = 15_000;
const MAX_WEB_FETCH_TIMEOUT_MS: u64 = 30_000;
const MAX_WEB_FETCH_REDIRECTS: usize = 5;
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
        "path": agent_path_string(&path),
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
    // Only search grant roots the in-process tools can actually reach: in
    // Flatpak, non-home roots are invisible or a divergent private view, so
    // walking them yields misleading results. No-op outside Flatpak.
    let grants = retain_flatpak_reachable_grants(filesystem_grants(context)?);
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
        "path": agent_path_string(&path),
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
    ensure_fs_reachable(&path)?;

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
        "path": agent_path_string(&path),
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

    let run_allowed = context.session_allowed_command_prefixes_snapshot();
    let run_blocked = context.session_blocked_command_prefixes_snapshot();
    match evaluate_command_policy(
        &context.execution,
        &params.command,
        &run_allowed,
        &run_blocked,
    ) {
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

    // Whether the model set timeoutMs itself — drives the recovery hint on a
    // timeout ("the requested timeout" vs "the default timeout").
    let explicit_timeout = params.timeout_ms.is_some();
    let timeout_ms = params
        .timeout_ms
        .unwrap_or(DEFAULT_BASH_TIMEOUT_MS)
        .min(MAX_BASH_TIMEOUT_MS);
    let output_limit = params
        .max_output_chars
        .unwrap_or(DEFAULT_BASH_OUTPUT_LIMIT)
        .min(MAX_BASH_OUTPUT_LIMIT);

    // Resolve the shell up front. On Windows this can fail when no POSIX
    // shell is installed; surface that as an actionable notice + error rather
    // than a cryptic spawn failure for a missing `bash`.
    let argv = match super::posix_shell::shell_argv(params.command) {
        Ok(argv) => argv,
        Err(message) => {
            context.add_notice(RunNoticeKind::SandboxUnavailable, message.clone());
            return Err(message);
        }
    };

    let output = run_command(SandboxCommand {
        argv,
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
    })
    .map_err(|error| augment_timeout_error(error, timeout_ms, explicit_timeout))?;

    Ok(serde_json::json!({
        "cwd": agent_path_string(&output.cwd),
        "exitCode": output.exit_code,
        "success": output.success,
        "stdout": output.stdout,
        "stderr": output.stderr
    }))
}

/// Turn a bare sandbox timeout into an actionable error the model can recover
/// from. A plain "timed out after N ms" is a dead end — the caller can't tell
/// that a larger `timeoutMs` exists or what its ceiling is. When the run timed
/// out below the cap, point at `timeoutMs` (distinguishing the default from an
/// explicitly requested value); when it was already at the cap, tell the model
/// to narrow the command instead of waiting longer. Non-timeout errors pass
/// through unchanged.
fn augment_timeout_error(error: String, timeout_ms: u64, explicit_timeout: bool) -> String {
    if !error.contains("timed out after") {
        return error;
    }
    if timeout_ms < MAX_BASH_TIMEOUT_MS {
        let which = if explicit_timeout {
            "requested"
        } else {
            "default"
        };
        format!(
            "{error}. That was the {which} timeout; for long-running work \
             (builds, large test suites) re-run with a higher `timeoutMs` \
             (up to {MAX_BASH_TIMEOUT_MS} ms)."
        )
    } else {
        format!(
            "{error}. That is the maximum `timeoutMs` ({MAX_BASH_TIMEOUT_MS} ms); \
             narrow the command (e.g. a single package or a test subset) so it \
             finishes within the limit."
        )
    }
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
    // In Flatpak, surface the host-only reachability BEFORE the existence
    // probe — otherwise a granted-but-unreachable path (e.g. /tmp, a private
    // tmpfs in Flatpak) fails the in-sandbox exists() check and returns a
    // misleading "Path does not exist" instead of steering to bash_exec.
    ensure_fs_reachable(&candidate)?;
    if !candidate.exists() {
        return Err(format!("Path does not exist: {}", candidate.display()));
    }
    Ok(candidate)
}

/// The user's real home directory, resolved once. Inside Flatpak this is the
/// host home (resolved via a host-spawn), cached so the fs_* path doesn't
/// spawn a process per call.
fn real_host_home() -> Option<PathBuf> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| crate::providers::get_home_dir().map(PathBuf::from))
        .clone()
}

/// True when `resolved` lies outside the home subtree. `home` is `None` when
/// the real home can't be determined, in which case we don't block (fail open).
fn path_outside_home(resolved: &Path, home: Option<&Path>) -> bool {
    match home {
        Some(home) => !resolved.starts_with(home),
        None => false,
    }
}

/// In a Flatpak build the in-process fs_* tools see only what the sandbox's
/// `--filesystem` permission exposes — in practice the user's home directory.
/// `/tmp` is a *private* tmpfs in Flatpak and other roots (`/opt`, `/mnt`, …)
/// aren't mapped at all, so a granted path outside home is either invisible or
/// a different view from what `bash_exec` (which runs host-side) sees. Probing
/// existence can't distinguish "not visible" from "absent" — a private `/tmp`
/// both exists and is empty — so we gate structurally on the home subtree.
fn fs_unreachable_in_flatpak(resolved: &Path, home: Option<&Path>) -> bool {
    crate::providers::is_flatpak() && path_outside_home(resolved, home)
}

/// Fail a single-path fs_* op whose resolved path is granted but unreachable by
/// the in-process tools in Flatpak, steering the agent to `bash_exec` instead
/// of silently lying ("Path does not exist") or writing a phantom file into the
/// sandbox-private view that neither bash_exec nor the host can see.
fn ensure_fs_reachable(resolved: &Path) -> Result<(), String> {
    if fs_unreachable_in_flatpak(resolved, real_host_home().as_deref()) {
        return Err(format!(
            "`{}` is granted but unreachable by fs_* tools in the Flatpak build, which can \
             only see your home directory. Use bash_exec for this path — it runs on the host \
             and can reach every granted path.",
            resolved.display()
        ));
    }
    Ok(())
}

/// Drop grant roots the in-process tools can't reach in Flatpak so a glob only
/// searches what it can actually see, rather than walking a divergent
/// sandbox-private view. No-op outside Flatpak.
fn retain_flatpak_reachable_grants(grants: Vec<ResolvedGrant>) -> Vec<ResolvedGrant> {
    if !crate::providers::is_flatpak() {
        return grants;
    }
    let home = real_host_home();
    grants
        .into_iter()
        .filter(|grant| !path_outside_home(&grant.root, home.as_deref()))
        .collect()
}

fn resolve_allowed_path(
    path: &str,
    grants: &[ResolvedGrant],
    require_write: bool,
) -> Result<PathBuf, String> {
    let candidate = resolve_candidate_path(path, grants)?;

    // Workspace isolation: an agent reaches its own workspace and HOME, but not
    // sibling workspaces — even though they sit under the same HOME grant. The
    // mask is the workspace container (`grants[0]` is always the agent's own
    // workspace root; its parent holds all workspaces). A grant that's only a
    // *broad ancestor* of that container (e.g. `$HOME`) does not authorize a
    // path inside it; only a grant rooted at-or-below the container does (the
    // own workspace, or another workspace the user explicitly granted). See
    // `sandbox::profile::workspace_mask`.
    let mask = grants.first().and_then(|ws| {
        crate::assistant::sandbox::profile::workspace_mask(&ws.root, real_host_home().as_deref())
    });

    // Grants that actually authorize this candidate (broad ancestors of the
    // masked container are dropped for paths inside it).
    let effective: Vec<&ResolvedGrant> = grants
        .iter()
        .filter(|grant| {
            candidate.starts_with(&grant.root)
                && !grant_masked_for_candidate(&grant.root, &candidate, mask.as_deref())
        })
        .collect();

    // Resolve against the MOST SPECIFIC (deepest-rooted) grant that contains the
    // candidate, not merely the first one in iteration order. Grants nest: a
    // broad read-only `/home/me` can coexist with a narrower read-write
    // `/home/me/project` accepted via `fs_request_grant`. The deeper grant is
    // the more precise statement of intent for its subtree and must win —
    // mirroring the last-writer-wins bind ordering in `linux_bwrap`. A
    // first-match scan let a read-only ancestor shadow a read-write descendant
    // and rejected legitimate writes (and vice-versa for read-only carve-outs).
    let deepest = effective
        .iter()
        .map(|grant| grant.root.components().count())
        .max();

    let Some(depth) = deepest else {
        return Err(format!(
            "Path {} is outside the agent's allowed filesystem grants",
            candidate.display()
        ));
    };

    if require_write {
        // Among equally-specific grants (same root, conflicting access), the
        // read-write one wins: an explicit fresh grant must not be shadowed by
        // a coincidental read-only entry at the same root.
        let writable = effective
            .iter()
            .filter(|grant| grant.root.components().count() == depth)
            .any(|grant| grant.access == AccessKind::ReadWrite);
        if !writable {
            return Err(format!(
                "Path {} is not writable for this agent",
                candidate.display()
            ));
        }
    }

    Ok(candidate)
}

/// True when `grant_root` is only a broad *ancestor* of the masked workspace
/// container and `candidate` lies inside that container — i.e. this grant must
/// not authorize the path (it would otherwise expose sibling workspaces via a
/// `$HOME`-style grant). A grant rooted at-or-below the container (the agent's
/// own workspace, or an explicitly-granted sibling) is not blocked.
fn grant_masked_for_candidate(grant_root: &Path, candidate: &Path, mask: Option<&Path>) -> bool {
    match mask {
        Some(mask) => {
            candidate.starts_with(mask) && mask.starts_with(grant_root) && mask != grant_root
        }
        None => false,
    }
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
                "path": agent_path_string(&entry.path),
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

/// Render a path for agent-facing tool output.
///
/// On Windows the agent's `bash_exec` runs inside Git Bash, where `\` is an
/// escape character, so paths handed to the model must use `/` to round-trip
/// back into shell commands. We also strip the `\\?\` verbatim prefix that
/// `std::fs::canonicalize` adds, leaving clean `C:/Users/...` paths that both
/// the Windows file APIs (for `fs_read`/`fs_write` round-trips) and Git Bash
/// accept.
///
/// On Unix the path is returned verbatim: `\` is a legal byte in a filename,
/// so rewriting it would corrupt names.
fn agent_path_string(path: &Path) -> String {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        let trimmed = s.strip_prefix(r"\\?\").unwrap_or(s.as_ref());
        trimmed.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
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
    extra_allowed_prefixes: &[String],
    extra_blocked_prefixes: &[String],
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

        // Blocklist applies to every segment, Simple or Opaque. Durable
        // blocklist is checked first; run-scoped blocklist (this run's
        // accepted `DenyAlways` decisions) is checked next so a fresh
        // deny-always takes effect mid-run.
        if let Some(matched) = find_matching_prefix(&execution.shell.blocked_command_prefixes, text)
            .or_else(|| find_matching_prefix(extra_blocked_prefixes, text))
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

        // Benign shell constructs never need approval. Test expressions
        // (`[ ... ]`, `[[ ... ]]`, `test ...`) only compare strings/files,
        // and segments that are purely variable assignments (`FOO=bar`)
        // just set shell state — neither runs a program of its own. The
        // checked-for carve-out (a command substitution / backtick inside
        // them) is handled in `is_benign_shell_construct`: those DO run
        // code, so they fall through to normal approval. Blocklist was
        // already applied above, so this can't bypass a hard deny.
        if is_benign_shell_construct(text) {
            continue;
        }

        // Run-scoped allowlist holds prefixes accepted this run via
        // `AllowOnce`/`AllowAlways`. Without consulting it, every bash
        // call after a fresh approval would re-prompt mid-run: durable
        // list updates land on the next session, and persistence does
        // not mutate the running execution snapshot.
        let run_match = find_matching_prefix(extra_allowed_prefixes, text);

        match segment {
            Segment::Opaque(_) => {
                // Opaque segments include redirects, heredocs, command
                // substitutions, brace groups, and executor heads. Even
                // though the *whole* segment can't be safely encoded as
                // a literal prefix, the binary head (what `suggest_prefix`
                // returns) IS a stable identifier — the redirect or
                // substitution doesn't change which program runs. We
                // honor a durable allowlist match against that head AND
                // fill in a non-empty `suggested_prefix` so the modal
                // surfaces an "Always allow" button for Opaque rows the
                // same way it does for Simple ones. The user opts into
                // the broader trust explicitly.
                let durable_match =
                    find_matching_prefix(&execution.shell.allowed_command_prefixes, text);
                if durable_match.is_some() || run_match.is_some() {
                    continue;
                }
                approvals.push(SegmentApproval {
                    text: text.to_string(),
                    kind: SegmentKind::Opaque,
                    suggested_prefix: suggest_prefix(text),
                });
            }
            Segment::Simple(_) => {
                let durable_match =
                    find_matching_prefix(&execution.shell.allowed_command_prefixes, text);
                if durable_match.is_none() && run_match.is_none() {
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
        PolicyResult::NeedsApproval(dedup_approvals_by_prefix(approvals))
    }
}

/// Collapses approval rows that share the same non-empty suggested prefix
/// into one. Without this, a command like `cd /a && cd /b && cd /c` asks the
/// user to grant `cd` three times in a single modal. Rows with an empty
/// suggested prefix (no stable allowlist head — e.g. the `:` no-op or an
/// over-collapsed opaque blob) are never merged: each is a distinct
/// allow-once decision and has no prefix to key on. When duplicates disagree
/// on kind, the kept row is marked `Opaque` so the "review carefully"
/// affordance is preserved rather than silently downgraded to `Simple`.
fn dedup_approvals_by_prefix(
    approvals: Vec<crate::commands::permissions::SegmentApproval>,
) -> Vec<crate::commands::permissions::SegmentApproval> {
    use crate::commands::permissions::SegmentKind;
    use std::collections::HashMap;

    let mut result: Vec<crate::commands::permissions::SegmentApproval> =
        Vec::with_capacity(approvals.len());
    let mut index_by_prefix: HashMap<String, usize> = HashMap::new();

    for approval in approvals {
        if approval.suggested_prefix.is_empty() {
            result.push(approval);
            continue;
        }
        if let Some(&idx) = index_by_prefix.get(&approval.suggested_prefix) {
            if matches!(approval.kind, SegmentKind::Opaque) {
                result[idx].kind = SegmentKind::Opaque;
            }
            continue;
        }
        index_by_prefix.insert(approval.suggested_prefix.clone(), result.len());
        result.push(approval);
    }
    result
}

/// True for shell segments that have no execution surface of their own and
/// therefore never need approval:
///
/// - **Test expressions** — `[ ... ]`, `[[ ... ]]`, or the `test` builtin.
///   These only compare strings/files; they spawn no program.
/// - **Pure assignments** — a segment whose every token is `NAME=value`
///   (`FOO=bar`, `A=1 B=2`). These just set shell variables.
///
/// Carve-out: if the segment embeds a command substitution (`$(...)`) or
/// backticks, it DOES run code — `[ -n "$(curl x)" ]` or `V=$(git log)` —
/// so we report it as non-benign and let the normal approval path gate the
/// embedded command. An assignment used as a *prefix* to a real command
/// (`FOO=bar mycmd`) is likewise not benign: `mycmd` is a token that isn't
/// an assignment, so the all-assignments check fails and we fall through.
fn is_benign_shell_construct(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    // A command substitution or backtick runs arbitrary code — defer to
    // normal approval so the embedded command is gated.
    if trimmed.contains("$(") || trimmed.contains('`') {
        return false;
    }

    let mut tokens = trimmed.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };

    // Test expressions: the head token is the `test` builtin in one of its
    // three spellings. Contents are side-effect-free (we already ruled out
    // embedded substitutions above).
    if first == "[" || first == "[[" || first == "test" {
        return true;
    }

    // Pure assignments: the head and every remaining token are `NAME=value`.
    std::iter::once(first)
        .chain(tokens)
        .all(is_pure_assignment_token)
}

/// True if `tok` is a `NAME=value` assignment with a shell-valid name
/// (leading letter/underscore, then alphanumerics/underscores). Mirrors the
/// env-assignment recognizer used by the splitter and prefix detector.
fn is_pure_assignment_token(tok: &str) -> bool {
    let Some(eq) = tok.find('=') else {
        return false;
    };
    if eq == 0 {
        return false;
    }
    let mut chars = tok[..eq].chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Cleans up an abandoned permission request when the approval-wait future
/// is dropped without a user decision — the run being cancelled, or the
/// run ending while this wait was orphaned by a CLI transport drop (the
/// rmcp session worker keeps the future alive past the dropped
/// connection; `BindingGuard` reaps it at run end, which drops this
/// future). The guard then removes the still-pending registry entry,
/// tells the frontend to drop the now-useless approval card, and cancels
/// the run so the model cannot continue without the missing decision
/// (a no-op when the run already ended). Disarmed on a normal decision or
/// explicit supersede, where the caller already removed/replaced the entry.
///
/// Cleanup is async (registry lock) so it's spawned onto the app runtime;
/// `Drop` can't await. `take` is a no-op if the entry was already removed
/// (e.g. a decision raced in), so the guard is safe even if it fires late.
struct AbandonedApprovalGuard {
    app: tauri::AppHandle,
    cancel_token: tokio_util::sync::CancellationToken,
    request_id: String,
    workspace_id: Option<String>,
    armed: bool,
}

impl AbandonedApprovalGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn expire_and_stop<T>(&mut self) -> T {
        use tauri::Manager;

        let state = self.app.state::<crate::AppState>();
        if let Some((_, remaining)) = state.pending_approvals.take(&self.request_id).await {
            crate::commands::permissions::emit_permission_resolved(&self.app, &self.request_id);
            crate::commands::permissions::emit_attention(
                &self.app,
                self.workspace_id.clone(),
                remaining,
            );
        }
        self.armed = false;
        super::cancel_run_and_park(&self.cancel_token).await
    }
}

impl Drop for AbandonedApprovalGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancel_token.cancel();
        use tauri::Manager;
        let app = self.app.clone();
        let request_id = std::mem::take(&mut self.request_id);
        let workspace_id = self.workspace_id.take();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::AppState>();
            if let Some((_, remaining)) = state.pending_approvals.take(&request_id).await {
                crate::commands::permissions::emit_permission_resolved(&app, &request_id);
                crate::commands::permissions::emit_attention(&app, workspace_id, remaining);
            }
        });
    }
}

/// Runs the interactive approval round-trip for a command that needs
/// user input. Registers the request in app state, emits the request
/// and attention events, waits (with timeout) for the user's decisions,
/// and resolves to `Ok(())` if every segment was allowed or `Err(_)` on
/// any deny / timeout / channel close. A missing user decision also
/// cancels the run via `AbandonedApprovalGuard`, so the model does not
/// receive a generic tool timeout and continue. Persistence of "always"
/// grants is performed by [`crate::commands::permissions::submit_permission_decision`]
/// before the oneshot is fired, so the grant is durable across crashes
/// between user click and command execution.
async fn await_user_permission(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    command: &str,
    segments: Vec<crate::commands::permissions::SegmentApproval>,
) -> Result<(), String> {
    use crate::commands::permissions::{
        emit_attention, PendingApprovalOutcome, PermissionRequest, SegmentDecision,
        APPROVAL_TIMEOUT, PERMISSION_REQUEST_EVENT,
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
        segments: segments.clone(),
    };
    let request_id = request.request_id.clone();

    // Supersede: if a previous request for this exact run + command is
    // still pending, it is an orphan — its CLI transport dropped mid-call
    // and the model is now re-asking. Replace it (and its UI card) with
    // the fresh request instead of stacking a duplicate the user can no
    // longer meaningfully answer. The stale waiter receives an explicit
    // supersede outcome (no run cancellation — this run is alive and
    // waiting on the NEW request).
    for stale in app_state
        .pending_approvals
        .take_superseded(&context.run_id, command)
        .await
    {
        crate::commands::permissions::emit_permission_resolved(&deps.app, &stale.request_id);
        emit_attention(&deps.app, stale.workspace_id.clone(), stale.remaining);
    }

    let (rx, count) = app_state
        .pending_approvals
        .register(request.clone(), context.run_id.clone())
        .await;

    if let Err(e) = deps.app.emit(PERMISSION_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit permission request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    // Cleans up if this future is abandoned before a decision (CLI transport
    // drop mid-call, or run cancellation): clears the pending entry and tells
    // the frontend to drop the now-useless approval card. Disarmed below on a
    // normal decision and explicit supersede.
    let mut abandon_guard = AbandonedApprovalGuard {
        app: deps.app.clone(),
        cancel_token: context.cancel_token.clone(),
        request_id: request_id.clone(),
        workspace_id: workspace_id.clone(),
        armed: true,
    };

    let wait_timeout = context.interactive_wait_timeout(APPROVAL_TIMEOUT);
    let decisions = match tokio::time::timeout(wait_timeout, rx).await {
        Ok(Ok(PendingApprovalOutcome::Decision(d))) => {
            // The submit command already removed the registry entry and the
            // frontend cleared the card optimistically.
            abandon_guard.disarm();
            d
        }
        Ok(Ok(PendingApprovalOutcome::Superseded)) => {
            // A fresh registration for the same run + command replaced
            // this orphaned wait after a transport drop. This stale future
            // is intentionally ignored: no notice, no warning, no cancel.
            abandon_guard.disarm();
            let msg = "Permission request was superseded by a newer request \
                       for the same command before a decision was made";
            return Err(msg.to_string());
        }
        Ok(Err(_)) if context.cancel_token.is_cancelled() => {
            return super::cancel_run_and_park(&context.cancel_token).await;
        }
        Ok(Err(_)) => {
            return abandon_guard.expire_and_stop().await;
        }
        Err(_) => {
            return abandon_guard.expire_and_stop().await;
        }
    };

    // Cache the user's intent on the run-scoped allow/block lists
    // BEFORE the deny short-circuit, so a `DenyAlways` still ends the
    // current call with an error but its block-prefix is recorded so
    // subsequent retries of the same command don't re-prompt.
    //
    // - `AllowAlways` caches the user's chosen prefix (also persisted
    //   durably by the submit command).
    // - `AllowOnce` caches the segment's full text: `matches_prefix`
    //   accepts an exact match or an additional-args extension (so an
    //   `AllowOnce` of `git status` covers `git status -s` in the same
    //   run, matching what the durable `AllowAlways` semantics would
    //   give for the same prefix).
    // - `DenyAlways` caches the user's chosen prefix on the run-scoped
    //   blocklist (also persisted durably). Without this, the LLM's
    //   next retry of the just-denied command would re-prompt the user
    //   because persistence updates the DB but not the running
    //   `context.execution` snapshot.
    // - `DenyOnce` is intentionally not cached. It's a single-shot
    //   decision by design — re-prompting on retry lets the user
    //   reconsider. (The current call is still denied by the check
    //   below.)
    for (segment, decision) in segments.iter().zip(decisions.iter()) {
        match decision {
            SegmentDecision::AllowAlways { prefix, .. } => {
                context.add_session_allowed_command_prefix(prefix.clone());
            }
            SegmentDecision::AllowOnce => {
                context.add_session_allowed_command_prefix(segment.text.clone());
            }
            SegmentDecision::DenyAlways { prefix, .. } => {
                context.add_session_blocked_command_prefix(prefix.clone());
            }
            SegmentDecision::DenyOnce => {}
        }
    }

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
    let canonical_str = agent_path_string(&canonical);

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
                "path": agent_path_string(std::path::Path::new(&path)),
                "access": access_to_str(access),
                "scope": "once",
                "note": "Valid for the current turn only — NOT retained for later turns. If you need this path again in a future turn, request it again, or ask the user to grant it always.",
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
                "path": agent_path_string(std::path::Path::new(&path)),
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
    // Same workspace-isolation mask as `resolve_allowed_path`: a broad ancestor
    // grant (e.g. `$HOME`) does NOT cover a path inside the masked workspace
    // container. Without this, `fs_request_grant` would short-circuit a sibling
    // workspace as "already-granted" even though it's isolated and unreachable —
    // so the request must instead fall through to a real user prompt.
    let mask = grants.first().and_then(|ws| {
        crate::assistant::sandbox::profile::workspace_mask(&ws.root, real_host_home().as_deref())
    });
    grants.iter().any(|grant| {
        let covers_path = path == grant.root || path.starts_with(&grant.root);
        if !covers_path {
            return false;
        }
        if grant_masked_for_candidate(&grant.root, path, mask.as_deref()) {
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
/// Path-grant analogue of [`AbandonedApprovalGuard`]: clears a pending
/// filesystem path-grant request, drops its card, and cancels the run when
/// the approval-wait future is abandoned (run cancellation, or run-end
/// reaping of a wait orphaned by a CLI transport drop).
struct AbandonedPathGrantGuard {
    app: tauri::AppHandle,
    cancel_token: tokio_util::sync::CancellationToken,
    request_id: String,
    workspace_id: Option<String>,
    armed: bool,
}

impl AbandonedPathGrantGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn expire_and_stop<T>(&mut self) -> T {
        use tauri::Manager;

        let state = self.app.state::<crate::AppState>();
        if let Some((_, remaining)) = state.pending_path_grants.take(&self.request_id).await {
            crate::commands::path_grants::emit_path_grant_resolved(&self.app, &self.request_id);
            crate::commands::path_grants::emit_attention(
                &self.app,
                self.workspace_id.clone(),
                remaining,
            );
        }
        self.armed = false;
        super::cancel_run_and_park(&self.cancel_token).await
    }
}

impl Drop for AbandonedPathGrantGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancel_token.cancel();
        use tauri::Manager;
        let app = self.app.clone();
        let request_id = std::mem::take(&mut self.request_id);
        let workspace_id = self.workspace_id.take();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::AppState>();
            if let Some((_, remaining)) = state.pending_path_grants.take(&request_id).await {
                crate::commands::path_grants::emit_path_grant_resolved(&app, &request_id);
                crate::commands::path_grants::emit_attention(&app, workspace_id, remaining);
            }
        });
    }
}

async fn await_path_grant_decision(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    request: crate::commands::path_grants::PathGrantRequest,
) -> Result<crate::commands::path_grants::PathGrantDecision, String> {
    use crate::commands::path_grants::{
        emit_attention, PendingPathGrantOutcome, PATH_GRANT_REQUEST_EVENT, PATH_GRANT_TIMEOUT,
    };
    use tauri::{Emitter, Manager};

    let app_state = deps.app.state::<crate::AppState>();
    let workspace_id = context.workspace_id.clone();
    let request_id = request.request_id.clone();

    // Supersede a stale orphaned request for the same run + path + access
    // (the model re-asked after a CLI transport drop). See the analogous
    // block in `await_user_permission` for the full rationale.
    for stale in app_state
        .pending_path_grants
        .take_superseded(
            &context.run_id,
            &request.requested_path,
            request.requested_access,
        )
        .await
    {
        crate::commands::path_grants::emit_path_grant_resolved(&deps.app, &stale.request_id);
        emit_attention(&deps.app, stale.workspace_id.clone(), stale.remaining);
    }

    let (rx, count) = app_state
        .pending_path_grants
        .register(request.clone(), context.run_id.clone())
        .await;

    if let Err(e) = deps.app.emit(PATH_GRANT_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit path-grant request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    // See `AbandonedApprovalGuard`: clears the pending entry and drops the
    // card if this future is abandoned before a decision. Disarmed on a
    // normal decision and explicit supersede.
    let mut abandon_guard = AbandonedPathGrantGuard {
        app: deps.app.clone(),
        cancel_token: context.cancel_token.clone(),
        request_id: request_id.clone(),
        workspace_id: workspace_id.clone(),
        armed: true,
    };

    let wait_timeout = context.interactive_wait_timeout(PATH_GRANT_TIMEOUT);
    match tokio::time::timeout(wait_timeout, rx).await {
        Ok(Ok(PendingPathGrantOutcome::Decision(decision))) => {
            abandon_guard.disarm();
            Ok(decision)
        }
        Ok(Ok(PendingPathGrantOutcome::Superseded)) => {
            // A fresh registration for the same run + path + access
            // replaced this orphaned wait after a transport drop. This
            // stale future is intentionally ignored.
            abandon_guard.disarm();
            let msg = "Path-grant request was superseded by a newer request \
                       for the same path before a decision was made"
                .to_string();
            Err(msg)
        }
        Ok(Err(_)) if context.cancel_token.is_cancelled() => {
            super::cancel_run_and_park(&context.cancel_token).await
        }
        Ok(Err(_)) => abandon_guard.expire_and_stop().await,
        Err(_) => abandon_guard.expire_and_stop().await,
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
    match evaluate_command_policy(execution, command, &[], &[]) {
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
    if matches_prefix_literal(p, command) {
        return true;
    }
    // Fall back to the command's canonical form so an allowlist entry the
    // suggester proposed still matches the raw command it came from. The
    // suggester strips leading env assignments and global flags between the
    // binary and its subcommand (`git --no-pager log` → `git log`); the
    // literal match above does not, so without this an "Always allow git log"
    // grant would never cover a `git --no-pager log` invocation. Applying it
    // to the blocklist too only strengthens it (a blocked `rm` can't hide
    // behind `FOO=bar rm`).
    if let Some(canonical) =
        crate::assistant::tools::prefix_detector::canonicalize_for_match(command)
    {
        return matches_prefix_literal(p, &canonical);
    }
    false
}

/// Literal word-boundary prefix test: `command` equals `p`, or starts with
/// `p` followed by a space (so `git log` matches `git log --oneline` but not
/// `git logs`).
fn matches_prefix_literal(p: &str, command: &str) -> bool {
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

    let url = parse_web_fetch_url(params.url.trim())?;
    let response = fetch_public_url(url, timeout_ms).await?;
    let final_url = response.url().clone();

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

    let body_limit = web_fetch_body_limit(content_limit);
    let (body, body_truncated) = read_limited_web_fetch_body(response, body_limit).await?;

    // Convert HTML to markdown, or return plain text as-is
    let markdown = if content_type.contains("text/html") || content_type.contains("xhtml") {
        html_to_markdown(&body)
    } else {
        body
    };

    let markdown_truncated = markdown.len() > content_limit;
    let content = if markdown_truncated {
        let chars: Vec<char> = markdown.chars().collect();
        let end = content_limit.min(chars.len());
        chars[..end].iter().collect::<String>()
    } else {
        markdown
    };
    let truncated = body_truncated || markdown_truncated;

    Ok(serde_json::json!({
        "url": final_url.as_str(),
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

fn parse_web_fetch_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw)
        .map_err(|_| "URL must be an absolute http:// or https:// URL".to_string())?;
    validate_web_fetch_scheme(&url)?;
    Ok(url)
}

fn validate_web_fetch_scheme(url: &reqwest::Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        _ => Err("URL must start with http:// or https://".to_string()),
    }
}

async fn fetch_public_url(
    mut url: reqwest::Url,
    timeout_ms: u64,
) -> Result<reqwest::Response, String> {
    let mut redirects_followed = 0;
    loop {
        let dns_override = resolve_public_web_fetch_target(&url).await?;
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none());

        if let Some((host, addrs)) = &dns_override {
            builder = builder.resolve_to_addrs(host, addrs);
        }

        let client = builder
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        let response = client
            .get(url.clone())
            .header("User-Agent", "CLAI/1.0")
            .header("Accept", "text/html, text/plain, application/xhtml+xml")
            .send()
            .await
            .map_err(|e| format!("Fetch failed: {}", e))?;

        if is_followable_redirect(response.status()) {
            if redirects_followed >= MAX_WEB_FETCH_REDIRECTS {
                return Err("Fetch exceeded redirect limit".to_string());
            }
            redirects_followed += 1;

            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| {
                    format!(
                        "Fetch returned HTTP {} without a Location header",
                        response.status().as_u16()
                    )
                })?
                .to_str()
                .map_err(|_| "Redirect location is not valid UTF-8".to_string())?;
            url = url
                .join(location)
                .map_err(|e| format!("Invalid redirect location: {}", e))?;
            validate_web_fetch_scheme(&url)?;
            continue;
        }

        return Ok(response);
    }
}

fn is_followable_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

async fn resolve_public_web_fetch_target(
    url: &reqwest::Url,
) -> Result<Option<(String, Vec<SocketAddr>)>, String> {
    validate_web_fetch_scheme(url)?;
    let host = url
        .host_str()
        .ok_or_else(|| "URL must include a host".to_string())?;
    let host_literal = strip_ipv6_url_brackets(host);
    let normalized_host = host_literal.trim_end_matches('.').to_ascii_lowercase();
    if is_blocked_web_fetch_hostname(&normalized_host) {
        return Err("Fetching private/local URLs is not allowed".to_string());
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL must include a valid port".to_string())?;

    if let Ok(ip) = host_literal.parse::<IpAddr>() {
        reject_private_web_fetch_ip(ip)?;
        return Ok(None);
    }

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("Failed to resolve URL host: {}", e))?
        .collect();
    if addrs.is_empty() {
        return Err("URL host did not resolve to any address".to_string());
    }

    reject_private_web_fetch_addrs(&addrs)?;

    // Pin the public DNS result into reqwest for this request so a hostname
    // cannot validate as public and then resolve privately during connect.
    Ok(Some((host.to_string(), addrs)))
}

fn is_blocked_web_fetch_hostname(normalized_host: &str) -> bool {
    normalized_host == "localhost"
        || normalized_host.ends_with(".localhost")
        || normalized_host.ends_with(".local")
        || normalized_host.ends_with(".internal")
}

fn strip_ipv6_url_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host)
}

fn reject_private_web_fetch_ip(ip: IpAddr) -> Result<(), String> {
    if is_private_web_fetch_ip(ip) {
        Err("Fetching private/local URLs is not allowed".to_string())
    } else {
        Ok(())
    }
}

fn reject_private_web_fetch_addrs(addrs: &[SocketAddr]) -> Result<(), String> {
    for addr in addrs {
        reject_private_web_fetch_ip(addr.ip())?;
    }
    Ok(())
}

fn is_private_web_fetch_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_private_web_fetch_ipv4(ip),
        IpAddr::V6(ip) => is_private_web_fetch_ipv6(ip),
    }
}

fn is_private_web_fetch_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && c == 0)
        || a >= 240
}

fn is_private_web_fetch_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_private_web_fetch_ipv4(mapped);
    }

    let octets = ip.octets();
    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (octets[0] & 0xfe) == 0xfc
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || segments[0] == 0x2002
}

fn web_fetch_body_limit(content_limit: usize) -> usize {
    content_limit
        .saturating_mul(4)
        .min(MAX_WEB_FETCH_BODY_BYTES)
}

async fn read_limited_web_fetch_body(
    response: reqwest::Response,
    body_limit: usize,
) -> Result<(String, bool), String> {
    let mut body = Vec::with_capacity(body_limit.min(16 * 1024));
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Failed to read response body: {}", e))?;
        if append_limited_web_fetch_body_chunk(&mut body, &chunk, body_limit) {
            return Ok((String::from_utf8_lossy(&body).into_owned(), true));
        }
    }

    Ok((String::from_utf8_lossy(&body).into_owned(), false))
}

fn append_limited_web_fetch_body_chunk(body: &mut Vec<u8>, chunk: &[u8], limit: usize) -> bool {
    let remaining = limit.saturating_sub(body.len());
    if remaining == 0 {
        return true;
    }

    if chunk.len() > remaining {
        body.extend_from_slice(&chunk[..remaining]);
        return true;
    }

    body.extend_from_slice(chunk);
    false
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
    fn path_already_covered_excludes_masked_sibling_workspace() {
        // Regression: fs_request_grant must NOT report a sibling workspace as
        // "already-granted" just because $HOME contains it — the mask makes it
        // unreachable, so the request has to fall through to a real prompt.
        let Some(home) = real_host_home() else {
            return; // no home (rare); nothing to assert
        };
        let own = home.join(".clai/workspaces/own");
        let grants = vec![
            ResolvedGrant {
                root: own.clone(),
                access: AccessKind::ReadWrite,
            },
            ResolvedGrant {
                root: home.clone(),
                access: AccessKind::ReadOnly,
            },
        ];

        // A sibling workspace under $HOME is masked → not covered.
        let sibling = home.join(".clai/workspaces/other/file.txt");
        assert!(!path_already_covered(
            &grants,
            &sibling,
            FilesystemPathAccess::ReadOnly
        ));
        // The agent's own workspace IS covered.
        assert!(path_already_covered(
            &grants,
            &own.join("notes.md"),
            FilesystemPathAccess::ReadWrite
        ));
        // A non-workspace home path is still covered by the $HOME grant.
        assert!(path_already_covered(
            &grants,
            &home.join(".gitconfig"),
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
    // resolve_allowed_path — most-specific grant wins over iteration order
    // ------------------------------------------------------------------

    // Regression: a read-write grant on a subdirectory must be honored even
    // when a broader read-only grant on an ancestor appears earlier in the
    // list. Previously the first-match scan let the read-only ancestor shadow
    // the read-write descendant, rejecting legitimate writes (this is exactly
    // what blocked `fs_write` after `fs_request_grant` returned read_write).
    #[test]
    fn rw_descendant_grant_is_writable_under_ro_ancestor() {
        let grants = vec![
            ResolvedGrant {
                root: PathBuf::from("/home/me"),
                access: AccessKind::ReadOnly,
            },
            ResolvedGrant {
                root: PathBuf::from("/home/me/project"),
                access: AccessKind::ReadWrite,
            },
        ];
        let resolved = resolve_allowed_path("/home/me/project/src/main.rs", &grants, true).unwrap();
        assert_eq!(resolved, PathBuf::from("/home/me/project/src/main.rs"));
    }

    // The dual: a read-only carve-out on a subdirectory must override a broader
    // read-write ancestor, so writes into the carve-out are denied.
    #[test]
    fn ro_descendant_carveout_denies_write_under_rw_ancestor() {
        let grants = vec![
            ResolvedGrant {
                root: PathBuf::from("/home/me"),
                access: AccessKind::ReadWrite,
            },
            ResolvedGrant {
                root: PathBuf::from("/home/me/.ssh"),
                access: AccessKind::ReadOnly,
            },
        ];
        let err = resolve_allowed_path("/home/me/.ssh/id_ed25519", &grants, true).unwrap_err();
        assert!(err.contains("not writable"), "unexpected error: {err}");
    }

    // Reads under a read-only ancestor still work when no deeper grant applies.
    #[test]
    fn read_is_allowed_under_ro_ancestor_without_deeper_grant() {
        let grants = vec![ResolvedGrant {
            root: PathBuf::from("/home/me"),
            access: AccessKind::ReadOnly,
        }];
        let resolved = resolve_allowed_path("/home/me/notes.txt", &grants, false).unwrap();
        assert_eq!(resolved, PathBuf::from("/home/me/notes.txt"));
    }

    // A read-write entry coexisting at the SAME root as a read-only one (the
    // dedup in filesystem_grants only drops exact root+access duplicates) must
    // resolve as writable.
    #[test]
    fn rw_grant_wins_over_ro_grant_at_same_root() {
        let grants = vec![
            ResolvedGrant {
                root: PathBuf::from("/data"),
                access: AccessKind::ReadOnly,
            },
            ResolvedGrant {
                root: PathBuf::from("/data"),
                access: AccessKind::ReadWrite,
            },
        ];
        let resolved = resolve_allowed_path("/data/out.json", &grants, true).unwrap();
        assert_eq!(resolved, PathBuf::from("/data/out.json"));
    }

    // Paths outside every grant root are still rejected as out-of-bounds.
    #[test]
    fn path_outside_all_grants_is_rejected() {
        let grants = vec![ResolvedGrant {
            root: PathBuf::from("/home/me"),
            access: AccessKind::ReadWrite,
        }];
        let err = resolve_allowed_path("/etc/passwd", &grants, false).unwrap_err();
        assert!(
            err.contains("outside the agent's allowed filesystem grants"),
            "unexpected error: {err}"
        );
    }

    // ------------------------------------------------------------------
    // path_outside_home — Flatpak fs_* reachability predicate
    // ------------------------------------------------------------------

    // The structural gate behind `fs_unreachable_in_flatpak`: paths under the
    // real host home are reachable by the in-process tools; everything else
    // (e.g. /tmp — a private tmpfs in Flatpak — or /opt) is not. An unknown
    // home fails open so non-Flatpak/edge contexts are never blocked.
    #[test]
    fn path_outside_home_gates_non_home_paths() {
        let home = PathBuf::from("/home/me");
        assert!(!path_outside_home(
            Path::new("/home/me/.clai/workspaces/ws/file.txt"),
            Some(&home)
        ));
        assert!(!path_outside_home(Path::new("/home/me"), Some(&home)));
        assert!(path_outside_home(
            Path::new("/tmp/test_file.txt"),
            Some(&home)
        ));
        assert!(path_outside_home(Path::new("/opt/thing"), Some(&home)));
        // Unknown real home → never block.
        assert!(!path_outside_home(Path::new("/tmp/x"), None));
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
        // The "~/some/subpath" target does not exist, so the product returns
        // the normalized (non-canonicalized) form. Compare structurally:
        // `canonicalize` would add a Windows `\\?\` verbatim prefix and resolve
        // the macOS `/var`->`/private/var` symlink, both of which break a naive
        // prefix match. Component-based checks are separator-agnostic.
        assert!(resolved.is_absolute());
        assert!(resolved.starts_with(temp.path()));
        assert!(resolved.ends_with(std::path::Path::new("some").join("subpath")));

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
                    // Normalize Windows `\` to `/` so the assertion is
                    // separator-agnostic (no-op on Unix).
                    .replace('\\', "/")
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
                    // Normalize Windows `\` to `/` (no-op on Unix).
                    .replace('\\', "/")
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

        // `/etc/...` is not absolute on Windows (no drive), so use a
        // platform-absolute pattern that cannot intersect the temp grant.
        let outside_pattern = if cfg!(windows) {
            "C:/Windows/System32/**/*.conf"
        } else {
            "/etc/**/*.conf"
        };
        let error = glob_allowed_paths(outside_pattern, &[grant_for(root)], 10).unwrap_err();

        assert!(error.contains("outside the agent's allowed filesystem grants"));
    }

    #[test]
    fn agent_path_string_normalizes_paths_for_the_agent() {
        // Windows branch is exercised by the windows-latest CI job.
        #[cfg(windows)]
        {
            assert_eq!(
                agent_path_string(std::path::Path::new(r"C:\Users\dev\proj\file.rs")),
                "C:/Users/dev/proj/file.rs"
            );
            // `std::fs::canonicalize` emits a `\\?\` verbatim prefix; strip it.
            assert_eq!(
                agent_path_string(std::path::Path::new(r"\\?\C:\Users\dev\file.rs")),
                "C:/Users/dev/file.rs"
            );
        }
        #[cfg(not(windows))]
        {
            // Unix: returned verbatim (backslash is a legal filename byte).
            assert_eq!(
                agent_path_string(std::path::Path::new("/home/dev/proj/file.rs")),
                "/home/dev/proj/file.rs"
            );
            assert_eq!(
                agent_path_string(std::path::Path::new("/home/dev/we\\ird")),
                "/home/dev/we\\ird"
            );
        }
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
    fn web_fetch_ip_filter_blocks_non_public_ranges() {
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "192.0.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "2001:db8::1",
            "2002:0a00:0001::1",
        ] {
            assert!(is_private_web_fetch_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn web_fetch_ip_filter_allows_public_addresses() {
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!is_private_web_fetch_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn web_fetch_blocks_private_dns_results() {
        let addrs = [
            SocketAddr::new("93.184.216.34".parse().unwrap(), 80),
            SocketAddr::new("192.168.1.1".parse().unwrap(), 80),
        ];

        let err = reject_private_web_fetch_addrs(&addrs).unwrap_err();
        assert!(err.contains("private/local"));
    }

    #[test]
    fn web_fetch_hostname_filter_blocks_local_names() {
        for host in [
            "localhost",
            "api.localhost",
            "myhost.local",
            "service.internal",
        ] {
            assert!(is_blocked_web_fetch_hostname(host), "{host}");
        }
        assert!(!is_blocked_web_fetch_hostname("example.com"));
    }

    #[test]
    fn parse_web_fetch_url_rejects_non_absolute_http_urls() {
        assert!(parse_web_fetch_url("/relative/path").is_err());
        assert!(parse_web_fetch_url("ftp://example.com/file").is_err());
    }

    #[test]
    fn web_fetch_body_reader_truncates_chunks_at_limit() {
        let mut body = Vec::new();

        assert!(!append_limited_web_fetch_body_chunk(&mut body, b"hello", 8));
        assert!(append_limited_web_fetch_body_chunk(&mut body, b" world", 8));
        assert_eq!(body, b"hello wo");
    }

    #[test]
    fn web_fetch_body_limit_scales_but_stays_bounded() {
        assert_eq!(web_fetch_body_limit(0), 0);
        assert_eq!(web_fetch_body_limit(1_000), 4_000);
        assert_eq!(web_fetch_body_limit(MAX_WEB_FETCH_CONTENT_LIMIT), 400_000);
        assert_eq!(
            web_fetch_body_limit(MAX_WEB_FETCH_CONTENT_LIMIT * 10),
            MAX_WEB_FETCH_BODY_BYTES
        );
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

    #[tokio::test]
    async fn web_fetch_rejects_private_ipv6_literals() {
        let url = parse_web_fetch_url("http://[::1]/admin").unwrap();

        let err = resolve_public_web_fetch_target(&url).await.unwrap_err();
        assert!(err.contains("private/local"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_redirect_targets_to_private_urls() {
        let start = parse_web_fetch_url("https://example.com/start").unwrap();
        let redirected = start.join("http://127.0.0.1/admin").unwrap();

        let err = resolve_public_web_fetch_target(&redirected)
            .await
            .unwrap_err();
        assert!(err.contains("private/local"));
    }

    // ------------------------------------------------------------------
    // enforce_command_policy — agent policy wiring
    // ------------------------------------------------------------------

    use crate::config::types::ShellCapabilityConfig;
    use crate::config::{ExecutionCapabilityConfig, ShellAccessMode};

    // Workspace isolation: `grant_masked_for_candidate` is the core decision
    // for whether a grant authorizes a path under the masked workspace
    // container. mask = `~/u/.clai/workspaces`, own ws = `…/workspaces/own`.
    #[test]
    fn home_grant_does_not_authorize_a_sibling_workspace() {
        let mask = Path::new("/home/u/.clai/workspaces");
        let home = Path::new("/home/u");
        let sibling = Path::new("/home/u/.clai/workspaces/other/secret.txt");
        assert!(grant_masked_for_candidate(home, sibling, Some(mask)));
    }

    #[test]
    fn own_workspace_grant_authorizes_its_own_files() {
        let mask = Path::new("/home/u/.clai/workspaces");
        let own = Path::new("/home/u/.clai/workspaces/own");
        let file = Path::new("/home/u/.clai/workspaces/own/notes.md");
        assert!(!grant_masked_for_candidate(own, file, Some(mask)));
    }

    #[test]
    fn home_grant_still_authorizes_non_workspace_home_paths() {
        let mask = Path::new("/home/u/.clai/workspaces");
        let home = Path::new("/home/u");
        let gitconfig = Path::new("/home/u/.gitconfig");
        // Not under the container → mask doesn't block the HOME grant.
        assert!(!grant_masked_for_candidate(home, gitconfig, Some(mask)));
    }

    #[test]
    fn explicit_sibling_grant_authorizes_that_sibling() {
        // The "unless the user grants it" escape hatch: a grant rooted at the
        // sibling workspace is not blocked by the mask.
        let mask = Path::new("/home/u/.clai/workspaces");
        let other = Path::new("/home/u/.clai/workspaces/other");
        let file = Path::new("/home/u/.clai/workspaces/other/shared.txt");
        assert!(!grant_masked_for_candidate(other, file, Some(mask)));
    }

    #[test]
    fn no_mask_blocks_nothing() {
        let home = Path::new("/home/u");
        let any = Path::new("/home/u/.clai/workspaces/other/x");
        assert!(!grant_masked_for_candidate(home, any, None));
    }

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
    fn policy_opaque_segment_in_restricted_passes_when_head_allowlisted() {
        // `bash -c "..."` is Opaque, but its binary head matches the
        // allowlist via `matches_prefix`. The user explicitly opting
        // into a broad prefix like `bash` is treated as consent — the
        // modal had to show "Always allow bash" for this entry to land
        // on the durable list. Same flow as Simple segments.
        let exec = restricted_execution_config(&["bash"], &[]);
        assert!(enforce_command_policy(&exec, None, r#"bash -c "echo hi""#).is_ok());
    }

    #[test]
    fn policy_opaque_segment_in_restricted_denies_when_head_not_allowlisted() {
        // Negative case: only commands whose head matches the allowlist
        // pass. An Opaque `bash -c` doesn't get a free ride just because
        // `find` is allowlisted.
        let exec = restricted_execution_config(&["find"], &[]);
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
    fn policy_mixed_pipeline_with_opaque_passes_when_all_heads_allowlisted() {
        // `find . | xargs rm` — `xargs` head triggers Opaque (executor),
        // `find .` is Simple. With both heads on the allowlist the
        // pipeline passes. Caller still has the blocklist as the final
        // safety net for catastrophic verbs like `rm`.
        let exec = restricted_execution_config(&["find", "xargs"], &[]);
        assert!(enforce_command_policy(&exec, None, "find . | xargs rm").is_ok());
    }

    #[test]
    fn policy_mixed_pipeline_denies_when_opaque_head_missing() {
        // If the Opaque segment's head isn't allowlisted, the pipeline
        // still hits NeedsApproval — only `find` covered, `xargs` is not.
        let exec = restricted_execution_config(&["find"], &[]);
        let err = enforce_command_policy(&exec, None, "find . | xargs rm").unwrap_err();
        assert!(matches!(err, CommandDenial::NotInAllowList(_)));
    }

    #[test]
    fn policy_redirect_segment_in_restricted_allows_when_head_allowlisted() {
        // `cat /etc/hosts > /tmp/x` — the redirect forces Opaque, but
        // `cat` is the head and it's on the allowlist. Passes.
        let exec = restricted_execution_config(&["cat"], &[]);
        assert!(enforce_command_policy(&exec, None, "cat /etc/hosts > /tmp/x").is_ok());
    }

    #[test]
    fn policy_redirect_segment_in_restricted_denies_when_head_not_allowlisted() {
        let exec = restricted_execution_config(&["ls"], &[]);
        let err = enforce_command_policy(&exec, None, "cat /etc/hosts > /tmp/x").unwrap_err();
        assert!(matches!(err, CommandDenial::NotInAllowList(_)));
    }

    #[test]
    fn policy_empty_command_still_errors() {
        let exec = restricted_execution_config(&[], &[]);
        let err = enforce_command_policy(&exec, None, "   ").unwrap_err();
        assert!(matches!(err, CommandDenial::ExplicitlyBlocked(_)));
    }

    // -------------------------------------------------------------------
    // Run-scoped allowed prefixes — the within-run cache populated by
    // AllowOnce/AllowAlways so the user isn't re-prompted mid-run.
    // -------------------------------------------------------------------

    #[test]
    fn policy_run_scoped_prefix_covers_simple_segment() {
        // Empty durable allowlist, but a run-scoped entry for `git status`.
        // A fresh `git status` should pass without approval.
        let exec = restricted_execution_config(&[], &[]);
        let run_allowed = vec!["git status".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "git status", &run_allowed, &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_run_scoped_prefix_covers_descendant_args() {
        // `matches_prefix` accepts the cached prefix plus more arguments,
        // mirroring durable-allowlist semantics. AllowOnce on `git status`
        // therefore also covers `git status -s` in the same run.
        let exec = restricted_execution_config(&[], &[]);
        let run_allowed = vec!["git status".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "git status -s", &run_allowed, &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_run_scoped_prefix_does_not_cover_different_command() {
        let exec = restricted_execution_config(&[], &[]);
        let run_allowed = vec!["git status".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "git log", &run_allowed, &[]),
            PolicyResult::NeedsApproval(_),
        ));
    }

    #[test]
    fn policy_run_scoped_prefix_covers_opaque_segment() {
        // The run-scoped cache holds the exact segment text from a
        // prior AllowOnce — honor it so the user isn't re-prompted for
        // the same `bash -c "..."` in the same run. (Opaque is also
        // honored on the durable allowlist now; see
        // `policy_durable_allowlist_covers_opaque_segment_by_head`.)
        let exec = restricted_execution_config(&[], &[]);
        let run_allowed = vec![r#"bash -c "echo hi""#.to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, r#"bash -c "echo hi""#, &run_allowed, &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_durable_allowlist_covers_opaque_segment_by_head() {
        // The durable allowlist matches Opaque segments by their binary
        // head (via `matches_prefix`), same as it does for Simple. This
        // lets users persist trust for tools whose syntactic shape
        // forces Opaque classification (heredocs, redirects, $()) using
        // the same "Always allow <prefix>" flow as Simple segments.
        //
        // For `bash -c "echo hi"` the allowlist entry `bash -c` matches
        // because `matches_prefix` accepts the prefix plus tail args.
        // Note: allowlisting `bash` would also match — this is the user
        // explicitly opting into a very broad trust grant.
        let exec = restricted_execution_config(&["bash -c"], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, r#"bash -c "echo hi""#, &[], &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_durable_allowlist_covers_redirect_opaque() {
        // Real-world case from the friction screenshots: a redirect
        // (`2>/dev/null`) turns a perfectly safe `which go` into an
        // Opaque segment. With this fix, allowlisting `which` covers
        // both `which go` and `which go 2>/dev/null` without re-asking.
        let exec = restricted_execution_config(&["which"], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "which go 2>/dev/null", &[], &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_durable_allowlist_git_log_covers_no_pager_variant() {
        // The reported friction: the agent runs `git --no-pager log …`, the
        // suggester proposes `git log` (stripping the global flag), the user
        // clicks "Always allow", yet every later `git --no-pager log` was
        // re-prompted because the literal matcher never reconciled the saved
        // `git log` with the flagged command. With matcher-side
        // canonicalization the durable `git log` now covers it.
        let exec = restricted_execution_config(&["git log"], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "git --no-pager log --oneline -2", &[], &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_run_scoped_git_log_covers_no_pager_variant() {
        // Same agreement must hold for the within-run cache populated by a
        // fresh AllowAlways/AllowOnce, so the user isn't re-prompted for the
        // rest of the run after granting once.
        let exec = restricted_execution_config(&[], &[]);
        let run_allowed = vec!["git log".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "git --no-pager log --oneline", &run_allowed, &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_blocklist_canonicalization_blocks_env_prefixed_command() {
        // Canonicalization strengthens the blocklist symmetrically: a blocked
        // `rm` can't slip through behind an env assignment.
        let exec = restricted_execution_config(&["git log"], &["rm"]);
        let err = enforce_command_policy(&exec, None, "FOO=bar rm -rf /tmp/x").unwrap_err();
        assert!(matches!(err, CommandDenial::ExplicitlyBlocked(_)));
    }

    #[test]
    fn policy_opaque_segment_suggests_prefix_when_unmatched() {
        // When an Opaque segment isn't allowlisted, the approval payload
        // now carries a non-empty `suggested_prefix` so the FE can
        // surface an "Always allow <prefix>" button — the affordance that
        // makes the user's trust persistable. Previously this was empty
        // and only "Allow once" was offered for Opaque rows.
        let exec = restricted_execution_config(&[], &[]);
        let result = evaluate_command_policy(&exec, "find /tmp -name foo 2>/dev/null", &[], &[]);
        let PolicyResult::NeedsApproval(approvals) = result else {
            panic!("expected NeedsApproval, got {:?}", policy_label(&result));
        };
        assert_eq!(approvals.len(), 1);
        assert!(approvals[0].kind == crate::commands::permissions::SegmentKind::Opaque);
        assert_eq!(approvals[0].suggested_prefix, "find");
    }

    #[test]
    fn policy_dedups_repeated_prefix_into_one_approval() {
        // The motivating bug: `cd /a && cd /b && cd /c` derives the same
        // `cd` prefix three times. The user should be asked to grant `cd`
        // exactly once, not once per occurrence.
        let exec = restricted_execution_config(&[], &[]);
        let result = evaluate_command_policy(&exec, "cd /a && cd /b && cd /c", &[], &[]);
        let PolicyResult::NeedsApproval(approvals) = result else {
            panic!("expected NeedsApproval, got {:?}", policy_label(&result));
        };
        assert_eq!(approvals.len(), 1, "repeated `cd` prefix should collapse");
        assert_eq!(approvals[0].suggested_prefix, "cd");
    }

    #[test]
    fn policy_distinct_prefixes_are_not_merged() {
        // Different derived prefixes remain separate decisions.
        let exec = restricted_execution_config(&[], &[]);
        let result = evaluate_command_policy(&exec, "git add -A && git commit -m x", &[], &[]);
        let PolicyResult::NeedsApproval(approvals) = result else {
            panic!("expected NeedsApproval, got {:?}", policy_label(&result));
        };
        let prefixes: Vec<&str> = approvals
            .iter()
            .map(|a| a.suggested_prefix.as_str())
            .collect();
        assert_eq!(prefixes, vec!["git add", "git commit"]);
    }

    #[test]
    fn policy_empty_prefix_rows_are_not_merged() {
        // Rows with no stable prefix (junk `:` head) have nothing to key on
        // and must each stay as a distinct allow-once decision.
        let exec = restricted_execution_config(&[], &[]);
        let result = evaluate_command_policy(&exec, ": > a; : > b", &[], &[]);
        let PolicyResult::NeedsApproval(approvals) = result else {
            panic!("expected NeedsApproval, got {:?}", policy_label(&result));
        };
        assert_eq!(approvals.len(), 2, "empty-prefix rows must not collapse");
        assert!(approvals.iter().all(|a| a.suggested_prefix.is_empty()));
    }

    #[test]
    fn policy_opaque_segment_no_match_still_needs_approval() {
        // An Opaque segment whose head is NOT on the allowlist still
        // hits NeedsApproval. The durable-allowlist check is added, not
        // replacing the gate — it only short-circuits when the user has
        // explicitly trusted the prefix.
        let exec = restricted_execution_config(&["ls"], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "find /tmp -name foo 2>/dev/null", &[], &[]),
            PolicyResult::NeedsApproval(_),
        ));
    }

    fn policy_label(result: &PolicyResult) -> &'static str {
        match result {
            PolicyResult::Allow => "Allow",
            PolicyResult::Block { .. } => "Block",
            PolicyResult::NeedsApproval(_) => "NeedsApproval",
        }
    }

    // -------------------------------------------------------------------
    // Benign shell constructs — test expressions and pure assignments
    // never need approval (no execution surface of their own).
    // -------------------------------------------------------------------

    #[test]
    fn policy_single_bracket_test_auto_allows() {
        // From the drift-detection screenshot: `[ "$CURRENT" != "61dda97" ]`
        // is a string comparison with no side effects. Should not prompt
        // even with an empty allowlist.
        let exec = restricted_execution_config(&[], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, r#"[ "$CURRENT" != "61dda97" ]"#, &[], &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_double_bracket_test_auto_allows() {
        let exec = restricted_execution_config(&[], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "[[ -f /tmp/foo ]]", &[], &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_test_builtin_auto_allows() {
        let exec = restricted_execution_config(&[], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "test -d /tmp", &[], &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_pure_assignment_auto_allows() {
        let exec = restricted_execution_config(&[], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "FOO=bar", &[], &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_multiple_pure_assignments_auto_allow() {
        let exec = restricted_execution_config(&[], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "A=1 B=2 C=3", &[], &[]),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn policy_assignment_with_substitution_still_prompts() {
        // `CURRENT=$(git log ...)` runs `git log` via the substitution, so
        // it is NOT benign — the embedded command must still be gated.
        let exec = restricted_execution_config(&[], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "CURRENT=$(git log -1 --format='%h')", &[], &[]),
            PolicyResult::NeedsApproval(_),
        ));
    }

    #[test]
    fn policy_test_with_substitution_still_prompts() {
        // A test that embeds a command substitution runs that command.
        let exec = restricted_execution_config(&[], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, r#"[ -n "$(curl http://x)" ]"#, &[], &[]),
            PolicyResult::NeedsApproval(_),
        ));
    }

    #[test]
    fn policy_assignment_prefix_to_command_still_prompts() {
        // `FOO=bar mycmd` is an assignment-prefixed command, not a pure
        // assignment — the real command `mycmd` must still be gated.
        let exec = restricted_execution_config(&[], &[]);
        assert!(matches!(
            evaluate_command_policy(&exec, "FOO=bar mycmd --opt", &[], &[]),
            PolicyResult::NeedsApproval(_),
        ));
    }

    #[test]
    fn policy_benign_construct_still_subject_to_blocklist() {
        // The benign carve-out sits AFTER the blocklist check, so a
        // blocklisted prefix still blocks. (Contrived, but proves the
        // ordering: `[` is blstd.)
        let exec = restricted_execution_config(&[], &["["]);
        assert!(matches!(
            evaluate_command_policy(&exec, "[ -f /tmp/x ]", &[], &[]),
            PolicyResult::Block { .. },
        ));
    }

    // -------------------------------------------------------------------
    // Run-scoped blocklist — populated by DenyAlways so a fresh deny
    // takes effect mid-run instead of waiting for the next session.
    // -------------------------------------------------------------------

    #[test]
    fn policy_run_scoped_blocklist_blocks_simple_segment() {
        // No durable blocklist, but a run-scoped DenyAlways for `rm -rf`.
        // A fresh `rm -rf /tmp/foo` should hit Block without prompting.
        let exec = restricted_execution_config(&["rm"], &[]);
        let run_blocked = vec!["rm -rf".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "rm -rf /tmp/foo", &[], &run_blocked),
            PolicyResult::Block { .. },
        ));
    }

    #[test]
    fn policy_run_scoped_blocklist_takes_priority_over_run_scoped_allowlist() {
        // Blocklist wins over allowlist — matches the durable-list
        // precedence rule.
        let exec = restricted_execution_config(&[], &[]);
        let run_allowed = vec!["rm".to_string()];
        let run_blocked = vec!["rm -rf".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "rm -rf /tmp/foo", &run_allowed, &run_blocked),
            PolicyResult::Block { .. },
        ));
    }

    #[test]
    fn policy_run_scoped_blocklist_does_not_cover_unrelated_command() {
        // `rm file.txt` doesn't match the `rm -rf` prefix, so the
        // durable allowlist for `rm` still lets it through.
        let exec = restricted_execution_config(&["rm"], &[]);
        let run_blocked = vec!["rm -rf".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "rm file.txt", &[], &run_blocked),
            PolicyResult::Allow,
        ));
    }

    #[test]
    fn augment_timeout_error_passes_through_non_timeout() {
        let err = "Shell command failed: boom".to_string();
        assert_eq!(
            augment_timeout_error(err.clone(), DEFAULT_BASH_TIMEOUT_MS, false),
            err
        );
    }

    #[test]
    fn augment_timeout_error_points_at_timeoutms_below_cap() {
        let err = "Sandboxed shell command timed out after 300000 ms".to_string();
        let out = augment_timeout_error(err, DEFAULT_BASH_TIMEOUT_MS, false);
        assert!(out.contains("timed out after 300000 ms"));
        assert!(out.contains("default timeout"));
        assert!(out.contains("`timeoutMs`"));
        assert!(out.contains(&MAX_BASH_TIMEOUT_MS.to_string()));
    }

    #[test]
    fn augment_timeout_error_labels_requested_timeout() {
        let err = "Sandboxed shell command timed out after 600000 ms".to_string();
        let out = augment_timeout_error(err, 600_000, true);
        assert!(out.contains("requested timeout"));
    }

    #[test]
    fn augment_timeout_error_at_cap_advises_narrowing() {
        let err = "Sandboxed shell command timed out after 1800000 ms".to_string();
        let out = augment_timeout_error(err, MAX_BASH_TIMEOUT_MS, true);
        assert!(out.contains("maximum `timeoutMs`"));
        assert!(out.contains("narrow the command"));
    }
}

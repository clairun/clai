use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::config::{bundled, AppConfig, SkillSourceKind};
use crate::config::{
    ExecutionCapabilityConfig, FilesystemPathAccess, FilesystemPathGrant, ShellAccessMode,
};

const WORKSPACE_CONFIG_VERSION: u32 = 1;

#[derive(Debug)]
pub enum WorkspaceConfigError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    Serialize {
        source: serde_json::Error,
    },
}

impl std::fmt::Display for WorkspaceConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceConfigError::Io {
                operation,
                path,
                source,
            } => write!(f, "Failed to {} {}: {}", operation, path.display(), source),
            WorkspaceConfigError::Parse { path, source } => {
                write!(f, "Failed to parse {}: {}", path.display(), source)
            }
            WorkspaceConfigError::Serialize { source } => {
                write!(f, "Failed to serialize workspace config: {}", source)
            }
        }
    }
}

impl std::error::Error for WorkspaceConfigError {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSchedule {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub paused: bool,
    /// How the next run is computed. `Interval` (current behavior) fires
    /// `N` minutes after the previous completion. `Cron` fires at the
    /// next wall-clock time matching a Vixie-style 5-field expression in
    /// a user-chosen IANA timezone.
    #[serde(default)]
    pub kind: ScheduleKind,
    /// Unix-ms wall-clock time when this workspace's manager should run
    /// next. `None` means "as soon as possible" — used for first-time
    /// scheduling before any tick has happened, and as the explicit
    /// "clear" value when the schedule is disabled.
    ///
    /// Persisting this is what survives an app restart: without it, the
    /// scheduler's in-memory `Instant` next_run_at resets to the
    /// "ready-now" state on startup and every scheduled workspace fires
    /// immediately. The runner writes this after each completed tick;
    /// `apply_workspace_schedule` reads it when (re)creating the live
    /// instance so the live schedule resumes from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at_unix_ms: Option<i64>,
}

/// Discriminated union describing *how* the manager's next run is
/// computed. Stored inline on [`WorkspaceSchedule`] and consumed by
/// [`crate::agents::schedule::compute_next_run_at`].
///
/// Note the dual rename: `rename_all = "camelCase"` only affects
/// **variant** names (so the JSON tag reads as `"interval"` /
/// `"cron"`); `rename_all_fields = "camelCase"` is the separate
/// attribute that also renames the **fields inside each variant**.
/// Without it, the JSON would need snake_case field names like
/// `interval_minutes`, but the frontend (and serde-style consistency
/// with the rest of the config) sends `intervalMinutes`. Earlier
/// shipping omitted `rename_all_fields` plus had a `#[serde(default)]`
/// on `interval_minutes`, which silently turned the missing field
/// into `0` and tripped the "interval must be ≥1" validator —
/// surfacing as a confusing save error when the user's interval was
/// actually 24h.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "bindings.ts")]
pub enum ScheduleKind {
    /// Fire `N` minutes after the previous completion. Stable in the
    /// face of long-running tasks: a tick that takes 10 minutes pushes
    /// the next fire 10 minutes later, guaranteeing inter-run quiet
    /// time. Doesn't let the user pin to a particular clock-time — for
    /// that, use `Cron`.
    Interval { interval_minutes: u32 },
    /// Fire at the next wall-clock time matching a 5-field Vixie cron
    /// expression in the given IANA timezone (e.g. `0 9 * * 1-5` in
    /// `America/New_York` = weekdays at 9am NY-local across DST).
    Cron {
        expression: String,
        /// IANA timezone name. Empty / unknown values are rejected by
        /// `compute_next_run_at` at save time so an invalid string can't
        /// silently fall through to UTC.
        timezone: String,
    },
}

impl Default for ScheduleKind {
    fn default() -> Self {
        Self::Interval {
            interval_minutes: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    #[serde(default = "default_workspace_config_version")]
    pub version: u32,
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Unix ms when the most recent run (scheduled or run-now) completed in
    /// this workspace. Compared against `last_opened_at` to derive the
    /// workspace rail's "unread" indicator. 0 = no completion recorded yet.
    #[serde(default)]
    pub last_run_completed_at: i64,
    /// Unix ms when the user last opened (viewed) this workspace in the UI.
    /// Deliberately separate from `updated_at`: bumping that on every open
    /// would reorder the rail's recency sort just by looking at a workspace.
    #[serde(default)]
    pub last_opened_at: i64,
    /// Unix ms when the user starred this workspace in the rail, or 0 when
    /// not starred. Stored as a timestamp (not a bool) so the UI can sort
    /// recently-starred entries first if it ever wants to; `> 0` = starred.
    #[serde(default)]
    pub starred_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_provider_connection_id: Option<String>,
    pub default_agent_id: String,
    #[serde(default)]
    pub schedule: WorkspaceSchedule,
    #[serde(default)]
    pub agents: Vec<WorkspaceAgent>,
}

fn default_workspace_config_version() -> u32 {
    WORKSPACE_CONFIG_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgent {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    #[serde(default)]
    pub selected_skills: Vec<SkillRef>,
    #[serde(default)]
    pub selected_mcp_servers: Vec<McpRef>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum SkillRef {
    Bundled { slug: String },
    Personal { slug: String },
    Remote { url: String, slug: String },
}

/// Reference to an AppConfig MCP server, stored by server id. The id is
/// resolved to a display name at render time. Legacy configs stored
/// `{ "name": ... }` refs; those deserialize with an empty id and are
/// dropped on [`load`] — users re-attach the server instead of CLAI
/// guessing a name→id migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpRef {
    #[serde(default)]
    pub id: String,
    /// Context-bar toggle for the workspace manager: `true` keeps the server
    /// attached to the workspace conversation but excluded from sessions and
    /// scheduled runs. An absent key or `false` means enabled; member agents
    /// never set it. Legacy workspace-level `disabledMcpServers` keys are
    /// ignored by serde and disappear on the next save.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

impl WorkspaceConfig {
    pub fn new(id: String, title: String, now: i64, manager_id: String) -> Self {
        Self {
            version: WORKSPACE_CONFIG_VERSION,
            id,
            title,
            created_at: now,
            updated_at: now,
            last_run_completed_at: 0,
            last_opened_at: 0,
            starred_at: 0,
            preferred_provider_connection_id: None,
            default_agent_id: manager_id.clone(),
            schedule: WorkspaceSchedule::default(),
            agents: vec![WorkspaceAgent::new_manager(manager_id, now)],
        }
    }

    /// Attach the first enabled provider connection as this workspace's
    /// default, so a freshly created workspace is immediately usable without a
    /// trip to Settings. Sets both the workspace-level preferred provider and
    /// the manager agent's provider list (the source of truth scheduled runs
    /// read). No-op when there are no enabled connections.
    pub fn attach_default_provider(
        &mut self,
        connections: &[crate::assistant::types::ProviderConnection],
        now: i64,
    ) {
        let Some(first) = connections.iter().find(|c| c.enabled) else {
            return;
        };
        self.preferred_provider_connection_id = Some(first.id.clone());
        let default_agent_id = self.default_agent_id.clone();
        if let Some(manager) = self.agents.iter_mut().find(|a| a.id == default_agent_id) {
            manager.provider_connection_ids = vec![first.id.clone()];
            manager.updated_at = now;
        }
        self.updated_at = now;
    }
}

/// Build the default sandbox config for a new agent. Every fresh agent —
/// manager, sub-agent, or template-instantiated — ships with the host
/// `$HOME` granted read-only so it can read user dotfiles (`.gitconfig`,
/// `.bashrc`, ...) the way the user's shell would. The user can ×-remove
/// it in agent settings to harden any specific agent.
pub fn default_agent_execution() -> ExecutionCapabilityConfig {
    let mut execution = ExecutionCapabilityConfig::default();
    if let Some(home) = dirs::home_dir() {
        let path = home.display().to_string();
        execution.filesystem.extra_paths.push(FilesystemPathGrant {
            path,
            access: FilesystemPathAccess::ReadOnly,
            origin: None,
        });
    }
    execution
}

impl WorkspaceAgent {
    pub fn new_manager(id: String, now: i64) -> Self {
        // A freshly created workspace should be ready to work without a detour
        // to Settings: give its manager restricted shell access (sandboxed
        // bash_exec with the default blocklist) and web access by default. The
        // user can still tighten either in agent settings.
        let mut execution = default_agent_execution();
        execution.shell.mode = ShellAccessMode::Restricted;
        execution.web.enabled = true;
        Self {
            id,
            name: "Manager".to_string(),
            description: String::new(),
            enabled: true,
            selected_skills: Vec::new(),
            selected_mcp_servers: Vec::new(),
            provider_connection_ids: Vec::new(),
            execution,
            created_at: now,
            updated_at: now,
        }
    }
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join(".clai").join("config.json")
}

pub fn data_path(root: &Path) -> PathBuf {
    root.join(".clai").join("data.sqlite")
}

#[cfg(test)]
thread_local! {
    /// Counts actual disk parses so tests can assert that [`load_cached`]
    /// served a memo hit without depending on *how* the memo decides a hit is
    /// valid. Thread-local because the test harness runs tests in parallel.
    static PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn load(root: &Path) -> Result<WorkspaceConfig, WorkspaceConfigError> {
    #[cfg(test)]
    PARSE_COUNT.with(|count| count.set(count.get() + 1));
    let path = config_path(root);
    let contents = fs::read_to_string(&path).map_err(|source| WorkspaceConfigError::Io {
        operation: "read",
        path: path.clone(),
        source,
    })?;
    let mut config: WorkspaceConfig = serde_json::from_str(&contents)
        .map_err(|source| WorkspaceConfigError::Parse { path, source })?;
    prune_legacy_mcp_refs(&mut config);
    Ok(config)
}

/// A parsed config plus the file stamp it was parsed from.
struct CachedConfig {
    mtime: SystemTime,
    len: u64,
    config: Arc<WorkspaceConfig>,
}

/// One path's slot in the memo.
///
/// `generation` counts invalidations of this path and outlives the parse it
/// guards, which is what lets [`load_cached`] tell "nothing happened while I
/// was reading" from "a writer invalidated me mid-read". Slots are per path,
/// so a write to one workspace never suppresses memoization for another.
#[derive(Default)]
struct CacheSlot {
    generation: u64,
    entry: Option<CachedConfig>,
}

/// Memo for [`load_cached`], keyed by `config.json` path.
///
/// Parsed configs are dropped by [`save`] and by [`forget`] (which workspace
/// deletion calls); what survives a deletion is the path key and its
/// generation counter, on the order of a hundred bytes per workspace that
/// has existed in this process.
static CONFIG_CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheSlot>>> = OnceLock::new();

fn config_cache() -> &'static Mutex<HashMap<PathBuf, CacheSlot>> {
    CONFIG_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_config_cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, CacheSlot>> {
    config_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Read the slot's generation and, when the stamp still matches, its parse.
/// Both under one lock so the generation belongs to the same observation as
/// the hit.
fn probe_cache(path: &Path, mtime: SystemTime, len: u64) -> (u64, Option<Arc<WorkspaceConfig>>) {
    let cache = lock_config_cache();
    match cache.get(path) {
        Some(slot) => {
            let hit = slot
                .entry
                .as_ref()
                .filter(|entry| entry.mtime == mtime && entry.len == len)
                .map(|entry| Arc::clone(&entry.config));
            (slot.generation, hit)
        }
        None => (0, None),
    }
}

/// Publish a parse under the stamp it was read at, unless this path was
/// invalidated while the read was in flight.
fn memoize_if_current(
    path: PathBuf,
    mtime: SystemTime,
    len: u64,
    config: Arc<WorkspaceConfig>,
    generation: u64,
) {
    let mut cache = lock_config_cache();
    let slot = cache.entry(path).or_default();
    if slot.generation != generation {
        return;
    }
    slot.entry = Some(CachedConfig { mtime, len, config });
}

fn invalidate_cached(path: &Path) {
    let mut cache = lock_config_cache();
    let slot = cache.entry(path.to_path_buf()).or_default();
    slot.generation = slot.generation.wrapping_add(1);
    slot.entry = None;
}

/// Drop the memo for a workspace root. Call this when the root goes away
/// (workspace deletion); otherwise a path that is later recreated could be
/// served from the previous workspace's parse.
pub fn forget(root: &Path) {
    invalidate_cached(&config_path(root));
}

/// Revalidating memo over [`load`] for read-only hot paths.
///
/// The Fleet list and the workspace snapshot are both polled every 5s by
/// the UI, and each pass loads the *same* `config.json` several times over
/// (`resolve_workspace_descriptor`, `workspace_default_agent_id`,
/// `load_workspace_agent_rows`, the `created_at` lookup, ...). With N
/// workspaces open that is O(5N) full `read_to_string` + `serde_json`
/// parses of a multi-KB file every 5 seconds, forever, with the app
/// otherwise idle — the dominant cost in an idle CPU profile.
///
/// This is a memo, not a snapshot: **every** call stats the file and only
/// serves the cached parse when both mtime and length still match, so an
/// edit made outside the process is picked up on the next call. A hit
/// costs one `statx` plus a clone instead of a read plus a parse.
///
/// Writers go through [`save`], which drops the entry, so an in-process
/// write is visible to the next reader even if the filesystem's mtime
/// resolution is too coarse to notice it.
///
/// Read-modify-write cycles must still use [`update`] (which reads through
/// [`load`], uncached, under the write lock). Never build a write on top of
/// this function.
pub fn load_cached(root: &Path) -> Result<WorkspaceConfig, WorkspaceConfigError> {
    let path = config_path(root);
    let Some((mtime, len)) = file_stamp(&path) else {
        // Unreadable/missing: drop any entry so a deleted config can never
        // be served from memory, and let `load` report the real IO error.
        invalidate_cached(&path);
        return load(root);
    };

    // Bound to a `let` so the cache lock is released before the deep copy:
    // on edition 2021 a temporary in an `if let` scrutinee lives for the
    // whole block.
    let (generation, hit) = probe_cache(&path, mtime, len);
    if let Some(hit) = hit {
        return Ok((*hit).clone());
    }

    // Stamp is read *before* the parse on purpose. If the file is rewritten
    // while we read it we store fresh content under the older stamp, and the
    // next call's stat mismatches and re-reads. Stamping afterwards would
    // instead risk pinning older content under the newer stamp, which would
    // stay stale until the following write.
    //
    // The stamp alone is not enough, though: a writer whose new bytes have
    // the same length and land inside the filesystem's mtime resolution
    // produces an identical stamp. Such a write could complete (and drop the
    // entry) between our stat and our insert, and we would then reinstate the
    // pre-write parse under a stamp that still looks current — stale until
    // the *next* write. `memoize_if_current` rejects that insert, because the
    // writer moved this path's generation past the one we probed with.
    let config = Arc::new(load(root)?);
    memoize_if_current(path, mtime, len, Arc::clone(&config), generation);
    Ok((*config).clone())
}

/// Drops MCP refs without a server id. Legacy configs referenced servers
/// by name; those refs are removed on load (the next save persists the
/// removal) and the user re-attaches the server from the UI.
fn prune_legacy_mcp_refs(config: &mut WorkspaceConfig) {
    for agent in &mut config.agents {
        agent
            .selected_mcp_servers
            .retain(|mcp_ref| !mcp_ref.id.is_empty());
    }
}

pub fn save(root: &Path, config: &WorkspaceConfig) -> Result<(), WorkspaceConfigError> {
    let path = config_path(root);
    let parent = path.parent().unwrap_or(root);
    fs::create_dir_all(parent).map_err(|source| WorkspaceConfigError::Io {
        operation: "create directory",
        path: parent.to_path_buf(),
        source,
    })?;

    let json = serde_json::to_string_pretty(config)
        .map_err(|source| WorkspaceConfigError::Serialize { source })?;
    let temp_path = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temp_path).map_err(|source| WorkspaceConfigError::Io {
        operation: "create",
        path: temp_path.clone(),
        source,
    })?;
    file.write_all(json.as_bytes())
        .map_err(|source| WorkspaceConfigError::Io {
            operation: "write",
            path: temp_path.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| WorkspaceConfigError::Io {
        operation: "sync",
        path: temp_path.clone(),
        source,
    })?;
    fs::rename(&temp_path, &path).map_err(|source| WorkspaceConfigError::Io {
        operation: "rename",
        path: path.clone(),
        source,
    })?;
    // The new bytes may land within the filesystem's mtime resolution of the
    // old ones, so `load_cached`'s stamp check alone is not enough to
    // guarantee a writer's own change is visible to the next reader.
    invalidate_cached(&path);
    Ok(())
}

/// Process-wide lock serializing read-modify-write cycles on workspace
/// config files. One lock for all workspaces: writes are rare and tiny,
/// so contention is negligible and a per-root map isn't worth the
/// bookkeeping.
static UPDATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Atomically read-modify-write a workspace's `config.json`.
///
/// Every writer that loads the config, mutates it, and saves it back
/// MUST go through this function. Bare `load` → mutate → `save`
/// sequences race with each other as lost updates: the agent runner's
/// run-completion persist (the `schedule.next_run_at_unix_ms` anchor)
/// was clobbered by `workspace_mark_opened`, which the FE invokes the
/// moment a run ends while the user is viewing that workspace — the
/// two cycles deterministically overlapped and whichever saved last
/// won. The reverted (past) anchor then re-fired the schedule on every
/// app restart.
///
/// The closure may return `Err` to abort; nothing is written then. On
/// success the freshly-saved config is returned so callers can update
/// the in-memory workspace index to match disk.
pub fn update<R>(
    root: &Path,
    mutate: impl FnOnce(&mut WorkspaceConfig) -> Result<R, String>,
) -> Result<(R, WorkspaceConfig), String> {
    let _guard = UPDATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut config = load(root).map_err(|e| e.to_string())?;
    let value = mutate(&mut config)?;
    save(root, &config).map_err(|e| e.to_string())?;
    Ok((value, config))
}

pub fn skill_ids_to_refs(config: &AppConfig, ids: &[String]) -> Vec<SkillRef> {
    ids.iter()
        .map(|id| {
            let Some((source_id, slug)) = id.split_once(':') else {
                return SkillRef::Personal { slug: id.clone() };
            };
            let Some(source) = config
                .skill_sources
                .iter()
                .find(|source| source.id == source_id)
            else {
                return SkillRef::Personal { slug: id.clone() };
            };
            if bundled::is_bundled_source(source) {
                SkillRef::Bundled {
                    slug: slug.to_string(),
                }
            } else if bundled::is_personal_source(source) {
                SkillRef::Personal {
                    slug: slug.to_string(),
                }
            } else if let SkillSourceKind::Git { uri, .. } = &source.source {
                SkillRef::Remote {
                    url: uri.clone(),
                    slug: slug.to_string(),
                }
            } else {
                SkillRef::Personal { slug: id.clone() }
            }
        })
        .collect()
}

pub fn refs_to_skill_ids(config: &AppConfig, refs: &[SkillRef]) -> Vec<String> {
    refs.iter()
        .filter_map(|skill_ref| match skill_ref {
            SkillRef::Bundled { slug } => config
                .skill_sources
                .iter()
                .find(|source| bundled::is_bundled_source(source))
                .map(|source| format!("{}:{}", source.id, slug)),
            SkillRef::Personal { slug } => config
                .skill_sources
                .iter()
                .find(|source| bundled::is_personal_source(source))
                .map(|source| format!("{}:{}", source.id, slug))
                .or_else(|| Some(slug.clone())),
            SkillRef::Remote { url, slug } => config
                .skill_sources
                .iter()
                .find(|source| match &source.source {
                    SkillSourceKind::Git { uri, .. } => uri == url,
                    SkillSourceKind::Local { .. } => false,
                })
                .map(|source| format!("{}:{}", source.id, slug)),
        })
        .collect()
}

pub fn mcp_ids_to_refs(ids: &[String]) -> Vec<McpRef> {
    ids.iter()
        .map(|id| McpRef {
            id: id.clone(),
            disabled: false,
        })
        .collect()
}

/// Every attached server id, the context-bar toggle notwithstanding. Use for
/// Settings surfaces that edit attachment; sessions and runs must use
/// [`enabled_mcp_ids`] instead.
pub fn refs_to_mcp_ids(refs: &[McpRef]) -> Vec<String> {
    refs.iter().map(|mcp_ref| mcp_ref.id.clone()).collect()
}

/// The effective enabled set consumed by sessions and scheduled runs.
pub fn enabled_mcp_ids(refs: &[McpRef]) -> Vec<String> {
    refs.iter()
        .filter(|mcp_ref| !mcp_ref.disabled)
        .map(|mcp_ref| mcp_ref.id.clone())
        .collect()
}

/// Attached-but-toggled-off servers (the context-bar badges).
pub fn disabled_mcp_ids(refs: &[McpRef]) -> Vec<String> {
    refs.iter()
        .filter(|mcp_ref| mcp_ref.disabled)
        .map(|mcp_ref| mcp_ref.id.clone())
        .collect()
}

/// Rebuilds an agent's MCP refs from a Settings save. The request carries
/// attachment only (which servers are checked); the context-bar `disabled`
/// flag is preserved for ids that stay attached and dropped together with
/// the ref when a server is unchecked.
pub fn merge_mcp_selection(previous: &[McpRef], requested_ids: &[String]) -> Vec<McpRef> {
    requested_ids
        .iter()
        .map(|id| McpRef {
            id: id.clone(),
            disabled: previous
                .iter()
                .any(|mcp_ref| mcp_ref.id == *id && mcp_ref.disabled),
        })
        .collect()
}

#[cfg(test)]
mod attach_provider_tests {
    use super::*;
    use crate::assistant::types::{AuthMode, ProviderConnection};

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

    fn workspace() -> WorkspaceConfig {
        WorkspaceConfig::new("ws".to_string(), "Title".to_string(), 1, "mgr".to_string())
    }

    #[test]
    fn attaches_first_enabled_connection_to_manager_and_preferred() {
        let mut config = workspace();
        config.attach_default_provider(&[connection("a", true), connection("b", true)], 42);

        assert_eq!(
            config.preferred_provider_connection_id.as_deref(),
            Some("a")
        );
        let manager = config.agents.iter().find(|a| a.id == "mgr").unwrap();
        assert_eq!(manager.provider_connection_ids, vec!["a".to_string()]);
        assert_eq!(manager.updated_at, 42);
    }

    #[test]
    fn skips_disabled_connections_and_picks_first_enabled() {
        let mut config = workspace();
        config.attach_default_provider(&[connection("a", false), connection("b", true)], 7);

        assert_eq!(
            config.preferred_provider_connection_id.as_deref(),
            Some("b")
        );
        let manager = config.agents.iter().find(|a| a.id == "mgr").unwrap();
        assert_eq!(manager.provider_connection_ids, vec!["b".to_string()]);
    }

    #[test]
    fn no_op_when_no_enabled_connections() {
        let mut config = workspace();
        config.attach_default_provider(&[connection("a", false)], 9);

        assert!(config.preferred_provider_connection_id.is_none());
        let manager = config.agents.iter().find(|a| a.id == "mgr").unwrap();
        assert!(manager.provider_connection_ids.is_empty());
    }

    #[test]
    fn new_manager_defaults_to_restricted_shell_and_web_enabled() {
        let manager = WorkspaceAgent::new_manager("mgr".to_string(), 1);
        assert_eq!(manager.execution.shell.mode, ShellAccessMode::Restricted);
        assert!(manager.execution.web.enabled);
    }

    // -------------------------------------------------------------------
    // update(): atomic read-modify-write
    // -------------------------------------------------------------------

    #[test]
    fn load_drops_legacy_name_only_mcp_refs() {
        // Legacy configs stored MCP refs as { "name": ... }. Per the
        // migration policy those refs are dropped on load (users re-attach
        // the server); id-based refs survive untouched.
        let tmp = tempfile::tempdir().unwrap();
        let config = workspace();
        save(tmp.path(), &config).unwrap();

        let path = config_path(tmp.path());
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // Legacy workspace-level key: serde ignores it and the next save
        // drops it — parsing must not fail.
        raw["disabledMcpServers"] = serde_json::json!([{ "id": "srv-1" }]);
        raw["agents"][0]["selectedMcpServers"] = serde_json::json!([
            { "name": "legacy-by-name" },
            { "id": "srv-2" },
            { "id": "srv-3", "disabled": true }
        ]);
        fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

        let loaded = load(tmp.path()).unwrap();
        assert_eq!(
            loaded.agents[0].selected_mcp_servers,
            vec![
                McpRef {
                    id: "srv-2".to_string(),
                    disabled: false
                },
                McpRef {
                    id: "srv-3".to_string(),
                    disabled: true
                }
            ]
        );
    }

    #[test]
    fn mcp_ref_disabled_flag_round_trips_and_defaults_off() {
        // `disabled: false` must not serialize (the absent key means
        // enabled); `disabled: true` must round-trip.
        let enabled = McpRef {
            id: "srv-a".to_string(),
            disabled: false,
        };
        assert!(serde_json::to_value(&enabled)
            .unwrap()
            .get("disabled")
            .is_none());

        let disabled = McpRef {
            id: "srv-b".to_string(),
            disabled: true,
        };
        let json = serde_json::to_value(&disabled).unwrap();
        assert_eq!(json["disabled"], serde_json::json!(true));
        let back: McpRef = serde_json::from_value(json).unwrap();
        assert!(back.disabled);

        let absent: McpRef = serde_json::from_value(serde_json::json!({ "id": "srv-c" })).unwrap();
        assert!(!absent.disabled);
    }

    #[test]
    fn enabled_and_disabled_mcp_ids_partition_refs() {
        let refs = vec![
            McpRef {
                id: "srv-a".to_string(),
                disabled: false,
            },
            McpRef {
                id: "srv-b".to_string(),
                disabled: true,
            },
        ];
        assert_eq!(enabled_mcp_ids(&refs), vec!["srv-a".to_string()]);
        assert_eq!(disabled_mcp_ids(&refs), vec!["srv-b".to_string()]);
        assert_eq!(
            refs_to_mcp_ids(&refs),
            vec!["srv-a".to_string(), "srv-b".to_string()]
        );
    }

    #[test]
    fn merge_mcp_selection_preserves_disabled_flags() {
        let previous = vec![
            McpRef {
                id: "srv-a".to_string(),
                disabled: false,
            },
            McpRef {
                id: "srv-b".to_string(),
                disabled: true,
            },
        ];
        // srv-b stays attached (its toggle survives), srv-c is newly
        // attached (enabled), srv-a is unchecked (dropped with its flag).
        let merged = merge_mcp_selection(&previous, &["srv-b".to_string(), "srv-c".to_string()]);
        assert_eq!(
            merged,
            vec![
                McpRef {
                    id: "srv-b".to_string(),
                    disabled: true
                },
                McpRef {
                    id: "srv-c".to_string(),
                    disabled: false
                }
            ]
        );
    }

    #[test]
    fn update_persists_mutation_and_returns_saved_config() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();

        let (value, config) = update(tmp.path(), |config| {
            config.schedule.next_run_at_unix_ms = Some(123);
            Ok("done")
        })
        .unwrap();

        assert_eq!(value, "done");
        assert_eq!(config.schedule.next_run_at_unix_ms, Some(123));
        let on_disk = load(tmp.path()).unwrap();
        assert_eq!(on_disk.schedule.next_run_at_unix_ms, Some(123));
    }

    #[test]
    fn update_err_closure_aborts_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();

        let result = update(tmp.path(), |config| {
            config.title = "clobbered".to_string();
            Err::<(), _>("validation failed".to_string())
        });

        assert_eq!(result.unwrap_err(), "validation failed");
        assert_eq!(load(tmp.path()).unwrap().title, "Title");
    }

    /// Regression test for the lost-update race: the runner's
    /// run-completion persist (writes `next_run_at_unix_ms`) overlapped
    /// with `workspace_mark_opened` (writes `last_opened_at`); whichever
    /// bare load→save finished last erased the other's field. With
    /// `update()` both mutations must survive regardless of interleaving.
    #[test]
    fn update_concurrent_writers_lose_no_fields() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();
        let root = tmp.path().to_path_buf();

        let runner = {
            let root = root.clone();
            std::thread::spawn(move || {
                update(&root, |config| {
                    config.schedule.next_run_at_unix_ms = Some(999);
                    Ok(())
                })
                .unwrap();
            })
        };
        let opener = {
            let root = root.clone();
            std::thread::spawn(move || {
                update(&root, |config| {
                    config.last_opened_at = 555;
                    Ok(())
                })
                .unwrap();
            })
        };
        runner.join().unwrap();
        opener.join().unwrap();

        let on_disk = load(&root).unwrap();
        assert_eq!(on_disk.schedule.next_run_at_unix_ms, Some(999));
        assert_eq!(on_disk.last_opened_at, 555);
    }

    /// `starredAt` must default to 0 (unstarred) for configs written before
    /// the field existed, and survive an `update` roundtrip once set.
    #[test]
    fn starred_at_defaults_to_zero_and_roundtrips() {
        // Legacy config JSON without the field deserializes as unstarred.
        let mut legacy = serde_json::to_value(workspace()).unwrap();
        legacy.as_object_mut().unwrap().remove("starredAt");
        let parsed: WorkspaceConfig = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.starred_at, 0);

        // Set-then-load roundtrip through the atomic updater.
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();
        update(tmp.path(), |config| {
            config.starred_at = 1234;
            Ok(())
        })
        .unwrap();
        assert_eq!(load(tmp.path()).unwrap().starred_at, 1234);
    }
}

#[cfg(test)]
mod load_cached_tests {
    use super::*;

    fn workspace() -> WorkspaceConfig {
        WorkspaceConfig::new("ws".to_string(), "Title".to_string(), 1, "mgr".to_string())
    }

    fn stamp_of(path: &Path) -> (SystemTime, SystemTime) {
        let meta = fs::metadata(path).unwrap();
        (meta.accessed().unwrap(), meta.modified().unwrap())
    }

    fn restore_stamp(path: &Path, stamp: (SystemTime, SystemTime)) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_times(
            fs::FileTimes::new()
                .set_accessed(stamp.0)
                .set_modified(stamp.1),
        )
        .unwrap();
    }

    #[test]
    fn first_read_matches_load() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();

        assert_eq!(
            load_cached(tmp.path()).unwrap().title,
            load(tmp.path()).unwrap().title
        );
    }

    /// Asserted on the parse counter rather than on observable staleness, so
    /// the test keeps its meaning if the validation strategy changes.
    #[test]
    fn repeat_reads_come_from_the_memo() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();

        let before = PARSE_COUNT.with(std::cell::Cell::get);
        for _ in 0..5 {
            assert_eq!(load_cached(tmp.path()).unwrap().title, "Title");
        }
        let parses = PARSE_COUNT.with(std::cell::Cell::get) - before;

        assert_eq!(
            parses, 1,
            "5 cached reads cost {parses} parses; exactly one cold read is expected"
        );
    }

    #[test]
    fn an_external_write_invalidates_the_memo() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();
        assert_eq!(load_cached(tmp.path()).unwrap().title, "Title");

        // A real edit changes the length, so the stamp check misses.
        let mut config = workspace();
        config.title = "Renamed by another process".to_string();
        let path = config_path(tmp.path());
        fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        assert_eq!(
            load_cached(tmp.path()).unwrap().title,
            "Renamed by another process"
        );
    }

    /// mtime resolution can be coarser than back-to-back writes, so `save`
    /// drops the entry outright rather than relying on the stamp.
    #[test]
    fn save_invalidates_the_memo_even_with_an_unchanged_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();
        assert_eq!(load_cached(tmp.path()).unwrap().title, "Title");

        let path = config_path(tmp.path());
        let stamp = stamp_of(&path);
        let mut config = workspace();
        config.title = "Tit1e".to_string(); // same length as "Title"
        save(tmp.path(), &config).unwrap();
        restore_stamp(&path, stamp);

        assert_eq!(load_cached(tmp.path()).unwrap().title, "Tit1e");
    }

    /// Same reasoning for the read-modify-write path, which reads through
    /// `load` and writes through `save`.
    #[test]
    fn update_is_visible_to_the_next_cached_read() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();
        assert_eq!(load_cached(tmp.path()).unwrap().starred_at, 0);

        update(tmp.path(), |config| {
            config.starred_at = 1234;
            Ok(())
        })
        .unwrap();

        assert_eq!(load_cached(tmp.path()).unwrap().starred_at, 1234);
    }

    /// The interleaving that the stamp check alone cannot see: a same-length
    /// write completes, with the same mtime, while an earlier reader is still
    /// parsing. Without the generation guard that reader would reinstate the
    /// pre-write parse under a stamp that still matches, and every later read
    /// would serve it.
    #[test]
    fn a_write_that_lands_during_a_read_is_not_memoized() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();
        let path = config_path(tmp.path());

        // What the in-flight reader saw before the write.
        let (mtime, len) = file_stamp(&path).unwrap();
        let stamp = stamp_of(&path);
        let (generation, _) = probe_cache(&path, mtime, len);
        let stale = Arc::new(workspace());

        // The writer completes: same length, and we force the same mtime.
        let mut updated = workspace();
        updated.title = "Tit1e".to_string();
        save(tmp.path(), &updated).unwrap();
        restore_stamp(&path, stamp);
        assert_eq!(file_stamp(&path).unwrap(), (mtime, len));

        // Now the slow reader tries to publish what it read.
        memoize_if_current(path, mtime, len, stale, generation);

        assert_eq!(load_cached(tmp.path()).unwrap().title, "Tit1e");
    }

    #[test]
    fn forget_drops_the_memo_for_a_root() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();
        assert_eq!(load_cached(tmp.path()).unwrap().title, "Title");

        forget(tmp.path());

        // Nothing observable changes for a live workspace -- the point is the
        // entry is gone, so a deleted root cannot leak its parse to whatever
        // is created at the same path next.
        assert!(lock_config_cache()
            .get(&config_path(tmp.path()))
            .is_none_or(|slot| slot.entry.is_none()));
        assert_eq!(load_cached(tmp.path()).unwrap().title, "Title");
    }

    #[test]
    fn a_deleted_config_is_never_served_from_the_memo() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &workspace()).unwrap();
        assert!(load_cached(tmp.path()).is_ok());

        fs::remove_file(config_path(tmp.path())).unwrap();

        assert!(load_cached(tmp.path()).is_err());
        assert!(load(tmp.path()).is_err());
    }

    /// A config that never parsed must not poison the memo: fixing the file
    /// has to be picked up without a restart.
    #[test]
    fn a_parse_failure_is_not_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let path = config_path(tmp.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();
        assert!(load_cached(tmp.path()).is_err());

        save(tmp.path(), &workspace()).unwrap();
        assert_eq!(load_cached(tmp.path()).unwrap().title, "Title");
    }
}

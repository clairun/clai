use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_provider_connection_id: Option<String>,
    pub default_agent_id: String,
    #[serde(default)]
    pub schedule: WorkspaceSchedule,
    #[serde(default)]
    pub agents: Vec<WorkspaceAgent>,
    /// MCP servers attached to the workspace conversation but toggled off in
    /// the context bar. The manager agent's `selected_mcp_servers` remains the
    /// *effective* (enabled) set that sessions and scheduled runs consume;
    /// this list only remembers the "attached but disabled" badges so the
    /// toggle survives app restarts. `workspace_update_session_mcp` keeps the
    /// two lists disjoint.
    #[serde(default)]
    pub disabled_mcp_servers: Vec<McpRef>,
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
            preferred_provider_connection_id: None,
            default_agent_id: manager_id.clone(),
            schedule: WorkspaceSchedule::default(),
            agents: vec![WorkspaceAgent::new_manager(manager_id, now)],
            disabled_mcp_servers: Vec::new(),
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

pub fn load(root: &Path) -> Result<WorkspaceConfig, WorkspaceConfigError> {
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

/// Drops MCP refs without a server id. Legacy configs referenced servers
/// by name; those refs are removed on load (the next save persists the
/// removal) and the user re-attaches the server from the UI.
fn prune_legacy_mcp_refs(config: &mut WorkspaceConfig) {
    config
        .disabled_mcp_servers
        .retain(|mcp_ref| !mcp_ref.id.is_empty());
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
        path,
        source,
    })?;
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
    ids.iter().map(|id| McpRef { id: id.clone() }).collect()
}

pub fn refs_to_mcp_ids(refs: &[McpRef]) -> Vec<String> {
    refs.iter().map(|mcp_ref| mcp_ref.id.clone()).collect()
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
        raw["disabledMcpServers"] = serde_json::json!([
            { "name": "legacy-by-name" },
            { "id": "srv-1" }
        ]);
        raw["agents"][0]["selectedMcpServers"] = serde_json::json!([
            { "name": "legacy-by-name" },
            { "id": "srv-2" }
        ]);
        fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

        let loaded = load(tmp.path()).unwrap();
        assert_eq!(
            loaded.disabled_mcp_servers,
            vec![McpRef {
                id: "srv-1".to_string()
            }]
        );
        assert_eq!(
            loaded.agents[0].selected_mcp_servers,
            vec![McpRef {
                id: "srv-2".to_string()
            }]
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
}

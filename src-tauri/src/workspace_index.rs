use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::config::workspace_config::ScheduleSurface;
use crate::config::{workspace_config, AppConfig, WorkspaceConfig};

#[derive(Debug, Clone)]
pub struct WorkspaceLocator {
    pub id: String,
    pub root_path: PathBuf,
    pub title: String,
    pub updated_at: i64,
    /// Mirrors `WorkspaceConfig::last_run_completed_at` / `last_opened_at`
    /// so `workspace_list` can derive the rail's "unread" flag without
    /// re-reading every config.json on each poll. Refreshed via
    /// `insert_config`.
    pub last_run_completed_at: i64,
    pub last_opened_at: i64,
    /// Mirrors `WorkspaceConfig::starred_at` (> 0 = starred) so the rail's
    /// "Starred" section can be derived without re-reading configs.
    pub starred_at: i64,
    pub default_agent_id: String,
    /// The workspace's schedule as the rail and Fleet surfaces read it, so
    /// those endpoints need no `config.json` access on a poll. Derived by
    /// [`workspace_config::WorkspaceSchedule::surface`], never field by field
    /// — deriving it here by hand is what made this locator disagree with the
    /// snapshot about `paused`. Refreshed via `insert_config`.
    pub schedule: ScheduleSurface,
}

#[derive(Debug, Clone)]
pub struct WorkspaceLoadFailure {
    pub path: PathBuf,
    pub reason: LoadFailureReason,
}

#[derive(Debug, Clone)]
pub enum LoadFailureReason {
    IdMismatch { expected: String, actual: String },
    DuplicateId { id: String },
    Unreadable(std::io::ErrorKind),
    InvalidConfig { line: usize, column: usize },
    DbCorrupt(String),
    MigrationFailed(String),
}

impl LoadFailureReason {
    fn from_config_error(error: &workspace_config::WorkspaceConfigError) -> Self {
        use workspace_config::WorkspaceConfigError;
        match error {
            WorkspaceConfigError::Io { source, .. } => Self::Unreadable(source.kind()),
            WorkspaceConfigError::Parse { source, .. }
            | WorkspaceConfigError::Serialize { source } => Self::InvalidConfig {
                line: source.line(),
                column: source.column(),
            },
        }
    }
}

impl LoadFailureReason {
    /// Whether the directory may hold a config the index could not use. A
    /// directory without `config.json` holds no title or star, and a database
    /// failure is recorded for a workspace whose config is already indexed.
    fn may_hide_config(&self) -> bool {
        match self {
            Self::Unreadable(kind) => *kind != std::io::ErrorKind::NotFound,
            Self::IdMismatch { .. } | Self::DuplicateId { .. } | Self::InvalidConfig { .. } => true,
            Self::DbCorrupt(_) | Self::MigrationFailed(_) => false,
        }
    }
}

/// Category only: serde and I/O messages can quote config contents.
impl std::fmt::Display for LoadFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdMismatch { .. } => f.write_str("config id does not match directory name"),
            Self::DuplicateId { .. } => f.write_str("duplicate workspace id"),
            Self::Unreadable(std::io::ErrorKind::NotFound) => f.write_str("missing config.json"),
            Self::Unreadable(kind) => write!(f, "unreadable config.json ({kind})"),
            Self::InvalidConfig { line, column } => {
                write!(f, "invalid config.json at line {line} column {column}")
            }
            Self::DbCorrupt(_) => f.write_str("workspace database could not be opened"),
            Self::MigrationFailed(_) => f.write_str("workspace database migration failed"),
        }
    }
}

#[derive(Default)]
pub struct WorkspaceIndex {
    pub by_id: HashMap<String, WorkspaceLocator>,
    pub sorted_by_updated: Vec<String>,
    pub load_failures: Vec<WorkspaceLoadFailure>,
    /// I/O errors that make name lookup incomplete.
    pub scan_errors: Vec<String>,
    pools: HashMap<String, SqlitePool>,
}

impl WorkspaceIndex {
    pub fn scan(config: &AppConfig) -> Self {
        let mut index = WorkspaceIndex::default();
        for workspace_dir in config.expanded_workspace_dirs() {
            let entries = match fs::read_dir(&workspace_dir) {
                Ok(entries) => entries,
                Err(error) => {
                    index.scan_errors.push(format!(
                        "Cannot scan workspace directory {}: {}",
                        workspace_dir.display(),
                        error
                    ));
                    continue;
                }
            };

            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        index.scan_errors.push(format!(
                            "Cannot read workspace directory entry in {}: {}",
                            workspace_dir.display(),
                            error
                        ));
                        continue;
                    }
                };
                let path = entry.path();
                let Some(dir_name) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                // Only UUID-named entries can be workspaces. Ignore unrelated
                // files and links before following their metadata.
                if dir_name.starts_with('.') || Uuid::parse_str(&dir_name).is_err() {
                    continue;
                }
                match fs::metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => {}
                    Ok(_) => continue,
                    Err(error) => {
                        index.scan_errors.push(format!(
                            "Cannot inspect workspace entry {}: {}",
                            path.display(),
                            error
                        ));
                        continue;
                    }
                }

                let workspace_config = match workspace_config::load(&path) {
                    Ok(config) => config,
                    Err(error) => {
                        index.load_failures.push(WorkspaceLoadFailure {
                            path,
                            reason: LoadFailureReason::from_config_error(&error),
                        });
                        continue;
                    }
                };

                if workspace_config.id != dir_name {
                    index.load_failures.push(WorkspaceLoadFailure {
                        path,
                        reason: LoadFailureReason::IdMismatch {
                            expected: dir_name,
                            actual: workspace_config.id,
                        },
                    });
                    continue;
                }

                if index.by_id.contains_key(&workspace_config.id) {
                    index.load_failures.push(WorkspaceLoadFailure {
                        path,
                        reason: LoadFailureReason::DuplicateId {
                            id: workspace_config.id,
                        },
                    });
                    continue;
                }

                index.insert_config(path, &workspace_config);
            }
        }
        for error in &index.scan_errors {
            tracing::warn!("{error}");
        }
        index.resort();
        index
    }

    pub fn insert_config(&mut self, root_path: PathBuf, config: &WorkspaceConfig) {
        self.insert_locator(WorkspaceLocator {
            id: config.id.clone(),
            root_path,
            title: config.title.clone(),
            updated_at: config.updated_at,
            last_run_completed_at: config.last_run_completed_at,
            last_opened_at: config.last_opened_at,
            starred_at: config.starred_at,
            default_agent_id: config.main_agent_id().to_string(),
            schedule: config.schedule.surface(),
        });
    }

    /// Insert an already-built locator, no `config.json` read involved.
    /// Used to undo a `remove_workspace` when the deletion it was part of
    /// failed and the workspace is still on disk.
    ///
    /// Deliberately not symmetric with `remove_workspace`: the sqlx pool that
    /// one dropped is not restored, because `AppState::workspace_db` re-opens
    /// a missing pool on demand.
    pub fn insert_locator(&mut self, locator: WorkspaceLocator) {
        self.by_id.insert(locator.id.clone(), locator);
        self.resort();
    }

    pub fn remove_workspace(&mut self, id: &str) -> Option<WorkspaceLocator> {
        self.pools.remove(id);
        let removed = self.by_id.remove(id);
        self.resort();
        removed
    }

    pub fn root(&self, id: &str) -> Option<PathBuf> {
        self.by_id.get(id).map(|locator| locator.root_path.clone())
    }

    pub fn locator(&self, id: &str) -> Option<WorkspaceLocator> {
        self.by_id.get(id).cloned()
    }

    pub fn locators_sorted(&self) -> Vec<WorkspaceLocator> {
        self.sorted_by_updated
            .iter()
            .filter_map(|id| self.by_id.get(id).cloned())
            .collect()
    }

    pub fn attach_pool(&mut self, id: String, pool: SqlitePool) {
        self.pools.insert(id, pool);
    }

    pub fn pool(&self, id: &str) -> Option<SqlitePool> {
        self.pools.get(id).cloned()
    }

    /// Load failures that may hide a workspace's title or star.
    pub fn failures_hiding_configs(&self) -> impl Iterator<Item = &WorkspaceLoadFailure> {
        self.load_failures
            .iter()
            .filter(|failure| failure.reason.may_hide_config())
    }

    pub fn record_failure(&mut self, path: PathBuf, reason: LoadFailureReason) {
        self.load_failures
            .push(WorkspaceLoadFailure { path, reason });
    }

    fn resort(&mut self) {
        let mut ids: Vec<_> = self.by_id.keys().cloned().collect();
        ids.sort_by_key(|id| {
            std::cmp::Reverse(
                self.by_id
                    .get(id)
                    .map(|loc| loc.updated_at)
                    .unwrap_or_default(),
            )
        });
        self.sorted_by_updated = ids;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::workspace_config::ScheduleKind;

    fn config(enabled: bool, paused: bool) -> WorkspaceConfig {
        let mut config = WorkspaceConfig::new("ws-1".into(), "WS".into(), 0, "mgr-1".into());
        config.schedule.enabled = enabled;
        config.schedule.paused = paused;
        config.schedule.kind = ScheduleKind::Interval {
            interval_minutes: 5,
        };
        config
    }

    #[test]
    fn locator_schedule_is_the_shared_surface_not_the_raw_config() {
        let mut index = WorkspaceIndex::default();

        // Rebuilding the surface field by field here is exactly the drift this
        // locator used to carry, so it is what this test exists to catch.
        index.insert_config(PathBuf::from("/tmp/ws-1"), &config(false, true));
        assert_eq!(
            index.locator("ws-1").unwrap().schedule,
            ScheduleSurface::default(),
            "a disabled schedule must not surface a stale on-disk pause"
        );

        index.insert_config(PathBuf::from("/tmp/ws-1"), &config(true, true));
        let schedule = index.locator("ws-1").unwrap().schedule;
        assert!(schedule.enabled && schedule.paused);
        assert_eq!(
            schedule.kind,
            Some(ScheduleKind::Interval {
                interval_minutes: 5
            })
        );
    }
}

#[cfg(test)]
mod cli_scan_tests {
    use super::*;

    #[test]
    fn missing_root_is_reported_as_incomplete_scan() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig {
            workspace_dirs: vec![temp.path().join("missing")],
            ..Default::default()
        };
        let index = WorkspaceIndex::scan(&config);
        assert_eq!(index.scan_errors.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn broken_non_workspace_link_does_not_block_starred_open() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let id = "aaaaaaaa-0000-0000-0000-000000000000";
        let mut workspace = WorkspaceConfig::new(id.into(), "Ready".into(), 1, "manager".into());
        workspace.starred_at = 1;
        workspace_config::save(&temp.path().join(id), &workspace).unwrap();
        symlink("missing", temp.path().join("notes")).unwrap();
        let config = AppConfig {
            workspace_dirs: vec![temp.path().to_path_buf()],
            ..Default::default()
        };

        let index = WorkspaceIndex::scan(&config);
        assert!(index.scan_errors.is_empty());
        assert_eq!(
            crate::cli::resolve(crate::cli::Command::Open(id.into()), &config, &index),
            Ok(crate::cli::LaunchRequest::Open(id.into()))
        );

        let broken_id = "bbbbbbbb-0000-0000-0000-000000000000";
        symlink("missing", temp.path().join(broken_id)).unwrap();
        let incomplete = WorkspaceIndex::scan(&config);
        assert_eq!(incomplete.scan_errors.len(), 1);
        assert_eq!(
            crate::cli::resolve(crate::cli::Command::Open(id.into()), &config, &incomplete)
                .unwrap_err()
                .code,
            1
        );
        assert_eq!(
            crate::cli::resolve(
                crate::cli::Command::Open(broken_id.into()),
                &config,
                &incomplete
            )
            .unwrap_err()
            .code,
            1
        );
    }

    #[test]
    fn load_failure_messages_never_quote_config_contents() {
        let temp = tempfile::tempdir().unwrap();
        let id = "aaaaaaaa-0000-0000-0000-000000000000";
        let root = temp.path().join(id);
        let json = "{\n  \"version\": \"SECRET_MARKER\"\n}";
        fs::create_dir_all(workspace_config::config_path(&root).parent().unwrap()).unwrap();
        fs::write(workspace_config::config_path(&root), json).unwrap();
        // The raw serde message does quote the value.
        assert!(workspace_config::load(&root)
            .unwrap_err()
            .to_string()
            .contains("SECRET_MARKER"));
        fs::create_dir(temp.path().join("bbbbbbbb-0000-0000-0000-000000000000")).unwrap();
        let config = AppConfig {
            workspace_dirs: vec![temp.path().to_path_buf()],
            ..Default::default()
        };
        let index = WorkspaceIndex::scan(&config);

        let open = |name: &str| {
            crate::cli::resolve(crate::cli::Command::Open(name.into()), &config, &index)
                .unwrap_err()
                .message
        };
        let exact = open(id);
        assert!(exact.contains(&root.display().to_string()), "{exact}");
        assert!(
            exact.contains("invalid config.json at line 2 column"),
            "{exact}"
        );
        assert!(!exact.contains("SECRET_MARKER"), "{exact}");
        let title = open("Some title");
        assert!(title.contains("invalid config.json"), "{title}");
        assert!(!title.contains("SECRET_MARKER"), "{title}");
        let orphan = open("bbbbbbbb-0000-0000-0000-000000000000");
        assert!(orphan.contains("missing config.json"), "{orphan}");
    }

    const READY: &str = "aaaaaaaa-0000-0000-0000-000000000000";
    const ORPHAN: &str = "bbbbbbbb-0000-0000-0000-000000000000";
    const CORRUPT: &str = "cccccccc-0000-0000-0000-000000000000";

    /// A starred workspace titled "Pisos" next to what a failed deletion
    /// leaves behind: a directory with files but no `config.json`.
    fn root_with_orphan() -> (tempfile::TempDir, AppConfig) {
        let temp = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new(READY.into(), "Pisos".into(), 1, "manager".into());
        workspace.starred_at = 1;
        workspace_config::save(&temp.path().join(READY), &workspace).unwrap();
        let leftover = temp.path().join(ORPHAN).join(".gocache");
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("README"), "cache").unwrap();
        let config = AppConfig {
            workspace_dirs: vec![temp.path().to_path_buf()],
            ..Default::default()
        };
        (temp, config)
    }

    fn open(
        config: &AppConfig,
        name: &str,
    ) -> Result<crate::cli::LaunchRequest, crate::cli::CliError> {
        let index = WorkspaceIndex::scan(config);
        crate::cli::resolve(crate::cli::Command::Open(name.into()), config, &index)
    }

    #[test]
    fn directory_without_config_does_not_block_title_or_prefix_lookup() {
        let (_temp, config) = root_with_orphan();
        assert_eq!(WorkspaceIndex::scan(&config).load_failures.len(), 1);
        assert_eq!(
            open(&config, "pisos"),
            Ok(crate::cli::LaunchRequest::Open(READY.into()))
        );
        assert_eq!(open(&config, &ORPHAN[..8]).unwrap_err().code, 2);
    }

    #[test]
    fn corrupt_config_still_blocks_title_lookup() {
        let (temp, config) = root_with_orphan();
        let corrupt = temp.path().join(CORRUPT);
        fs::create_dir_all(workspace_config::config_path(&corrupt).parent().unwrap()).unwrap();
        fs::write(workspace_config::config_path(&corrupt), "{").unwrap();

        let error = open(&config, "Pisos").unwrap_err();
        assert_eq!(error.code, 1);
        assert!(error.message.contains(CORRUPT), "{}", error.message);
        assert!(!error.message.contains(ORPHAN), "{}", error.message);
        assert_eq!(open(&config, &CORRUPT[..8]).unwrap_err().code, 1);
    }

    #[test]
    fn id_lookup_is_unchanged_by_directories_without_config() {
        let (_temp, config) = root_with_orphan();
        assert_eq!(
            open(&config, READY),
            Ok(crate::cli::LaunchRequest::Open(READY.into()))
        );
        let error = open(&config, ORPHAN).unwrap_err();
        assert_eq!(error.code, 1);
        assert!(
            error.message.contains("missing config.json"),
            "{}",
            error.message
        );
    }

    #[test]
    fn malformed_requested_uuid_is_recorded() {
        let temp = tempfile::tempdir().unwrap();
        let id = "aaaaaaaa-0000-0000-0000-000000000000";
        fs::create_dir(temp.path().join(id)).unwrap();
        let config = AppConfig {
            workspace_dirs: vec![temp.path().to_path_buf()],
            ..Default::default()
        };
        let index = WorkspaceIndex::scan(&config);
        assert!(index
            .load_failures
            .iter()
            .any(|failure| failure.path.file_name().unwrap() == id));
    }
}

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
    BadName,
    IdMismatch { expected: String, actual: String },
    DuplicateId { id: String },
    Unparseable(String),
    DbCorrupt(String),
    MigrationFailed(String),
}

#[derive(Default)]
pub struct WorkspaceIndex {
    pub by_id: HashMap<String, WorkspaceLocator>,
    pub sorted_by_updated: Vec<String>,
    pub load_failures: Vec<WorkspaceLoadFailure>,
    pools: HashMap<String, SqlitePool>,
}

impl WorkspaceIndex {
    pub fn scan(config: &AppConfig) -> Self {
        let mut index = WorkspaceIndex::default();
        for workspace_dir in config.expanded_workspace_dirs() {
            let entries = match fs::read_dir(&workspace_dir) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(
                        path = %workspace_dir.display(),
                        "Skipping unreadable workspace dir: {}",
                        error
                    );
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(dir_name) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                // Dot-prefixed siblings are the app's own bookkeeping, not
                // malformed workspaces (e.g. `.scratch`, the per-workspace
                // sandbox temp space). Recording them as load failures would be
                // noise the user can neither act on nor remove.
                if dir_name.starts_with('.') {
                    continue;
                }
                if Uuid::parse_str(&dir_name).is_err() {
                    index.load_failures.push(WorkspaceLoadFailure {
                        path,
                        reason: LoadFailureReason::BadName,
                    });
                    continue;
                }

                let workspace_config = match workspace_config::load(&path) {
                    Ok(config) => config,
                    Err(error) => {
                        index.load_failures.push(WorkspaceLoadFailure {
                            path,
                            reason: LoadFailureReason::Unparseable(error.to_string()),
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
            default_agent_id: config.default_agent_id.clone(),
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

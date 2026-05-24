use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::config::{workspace_config, AppConfig, WorkspaceConfig};

#[derive(Debug, Clone)]
pub struct WorkspaceLocator {
    pub id: String,
    pub root_path: PathBuf,
    pub title: String,
    pub updated_at: i64,
    pub default_agent_id: String,
    pub schedule_enabled: bool,
    pub schedule_paused: bool,
    pub interval_minutes: Option<u32>,
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
        self.by_id.insert(
            config.id.clone(),
            WorkspaceLocator {
                id: config.id.clone(),
                root_path,
                title: config.title.clone(),
                updated_at: config.updated_at,
                default_agent_id: config.default_agent_id.clone(),
                schedule_enabled: config.schedule.enabled,
                schedule_paused: config.schedule.paused,
                interval_minutes: if config.schedule.enabled {
                    Some(config.schedule.interval_minutes)
                } else {
                    None
                },
            },
        );
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
